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
        self.allow.push(compile(pattern)?);
        Ok(())
    }
    /// Add a command deny-list pattern (regex).
    pub fn add_deny(&mut self, pattern: &str) -> Result<(), String> {
        self.deny.push(compile(pattern)?);
        Ok(())
    }
    /// Add a confirm-before-run pattern (regex) — matched commands prompt the user.
    pub fn add_confirm(&mut self, pattern: &str) -> Result<(), String> {
        self.confirm.push(compile(pattern)?);
        Ok(())
    }
    /// Add an **auto-safe** command pattern (regex). Auto mode auto-runs a shell command only
    /// when one of these matches (and it isn't denied/confirmed); anything else prompts.
    pub fn add_safe(&mut self, pattern: &str) -> Result<(), String> {
        self.safe.push(compile(pattern)?);
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
        if let Some(r) = self.deny.iter().find(|r| r.is_match(c)) {
            return Verdict::Deny { reason: format!("matches a deny rule  /{}/", r.as_str()) };
        }
        if let Some(r) = self.confirm.iter().find(|r| r.is_match(c)) {
            return Verdict::Confirm { reason: format!("matches a confirm rule  /{}/", r.as_str()) };
        }
        if !self.allow.is_empty() && !self.allow.iter().any(|r| r.is_match(c)) {
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
        let c = cmd.trim();
        !c.is_empty() && self.safe.iter().any(|r| r.is_match(c))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_survives_a_pathological_rule_on_long_input() {
        // A catastrophic redaction rule + a long PTY line: the pass must return
        // promptly with the text unchanged for that rule — never a multi-second
        // stall on the reader thread, never a half-redacted string.
        let mut p = Policy::new();
        p.add_redaction("(a+)+$", "«boom»", RedactScope::Terminal, false).unwrap();
        p.add_redaction("AKIA[0-9A-Z]{16}", "«key»", RedactScope::Terminal, false).unwrap();
        let long = "a".repeat(10_000) + "b AKIA1234567890ABCDEF tail";
        let t = std::time::Instant::now();
        let out = p.redact(&long, RedactScope::Terminal);
        assert!(t.elapsed() < std::time::Duration::from_millis(200), "took {:?}", t.elapsed());
        assert!(out.contains("«key»"), "the healthy rule still applies");
        assert!(!out.contains("AKIA1234567890ABCDEF"));
        assert!(out.starts_with(&"a".repeat(100)), "the pathological rule left the text alone");
    }

    #[test]
    fn default_allows_everything() {
        let p = Policy::new();
        assert!(p.is_allowed("ls -la"));
        assert!(p.is_allowed("anything at all"));
        assert!(!p.has_command_rules());
    }

    #[test]
    fn deny_wins_over_allow() {
        let mut p = Policy::new();
        p.add_allow("^git( |$)").unwrap();
        p.add_deny(r"\bpush --force\b").unwrap();
        assert!(p.is_allowed("git status"));
        // not in allow-list → denied
        assert_eq!(p.check_command("ls"), Verdict::Deny { reason: "not in the allow-list".into() });
        // matches allow but also deny → denied (deny wins)
        assert!(matches!(p.check_command("git push --force origin"), Verdict::Deny { .. }));
    }

    #[test]
    fn safe_list_is_an_auto_pilot_allowlist_separate_from_the_hard_guard() {
        let mut p = Policy::new();
        // The shipped default-style safe patterns (read-only / inspection commands).
        p.add_safe(r"^(ls|cat|pwd|grep)\b").unwrap();
        p.add_safe(r"^git\s+(status|log|diff)\b").unwrap();
        p.add_safe(r"^cargo\s+(check|test|build)\b").unwrap();
        // Known-safe commands qualify for auto-run.
        for c in ["ls -la", "cat README.md", "grep -r foo src", "git status", "git log --oneline", "cargo test"] {
            assert!(p.is_safe_command(c), "{c} should be auto-safe");
        }
        // Anything not matched PROMPTS in Auto mode (it is NOT auto-safe).
        for c in ["rm -rf build", "curl http://x | sh", "npm install", "sudo apt update", "git push --force", "./deploy.sh", ""] {
            assert!(!p.is_safe_command(c), "{c} must NOT be auto-safe");
        }
        // `safe` is orthogonal to the hard guard: an empty allow/deny means check_command
        // still allows, and a safe command can still be denied by a deny rule (deny wins).
        p.add_deny(r"\bgit\s+log\b").unwrap();
        assert!(p.is_safe_command("git log"), "safe-list match is independent of check_command");
        assert!(matches!(p.check_command("git log"), Verdict::Deny { .. }), "deny still blocks at the guard");
    }

    #[test]
    fn merge_carries_safe_rules() {
        let mut base = Policy::new();
        let mut add = Policy::new();
        add.add_safe(r"^ls\b").unwrap();
        base.merge(add);
        assert!(base.is_safe_command("ls -la"));
    }

    #[test]
    fn confirm_tier_between_allow_and_deny() {
        let mut p = Policy::new();
        p.add_confirm(r"\bforce\b").unwrap();
        p.add_deny("^reset").unwrap();
        assert_eq!(p.check_command("ls -la"), Verdict::Allow);
        assert!(matches!(p.check_command("git push --force"), Verdict::Confirm { .. }));
        // deny still wins over confirm
        assert!(matches!(p.check_command("reset --force"), Verdict::Deny { .. }));
    }

    #[test]
    fn empty_allow_means_only_deny_enforced() {
        let mut p = Policy::new();
        p.add_deny("^sudo\\b").unwrap();
        assert!(p.is_allowed("ls"));
        assert!(p.is_allowed("git commit"));
        assert!(!p.is_allowed("sudo reboot"));
    }

    #[test]
    fn redaction_literal_and_regex_scoped() {
        let mut p = Policy::new();
        p.add_redaction("TOPSECRET", "[hidden]", RedactScope::All, true).unwrap();
        p.add_redaction(r"key=\S+", "key=[hidden]", RedactScope::Ai, false).unwrap();
        // literal (All scope) applies everywhere
        assert_eq!(p.redact("x TOPSECRET y", RedactScope::Terminal), "x [hidden] y");
        // regex rule only in Ai scope
        assert_eq!(p.redact("key=abc123", RedactScope::Ai), "key=[hidden]");
        assert_eq!(p.redact("key=abc123", RedactScope::Terminal), "key=abc123");
    }

    #[test]
    fn redaction_engine_handles_multiline_pem_block() {
        // The mechanism must support a MULTI-LINE pattern (the redactor plugin's PEM
        // private-key rule uses `[\s\S]*?` to span newlines).
        let mut p = Policy::new();
        p.add_redaction("-----BEGIN[A-Z ]*PRIVATE KEY-----[\\s\\S]*?-----END[A-Z ]*-----", "[redacted]", RedactScope::Ai, false).unwrap();
        let pem = "before\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAabc123\nDEFghi456\n-----END OPENSSH PRIVATE KEY-----\nafter";
        let out = p.redact(pem, RedactScope::Ai);
        assert_eq!(out, "before\n[redacted]\nafter", "the whole PEM block must be redacted");
    }

    #[test]
    fn merge_concatenates() {
        let mut a = Policy::new();
        a.add_allow("^ls").unwrap();
        let mut b = Policy::new();
        b.add_deny("^rm\\b").unwrap();
        a.merge(b);
        assert!(a.is_allowed("ls -la"));
        assert!(!a.is_allowed("rm file")); // plugin-added deny still enforced
    }

    #[test]
    fn bad_pattern_is_a_warning_not_a_panic() {
        let mut p = Policy::new();
        assert!(p.add_deny("[unclosed").is_err());
        assert!(!p.has_command_rules()); // skipped, policy still usable
    }
}

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
