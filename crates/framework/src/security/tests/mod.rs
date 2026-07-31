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

#[test]
fn empty_command_pattern_is_rejected() {
    // `denied_commands = [""]` would deny EVERY command (empty regex matches anywhere).
    // All four command lists must reject an empty pattern like redaction already does.
    let mut p = Policy::new();
    assert!(p.add_deny("").is_err());
    assert!(p.add_allow("").is_err());
    assert!(p.add_confirm("").is_err());
    assert!(p.add_safe("").is_err());
    assert!(!p.has_command_rules(), "no rule was actually added");
    assert!(p.is_allowed("rm -rf /"), "an empty deny didn't secretly block everything");
}

#[test]
fn multiline_command_cannot_shield_a_denied_line() {
    // The bypass: `^sudo` anchors to the whole string, so a benign first line used to
    // hide `sudo …` on line two. Every line is now checked independently.
    let mut p = Policy::new();
    p.add_deny("^sudo\\b").unwrap();
    assert!(matches!(p.check_command("sudo rm -rf /"), Verdict::Deny { .. }));
    assert!(matches!(p.check_command("echo hi\nsudo rm -rf /"), Verdict::Deny { .. }), "the hidden sudo line is caught");
    assert!(matches!(p.check_command("echo hi\necho bye"), Verdict::Allow));
    // Auto-safe likewise requires every line to be safe.
    let mut s = Policy::new();
    s.add_safe("^echo\\b").unwrap();
    assert!(s.is_safe_command("echo hi"));
    assert!(!s.is_safe_command("echo hi\nsudo rm -rf /"), "one unsafe line makes the whole command unsafe");
}
