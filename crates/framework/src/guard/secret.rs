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
        let name = match name.trim() {
            "" => "secret".to_string(),
            n => n.to_string(),
        };
        self.rules.push(Rule { matcher, name, scope });
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

/// One run's secrets and the placeholder each is known by.
///
/// Shared by every clone of a [`Guard`](super::Guard) — a flow runs four nodes through one
/// guard, and a secret seen by one of them has to read back the same in the next.
#[derive(Clone, Default)]
pub struct Vault {
    entries: Arc<Mutex<Vec<(String, String)>>>, // (value, token) in first-seen order
}

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
        if let Some((_, token)) = entries.iter().find(|(v, _)| v == value) {
            return token.clone();
        }
        if entries.len() >= MAX_SECRETS {
            return MASK.to_string();
        }
        let n = entries.iter().filter(|(_, t)| t.starts_with(&format!("\u{ab}{name}-"))).count() + 1;
        let token = format!("\u{ab}{name}-{n}\u{bb}");
        entries.push((value.to_string(), token.clone()));
        token
    }

    /// Put the real values back, for text that is about to touch this machine.
    ///
    /// A placeholder this vault did not mint is an **error**, not something to pass along:
    /// it came from another run's record — a node transcript, a job log — and the value
    /// behind it is not here. Running the command anyway would send the literal text
    /// `«db-password-1»` to a database, and the failure would be a puzzle.
    pub fn restore(&self, text: &str) -> Result<String, String> {
        if !text.contains('\u{ab}') {
            return Ok(text.to_string());
        }
        let out = match self.entries.lock() {
            Ok(entries) => entries.iter().fold(text.to_string(), |acc, (value, token)| acc.replace(token.as_str(), value)),
            Err(_) => text.to_string(),
        };
        match unresolved(&out) {
            Some(token) => Err(format!(
                "{token} is a secret placeholder from another run — nothing here knows what it stood for, so re-run the step that read it"
            )),
            None => Ok(out),
        }
    }

    /// How many secrets this run is holding — what a run reports having restored.
    pub fn len(&self) -> usize {
        self.entries.lock().map(|e| e.len()).unwrap_or(0)
    }
}

/// The first `«name-N»`-shaped placeholder left in `text`, if any.
///
/// Deliberately narrow: `«redacted»` carries no `-N`, and neither does ordinary prose that
/// happens to use guillemets, so quoting a French sentence is not mistaken for a secret.
fn unresolved(text: &str) -> Option<String> {
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
            if looks_minted(&inner) {
                return Some(chars[start..=j].iter().collect());
            }
            i = j + 1;
            continue;
        }
        i = start + 1;
    }
    None
}

/// `name-12` — lowercase name, a dash, digits, and nothing else.
fn looks_minted(inner: &str) -> bool {
    let Some((name, n)) = inner.rsplit_once('-') else { return false };
    !name.is_empty()
        && !n.is_empty()
        && n.chars().all(|c| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}
