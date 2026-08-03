//! Secrets: what leaves as a placeholder, and what comes back as itself.
//!
//! A secret you `cat` is *yours* — it is already on your screen, your disk, your
//! environment. The boundary that matters is **egress**: the moment text is about to leave
//! for a model, a tool, or a chat app. Two things happen there, and they are not the same
//! thing:
//!
//! * **[`Secrets::hide`]** — reversible. The value is swapped for a placeholder and
//!   remembered, so when the text comes back to this machine — as a command to run, as a
//!   tool's arguments — [`Vault::restore`] puts the real value back and the work actually
//!   works. This is what lets an agent use a database password it was never shown.
//! * **[`Secrets::mask`]** — irreversible. For the screen. You cannot un-mask a screen, and
//!   somebody who scoped a rule to `terminal` meant "I do not want to look at this".
//!
//! The vault lives in memory, for one run, and is never written down. A vault on disk
//! would be a secret store, and this product does not have one — which is also why text
//! crossing a process boundary is masked rather than hidden: the reading process has a
//! different vault and could not put anything back.

use std::sync::{Arc, Mutex};

use super::regex::Regex;
use super::rules::Scope;

/// What a masked value reads as on screen.
pub const MASK: &str = "\u{ab}redacted\u{bb}"; // «redacted»

/// How many distinct secrets one run may hold. A hostile tool result full of
/// key-shaped strings costs a bounded amount of memory and then stops minting.
const MAX_SECRETS: usize = 512;

/// The longest value worth vaulting. A "secret" longer than this is a document that
/// happened to match, and carrying it would be carrying the document.
const MAX_VALUE: usize = 8 * 1024;

#[derive(Clone)]
enum Matcher {
    Literal(String),
    Re(Regex),
}

#[derive(Clone)]
struct Rule {
    matcher: Matcher,
    /// Names the placeholder, never the value.
    name: String,
    scope: Scope,
}

/// The secret rules, compiled.
#[derive(Clone, Default)]
pub(crate) struct Secrets {
    rules: Vec<Rule>,
}

impl Secrets {
    pub(crate) fn add(&mut self, pattern: &str, name: &str, scope: Scope, literal: bool) -> Result<(), String> {
        let matcher = match literal {
            true => Matcher::Literal(pattern.to_string()),
            false => Matcher::Re(Regex::new(pattern).map_err(|e| format!("invalid pattern `{pattern}`: {e}"))?),
        };
        self.rules.push(Rule { matcher, name: tidy_name(name), scope });
        Ok(())
    }

    /// Whether any rule reaches the screen (so the terminal can skip the whole pass).
    pub(crate) fn masks_display(&self) -> bool {
        self.rules.iter().any(|r| r.scope.masks())
    }

    /// The names of the rules that reach a model, for the briefing.
    pub(crate) fn hidden_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = Vec::new();
        for r in self.rules.iter().filter(|r| r.scope.hides()) {
            if !names.contains(&r.name.as_str()) {
                names.push(&r.name);
            }
        }
        names
    }

    /// Swap every egress-scope match for its placeholder.
    pub(crate) fn hide(&self, text: &str, vault: &Vault) -> String {
        self.apply(text, Scope::hides, &mut |rule, value| vault.token_for(&rule.name, value))
    }

    /// Swap every display-scope match for [`MASK`], with no way back.
    pub(crate) fn mask(&self, text: &str) -> String {
        self.apply(text, Scope::masks, &mut |_, _| MASK.to_string())
    }

    /// Swap EVERY match for [`MASK`], whatever its scope and with no way back.
    pub(crate) fn scrub(&self, text: &str) -> String {
        self.apply(text, |_| true, &mut |_, _| MASK.to_string())
    }

    /// Run the rules whose scope `wants`, each over the previous one's output.
    ///
    /// Rules **compose** on purpose: a key caught by an `sk-` rule can be caught again by a
    /// `KEY=value` rule, which takes the key's *name* with it. Over-redacting is the safe
    /// direction. Text that no rule touches is never reallocated — this runs on every PTY
    /// chunk and every string bound for a model, usually over perfectly clean text.
    fn apply(&self, text: &str, wants: fn(Scope) -> bool, with: &mut dyn FnMut(&Rule, &str) -> String) -> String {
        if self.rules.is_empty() {
            return text.to_string();
        }
        let mut s: Option<String> = None;
        for rule in self.rules.iter().filter(|r| wants(r.scope)) {
            let cur = s.as_deref().unwrap_or(text);
            match &rule.matcher {
                Matcher::Literal(lit) => {
                    if !lit.is_empty() && cur.contains(lit.as_str()) {
                        let token = with(rule, lit);
                        s = Some(cur.replace(lit.as_str(), &token));
                    }
                }
                Matcher::Re(re) => {
                    if let Some(next) = re.replace_all_with(cur, &mut |m| with(rule, m)) {
                        s = Some(next);
                    }
                }
            }
        }
        s.unwrap_or_else(|| text.to_string())
    }
}

/// One secret this run is holding.
struct Held {
    value: String,
    /// The rule that named it — kept so minting the next one of the same kind is a count
    /// rather than a scan that rebuilds every token's prefix to compare against.
    name: String,
    token: String,
}

/// One run's secrets and the placeholder each is known by.
///
/// Shared by every clone of a [`Guard`](super::Guard) — a flow runs four nodes through one
/// guard, and a secret seen by one of them has to read back the same in the next.
#[derive(Clone, Default)]
pub struct Vault {
    entries: Arc<Mutex<Vec<Held>>>, // first-seen order
}

/// How many times [`Vault::restore`] will walk its entries before giving up.
///
/// One pass is not enough, because placeholders **nest**: the shipped rules compose, so
/// `API_KEY=sk-…` is hidden by the key rule and then hidden AGAIN by the `KEY=value` rule,
/// which takes the key's name with it. Restoring the outer one puts the inner one back into
/// the text, and a single pass would leave it there — which is to say the most ordinary
/// `.env` line there is could never be restored at all. Real nesting is two or three deep;
/// the bound is what stops a pathological policy from looping.
const RESTORE_PASSES: usize = 8;

impl Vault {
    /// The placeholder for `value` — the same one every time, so the model sees a stable
    /// identifier it can carry from a file it read into a command it writes.
    fn token_for(&self, name: &str, value: &str) -> String {
        if value.len() > MAX_VALUE {
            return MASK.to_string();
        }
        let mut entries = match self.entries.lock() {
            Ok(e) => e,
            // A poisoned lock means another thread panicked mid-mint. Masking is the safe
            // answer: unreadable beats leaked.
            Err(_) => return MASK.to_string(),
        };
        if let Some(held) = entries.iter().find(|h| h.value == value) {
            return held.token.clone();
        }
        if entries.len() >= MAX_SECRETS {
            // Said once, at the boundary. Past here every further secret is masked rather
            // than vaulted, so a run starts failing to restore things — and a run that
            // fails for a reason nobody was told is the worst kind.
            if entries.len() == MAX_SECRETS {
                platform::warn!("the guard is holding {MAX_SECRETS} secrets for this run — further ones are masked, not restorable");
            }
            return MASK.to_string();
        }
        let n = entries.iter().filter(|h| h.name == name).count() + 1;
        let token = format!("\u{ab}{name}-{n}\u{bb}");
        entries.push(Held { value: value.to_string(), name: name.to_string(), token: token.clone() });
        token
    }

    /// Put the real values back, and say which placeholders were left over.
    ///
    /// Runs to a fixed point (see [`RESTORE_PASSES`]) because placeholders nest. Only
    /// entries whose token is actually present cost anything: `fs.write` restores whole
    /// file contents, and five hundred blind `replace` passes over a megabyte is half a
    /// gigabyte of copying to change nothing.
    pub(super) fn put_back(&self, text: &str) -> String {
        if !text.contains('\u{ab}') {
            return text.to_string();
        }
        let Ok(entries) = self.entries.lock() else { return text.to_string() };
        let mut out = text.to_string();
        for _ in 0..RESTORE_PASSES {
            let mut changed = false;
            for held in entries.iter() {
                if out.contains(held.token.as_str()) {
                    out = out.replace(held.token.as_str(), &held.value);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        out
    }

    /// How many secrets this run is holding.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.lock().map(|e| e.len()).unwrap_or(0)
    }
}

/// Every placeholder one of `names` could have minted, replaced by [`MASK`].
///
/// The other half of scrubbing. Masking rewrites a *value*; this rewrites the stand-in for
/// one — which is what text crossing out of a run is usually carrying by then. Both have to
/// go, because the reader on the other side has neither the value nor a vault that knows
/// the token, and a record holding a placeholder nothing can resolve is a trap laid for a
/// later run.
pub(super) fn strip(text: &str, names: &[&str]) -> String {
    let mut out = text.to_string();
    while let Some(token) = unresolved(&out, names) {
        out = out.replace(&token, MASK);
    }
    out
}

/// The first placeholder left in `text` that one of `names` could have minted, if any.
///
/// Scoped to the guard's OWN rule names on purpose. `«page-12»` in a filename has exactly
/// the shape of a placeholder and is not one, and refusing a perfectly good command over a
/// pair of guillemets is a worse bug than the one this check exists to catch.
pub(super) fn unresolved(text: &str, names: &[&str]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '\u{ab}' {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 1;
        while j < chars.len() && chars[j] != '\u{bb}' && chars[j] != '\u{ab}' {
            j += 1;
        }
        if j < chars.len() && chars[j] == '\u{bb}' {
            let inner: String = chars[start + 1..j].iter().collect();
            if minted_by(&inner, names) {
                return Some(chars[start..=j].iter().collect());
            }
            i = j + 1;
            continue;
        }
        i = start + 1;
    }
    None
}

/// `<one of ours>-12` — a rule's name, a dash, digits, and nothing else.
fn minted_by(inner: &str, names: &[&str]) -> bool {
    let Some((name, n)) = inner.rsplit_once('-') else { return false };
    !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && names.contains(&name)
}

/// A rule's name reduced to what a placeholder may carry: `[a-z0-9_-]`, or `secret` when
/// nothing survives.
///
/// Sanitised rather than rejected, because the name is decoration on a rule that is
/// otherwise fine — but a token nobody can recognise defeats [`unresolved`], so `AWS Key`
/// becomes `aws-key` here rather than reaching the vault as it was written.
fn tidy_name(raw: &str) -> String {
    let name: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' })
        .collect();
    let name = name.trim_matches('-').to_string();
    match name.is_empty() {
        true => "secret".to_string(),
        false => name,
    }
}
