use super::*;

#[test]
fn crash_log_rotates_at_its_cap_instead_of_growing_forever() {
    let dir = std::env::temp_dir().join(format!("tt-crashlog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("crash.log");
    std::fs::write(&log, "x".repeat(2 * 1024 * 1024)).unwrap();
    append_crash_line(&log, "[panic] boom\n");
    assert!(std::fs::metadata(&log).unwrap().len() < 1024, "fresh file after rotation");
    assert!(log.with_extension("log.1").exists(), "the old log is kept aside");
    // Under the cap → plain append, no rotation.
    append_crash_line(&log, "[panic] again\n");
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("boom") && text.contains("again"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dirty_flag_coalesces_wakes_to_the_clean_to_dirty_edge() {
    use std::sync::atomic::AtomicUsize;
    let wakes = Arc::new(AtomicUsize::new(0));
    let flag = {
        let wakes = wakes.clone();
        DirtyFlag::with_waker(Arc::new(move || {
            wakes.fetch_add(1, SeqCst);
        }))
    };
    // The flag starts dirty (first frame always renders) — a flooding producer
    // must not wake the loop again until the frame is consumed.
    flag.set();
    flag.set();
    assert_eq!(wakes.load(SeqCst), 0, "already dirty → no wake");
    assert!(flag.take(), "the initial dirty state renders");
    assert!(!flag.take(), "consumed");
    flag.set();
    flag.set();
    flag.set();
    assert_eq!(wakes.load(SeqCst), 1, "one wake per clean→dirty edge, not per set");
    assert!(flag.take());
    flag.set();
    assert_eq!(wakes.load(SeqCst), 2, "a fresh edge wakes again");
}

#[test]
fn folder_label_is_the_basename() {
    assert_eq!(folder_label("/Users/me/testclaude").as_deref(), Some("testclaude"));
    assert_eq!(folder_label("/Users/me/My Project").as_deref(), Some("My Project"));
    assert_eq!(folder_label("/a/b/proj/").as_deref(), Some("proj"), "a trailing slash is ignored");
    assert_eq!(folder_label("~/مجلد").as_deref(), Some("مجلد"), "non-ASCII basename");
    assert_eq!(folder_label("~").as_deref(), Some("~"), "home stays ~");
    assert_eq!(folder_label("/").as_deref(), Some("/"), "root stays /");
    assert_eq!(folder_label(""), None, "empty path → no label");
    assert_eq!(folder_label("  "), None, "blank path → no label");
}

fn redacting_policy() -> crate::security::Policy {
    let mut p = crate::security::Policy::new();
    p.add_redaction("AKIA[0-9A-Z]{6}", "«key»", crate::security::RedactScope::Terminal, false).unwrap();
    p
}

#[test]
fn redact_terminal_masks_plain_text() {
    let p = redacting_policy();
    assert_eq!(redact_terminal("token AKIA123ABC done", &p), "token «key» done");
}

#[test]
fn redact_terminal_preserves_ansi_escapes() {
    let p = redacting_policy();
    // SGR colour + an OSC title around the secret — escape bytes must survive
    // untouched while only the printable run is masked.
    let input = "\u{1b}[31mAKIA123ABC\u{1b}[0m\u{1b}]0;AKIA123ABC\u{07}tail";
    let out = redact_terminal(input, &p);
    assert_eq!(out, "\u{1b}[31m«key»\u{1b}[0m\u{1b}]0;AKIA123ABC\u{07}tail");
    // The CSI and OSC control sequences are byte-identical to the input.
    assert!(out.contains("\u{1b}[31m") && out.contains("\u{1b}[0m"));
    assert!(out.contains("\u{1b}]0;AKIA123ABC\u{07}"));
}

#[test]
fn redact_terminal_noop_without_rules() {
    let p = crate::security::Policy::new();
    let s = "\u{1b}[1mhello\u{1b}[0m world";
    assert_eq!(redact_terminal(s, &p), s);
}

#[test]
fn build_policy_threads_confirm_tier() {
    let mut config = Config::default();
    config.denied_commands = vec!["^rm\\b".to_string()];
    config.confirm_commands = vec!["\\bforce\\b".to_string()];
    config.allowed_commands = vec!["^git".to_string()];
    let registry = crate::plugin::PluginRegistry::new();
    let p = crate::security::build_policy(&config, &registry);
    assert!(matches!(p.check_command("git status"), crate::security::Verdict::Allow));
    assert!(matches!(p.check_command("git push --force"), crate::security::Verdict::Confirm { .. }));
    assert!(matches!(p.check_command("rm file"), crate::security::Verdict::Deny { .. }));
}

// The default command-guard + redactor PLUGINS supply the policy (the guard
// crate is gone). The rules are registry DATA (builtin/plugins/) the user
// installs — loaded here from the repo, not embedded. These golden tests fail
// if a default rule's regex is silently dropped (build_policy skips a bad
// pattern), so they double as a compile check. All strings are INERT literals.
fn default_policy() -> crate::security::Policy {
    let mut reg = crate::plugin::PluginRegistry::new();
    for name in ["command-guard", "redactor"] {
        let p = format!("{}/../../builtin/plugins/{name}/plugin.toml", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p}: {e}"));
        reg.add_trusted(crate::plugin::Manifest::parse(&text).unwrap());
    }
    crate::security::build_policy(&Config::default(), &reg)
}

#[test]
fn command_guard_plugin_enforces_default_deny_and_confirm() {
    use crate::security::Verdict;
    let p = default_policy();
    assert!(matches!(p.check_command("rm -rf /"), Verdict::Deny { .. }), "catastrophic rm denied");
    assert!(matches!(p.check_command(":(){ :|:& };:"), Verdict::Deny { .. }), "fork bomb denied");
    assert!(matches!(p.check_command("sudo apt install x"), Verdict::Confirm { .. }), "sudo confirmed");
    assert!(matches!(p.check_command("git push --force origin"), Verdict::Confirm { .. }), "force-push confirmed");
    // ordinary commands stay allowed
    assert!(matches!(p.check_command("ls -la"), Verdict::Allow));
    assert!(matches!(p.check_command("git status"), Verdict::Allow));
}

#[test]
fn redactor_plugin_is_the_single_redaction_source() {
    use crate::security::RedactScope::Ai;
    let p = default_policy();
    // Each secret is an INERT literal; the redactor plugin's rules must scrub it.
    assert!(!p.redact("key sk-ant-api03-AbCd1234EfGh5678IjKl", Ai).contains("AbCd1234EfGh5678IjKl"));
    assert!(!p.redact("AKIA1234567890ABCDEF here", Ai).contains("AKIA1234567890ABCDEF"));
    assert!(!p.redact("Authorization: Bearer eyJabc.def.ghi", Ai).contains("eyJabc.def.ghi"));
    assert!(!p.redact("API_KEY=supersecretvalue123", Ai).contains("supersecretvalue123"));
    // ordinary text is untouched
    assert_eq!(p.redact("cargo build --release", Ai), "cargo build --release");
}
