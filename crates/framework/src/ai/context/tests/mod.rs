use super::*;

#[test]
fn empty_context_is_empty() {
    let c = TermContext { cwd: None, shell: "", recent_lines: &[] };
    assert!(capture_context(&c, 40).is_empty());
}

#[test]
fn captures_last_lines() {
    let lines: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
    let c = TermContext { cwd: Some("/work"), shell: "zsh", recent_lines: &lines };
    let out = capture_context(&c, 5);
    assert!(out.contains("# cwd: /work"));
    assert!(out.contains("line 49"));
    assert!(!out.contains("line 10"), "only the last 5 lines");
}
// Redaction is the host's responsibility (framework::security policy fed by
// the `redactor` plugin); see the app's single-source-redaction golden test.
