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

fn masking_guard() -> crate::guard::Guard {
    crate::guard::Guard::from_toml("[[guard.secret]]\npattern = \"AKIA[0-9A-Z]{6}\"\nscope = \"terminal\"\n")
}

#[test]
fn redact_terminal_masks_plain_text() {
    let p = masking_guard();
    assert_eq!(redact_terminal("token AKIA123ABC done", &p), format!("token {} done", crate::guard::MASK));
}

#[test]
fn redact_terminal_preserves_ansi_escapes() {
    let p = masking_guard();
    // SGR colour + an OSC title around the secret — escape bytes must survive
    // untouched while only the printable run is masked.
    let input = "\u{1b}[31mAKIA123ABC\u{1b}[0m\u{1b}]0;AKIA123ABC\u{07}tail";
    let out = redact_terminal(input, &p);
    assert_eq!(out, format!("\u{1b}[31m{}\u{1b}[0m\u{1b}]0;AKIA123ABC\u{07}tail", crate::guard::MASK));
    // The CSI and OSC control sequences are byte-identical to the input.
    assert!(out.contains("\u{1b}[31m") && out.contains("\u{1b}[0m"));
    assert!(out.contains("\u{1b}]0;AKIA123ABC\u{07}"));
}

#[test]
fn redact_terminal_noop_without_rules() {
    let p = crate::guard::Guard::default();
    let s = "\u{1b}[1mhello\u{1b}[0m world";
    assert_eq!(redact_terminal(s, &p), s);
}

#[test]
fn a_config_carries_all_three_tiers_into_the_guard() {
    use crate::guard::{Act, Decision};
    let mut config = Config::default();
    config.guard = crate::guard::RuleSet::parse(
        corelib::wire::Toml::parse(
            "[[guard.command]]\npattern = \"^tidy\\\\b\"\nrule = \"deny\"\n\
             [[guard.command]]\npattern = \"\\\\bforce\\\\b\"\nrule = \"confirm\"\n\
             [[guard.command]]\npattern = \"^(git|tidy)\"\nrule = \"allow\"\n",
        )
        .unwrap()
        .get("guard")
        .unwrap(),
    );
    let p = crate::guard::build(&config, &crate::plugin::PluginRegistry::new());
    assert!(matches!(p.judge(Act::Run("git status")), Decision::Allow));
    assert!(matches!(p.judge(Act::Run("git push --force")), Decision::Confirm { .. }));
    assert!(matches!(p.judge(Act::Run("tidy up")), Decision::Deny { .. }));
}

// The `ai-guard` PLUGIN supplies the defaults: they are registry DATA the user installs
// (builtin/plugins/), loaded here from the repo rather than embedded. These golden tests
// fail if a default rule's regex is silently dropped — `build` skips a pattern that will
// not compile — so they double as a compile check on the shipped policy. Every string
// below is an INERT literal.
fn shipped_guard() -> crate::guard::Guard {
    let mut reg = crate::plugin::PluginRegistry::new();
    let p = format!("{}/../../builtin/plugins/ai-guard/plugin.toml", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p}: {e}"));
    reg.add_trusted(crate::plugin::Manifest::parse(&text).unwrap());
    crate::guard::build(&Config::default(), &reg)
}

#[test]
fn the_shipped_guard_refuses_what_it_promises_to() {
    use crate::guard::{Act, Decision};
    let p = shipped_guard();
    assert!(matches!(p.judge(Act::Run("mkfs.ext4 /dev/disk2")), Decision::Deny { .. }), "reformatting denied");
    assert!(matches!(p.judge(Act::Run(":(){ :|:& };:")), Decision::Deny { .. }), "fork bomb denied");
    assert!(matches!(p.judge(Act::Run("sudo apt install x")), Decision::Confirm { .. }), "sudo confirmed");
    assert!(matches!(p.judge(Act::Run("git push --force origin")), Decision::Confirm { .. }), "force-push confirmed");
    // ordinary commands stay allowed
    assert!(matches!(p.judge(Act::Run("ls -la")), Decision::Allow));
    assert!(matches!(p.judge(Act::Run("git status")), Decision::Allow));
}

#[test]
fn the_shipped_guard_is_the_single_source_of_secret_rules() {
    let p = shipped_guard();
    // Each "secret" is an INERT literal that the redactor plugin's rules must scrub.
    // They keep the SHAPE a rule matches — that is what is under test — but carry no
    // entropy, so they are recognisably placeholders rather than anything that reads
    // like a credential. Nothing that could be mistaken for a real key belongs in a
    // repository, least of all in the tests for the thing that hides keys.
    assert!(!p.hide("key sk-ant-example-only-not-a-key").contains("example-only-not-a-key"));
    assert!(!p.hide("AKIAEXAMPLEONLY00000 here").contains("AKIAEXAMPLEONLY00000"));
    assert!(!p.hide("Authorization: Bearer eyJabc.def.ghi").contains("eyJabc.def.ghi"));
    assert!(!p.hide("API_KEY=supersecretvalue123").contains("supersecretvalue123"));
    // ordinary text is untouched
    assert_eq!(p.hide("cargo build --release"), "cargo build --release");
    // …and every one of them comes back, which is what makes the run still work.
    let hidden = p.hide("AWS_ACCESS_KEY_ID=AKIAEXAMPLEONLY00000");
    assert_eq!(p.restore(&hidden).unwrap(), "AWS_ACCESS_KEY_ID=AKIAEXAMPLEONLY00000");
}
