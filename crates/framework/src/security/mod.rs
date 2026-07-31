//! `guard` — the terminal's security policy, built on the `re` regex engine.
//!
//! Two capabilities, both pure data (config + declarative plugins feed them; no
//! code runs here):
//!   * **Command guard** — allow/deny regex lists. Default: everything allowed,
//!     nothing denied. A command is permitted iff it is not denied AND (the
//!     allow-list is empty OR it matches the allow-list). **Deny always wins.**
//!   * **Redaction** — replace literal/regex matches with a placeholder, scoped
//!     to terminal output / AI egress / browser display.
//!
//! Patterns are added as data; an invalid regex is reported as a warning (the
//! rule is skipped) rather than panicking, so a bad config never breaks startup.
#![forbid(unsafe_code)]

// The in-house regex engine the guard is built on lives in this crate. `pub(crate)` so
// other from-scratch features (e.g. the agent's `fs.search` grep) reuse the same engine.
pub(crate) mod regex;

use crate::security::regex::Regex;

/// The default placeholder (matches the AI crate's secret-redaction placeholder).
pub const PLACEHOLDER: &str = "\u{ab}redacted\u{bb}"; // «redacted»

/// Where a redaction rule applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedactScope {
    Terminal,
    Ai,
    All,
}

impl RedactScope {
    /// Parse a scope token; unknown / empty → `All`.
    pub fn parse(s: &str) -> RedactScope {
        match s.trim().to_ascii_lowercase().as_str() {
            "terminal" | "term" => RedactScope::Terminal,
            "ai" => RedactScope::Ai,
            _ => RedactScope::All,
        }
    }
}

/// The result of a command check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Allowed only after the user confirms (human-in-the-loop).
    Confirm { reason: String },
    Deny { reason: String },
}

#[derive(Clone)]
enum Matcher {
    Literal(String),
    Re(Regex),
}

#[derive(Clone)]
struct RedactionRule {
    matcher: Matcher,
    replacement: String,
    scope: RedactScope,
}

/// A compiled security policy.
#[derive(Clone, Default)]
pub struct Policy {
    allow: Vec<Regex>,
    deny: Vec<Regex>,
    confirm: Vec<Regex>,
    /// The **auto-pilot safe-list**: commands a regex here matches are the ONLY ones the AI
    /// agent auto-runs in Auto mode (everything else prompts). Orthogonal to the hard guard
    /// (`check_command`) — `deny`/`confirm` still win; `safe` only relaxes the Auto prompt.
    safe: Vec<Regex>,
    redactions: Vec<RedactionRule>,
}

impl Policy {
    pub fn new() -> Policy {
        Policy::default()
    }

    /// Add a command allow-list pattern (regex). Returns the pattern on a
    /// compile error so the caller can warn.
    pub fn add_allow(&mut self, pattern: &str) -> Result<(), String> {
        self.allow.push(compile(non_empty(pattern)?)?);
        Ok(())
    }
    /// Add a command deny-list pattern (regex).
    pub fn add_deny(&mut self, pattern: &str) -> Result<(), String> {
        self.deny.push(compile(non_empty(pattern)?)?);
        Ok(())
    }
    /// Add a confirm-before-run pattern (regex) — matched commands prompt the user.
    pub fn add_confirm(&mut self, pattern: &str) -> Result<(), String> {
        self.confirm.push(compile(non_empty(pattern)?)?);
        Ok(())
    }
    /// Add an **auto-safe** command pattern (regex). Auto mode auto-runs a shell command only
    /// when one of these matches (and it isn't denied/confirmed); anything else prompts.
    pub fn add_safe(&mut self, pattern: &str) -> Result<(), String> {
        self.safe.push(compile(non_empty(pattern)?)?);
        Ok(())
    }
    /// Add a redaction rule. `literal` true → exact-substring; false → regex.
    pub fn add_redaction(
        &mut self,
        pattern: &str,
        replacement: &str,
        scope: RedactScope,
        literal: bool,
    ) -> Result<(), String> {
        if pattern.is_empty() {
            return Err("empty redaction pattern".to_string());
        }
        let matcher = if literal {
            Matcher::Literal(pattern.to_string())
        } else {
            Matcher::Re(compile(pattern)?)
        };
        self.redactions.push(RedactionRule { matcher, replacement: replacement.to_string(), scope });
        Ok(())
    }

    /// Fold another policy into this one (config first, then plugins). Allow/deny/
    /// redactions concatenate — a plugin can only ADD denials/redactions or WIDEN
    /// the allow-list, never remove a user's restriction (deny still wins).
    pub fn merge(&mut self, other: Policy) {
        self.allow.extend(other.allow);
        self.deny.extend(other.deny);
        self.confirm.extend(other.confirm);
        self.safe.extend(other.safe);
        self.redactions.extend(other.redactions);
    }

    pub fn has_command_rules(&self) -> bool {
        !self.allow.is_empty() || !self.deny.is_empty() || !self.confirm.is_empty()
    }
    pub fn has_redactions(&self) -> bool {
        !self.redactions.is_empty()
    }
    /// Are there any redaction rules that apply to `scope` (used to skip work)?
    pub fn has_scope(&self, scope: RedactScope) -> bool {
        self.redactions.iter().any(|r| r.scope == scope || r.scope == RedactScope::All)
    }

    /// Check whether `cmd` may run. Precedence: **deny > confirm > allow-list**.
    pub fn check_command(&self, cmd: &str) -> Verdict {
        let c = cmd.trim();
        if c.is_empty() {
            return Verdict::Allow;
        }
        // A pasted / AI-suggested command may span MULTIPLE lines, all of which run. `^`/`$`
        // anchor to the whole string, so a benign first line would otherwise shield a
        // `sudo rm -rf /` on line two from a `^sudo`-anchored deny rule. Evaluate each
        // non-empty line independently; the precedence stays deny > confirm > allow-list.
        let lines: Vec<&str> = c.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        for line in &lines {
            if let Some(r) = self.deny.iter().find(|r| r.is_match(line)) {
                return Verdict::Deny { reason: format!("matches a deny rule  /{}/", r.as_str()) };
            }
        }
        for line in &lines {
            if let Some(r) = self.confirm.iter().find(|r| r.is_match(line)) {
                return Verdict::Confirm { reason: format!("matches a confirm rule  /{}/", r.as_str()) };
            }
        }
        // Allow-list mode: EVERY line must be allow-listed, else the whole command is denied.
        if !self.allow.is_empty() && lines.iter().any(|line| !self.allow.iter().any(|r| r.is_match(line))) {
            return Verdict::Deny { reason: "not in the allow-list".to_string() };
        }
        Verdict::Allow
    }

    pub fn is_allowed(&self, cmd: &str) -> bool {
        matches!(self.check_command(cmd), Verdict::Allow)
    }

    /// Is `cmd` on the **auto-pilot safe-list** — a read-only / inspection command the AI
    /// agent may auto-run in Auto mode without a prompt? Pure read of the `safe` rules; the
    /// hard guard (`check_command`) is consulted separately and still wins. An empty
    /// safe-list means *nothing* auto-qualifies (Auto then prompts for every command).
    pub fn is_safe_command(&self, cmd: &str) -> bool {
        // Auto-run requires EVERY line to be safe — a multi-line command is only as safe as
        // its least-safe line (same anti-shielding reasoning as `check_command`).
        let lines: Vec<&str> = cmd.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        !lines.is_empty() && lines.iter().all(|line| self.safe.iter().any(|r| r.is_match(line)))
    }

    /// Apply every redaction rule whose scope matches `scope` (or is `All`).
    pub fn redact(&self, text: &str, scope: RedactScope) -> String {
        if self.redactions.is_empty() {
            return text.to_string();
        }
        // Rules that don't touch the text must not reallocate it — this runs per
        // PTY chunk and per AI-bound string, usually over perfectly clean text.
        let mut s: Option<String> = None;
        for r in self.redactions.iter().filter(|r| r.scope == scope || r.scope == RedactScope::All) {
            let cur = s.as_deref().unwrap_or(text);
            match &r.matcher {
                Matcher::Literal(lit) => {
                    if cur.contains(lit.as_str()) {
                        s = Some(cur.replace(lit.as_str(), &r.replacement));
                    }
                }
                Matcher::Re(re) => {
                    if let Some(next) = re.replace_all_opt(cur, &r.replacement) {
                        s = Some(next);
                    }
                }
            }
        }
        s.unwrap_or_else(|| text.to_string())
    }
}

fn compile(pattern: &str) -> Result<Regex, String> {
    Regex::new(pattern).map_err(|e| format!("invalid pattern `{pattern}`: {e}"))
}

/// Reject an empty command pattern: an empty regex matches at every position, so an
/// empty deny/confirm rule would silently block or prompt on EVERY command (and an
/// empty allow/safe rule would allow everything). A templating slip (`denied = [""]`)
/// must fail loudly, exactly as [`add_redaction`] already guards its patterns.
fn non_empty(pattern: &str) -> Result<&str, String> {
    if pattern.is_empty() {
        return Err("empty command pattern matches everything — rejected".to_string());
    }
    Ok(pattern)
}

#[cfg(test)]
mod tests;

/// Compile the security policy from config + enabled-plugin contributions —
/// **UI-free** (shared by the window, the CLI, and agent runs). Bad patterns are
/// reported to stderr and skipped (never break startup). Plugins can only ADD
/// restrictions/safety data; deny wins.
pub fn build_policy(config: &crate::config::Config, registry: &crate::plugin::PluginRegistry) -> Policy {
    let mut p = Policy::new();
    let warn = |r: Result<(), String>| {
        if let Err(e) = r {
            eprintln!("aiTerminal: security rule skipped — {e}");
        }
    };
    for pat in &config.allowed_commands {
        warn(p.add_allow(pat));
    }
    for pat in &config.denied_commands {
        warn(p.add_deny(pat));
    }
    for pat in &config.confirm_commands {
        warn(p.add_confirm(pat));
    }
    for pat in &config.auto_safe_commands {
        warn(p.add_safe(pat));
    }
    for r in &config.redactions {
        warn(p.add_redaction(&r.pattern, &r.replacement, RedactScope::parse(&r.scope), r.literal));
    }
    for a in registry.allow_commands() {
        warn(p.add_allow(&a.pattern));
    }
    for d in registry.deny_commands() {
        warn(p.add_deny(&d.pattern));
    }
    for cf in registry.confirm_commands() {
        warn(p.add_confirm(&cf.pattern));
    }
    for sf in registry.safe_commands() {
        warn(p.add_safe(&sf.pattern));
    }
    for r in registry.redact_rules() {
        warn(p.add_redaction(&r.pattern, &r.replacement, RedactScope::parse(&r.scope), r.literal));
    }
    p
}
