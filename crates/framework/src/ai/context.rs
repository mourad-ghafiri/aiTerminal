//! Capture compact terminal context to ground the assistant. The caller supplies
//! the visible rows it already holds (a `term::Term` snapshot in the GUI, or
//! nothing for the CLI), so this crate reads no PTY and stays testable against
//! `MockPlatform`.
//!
//! Secret redaction is NOT done here — it is the host's single responsibility,
//! applied via the `framework::security` policy (fed by the `redactor` plugin)
//! before this context, and any tool result, leaves for a model.

/// What the caller knows about the focused terminal right now.
pub struct TermContext<'a> {
    pub cwd: Option<&'a str>,
    pub shell: &'a str,
    /// Visible/recent terminal lines, oldest first.
    pub recent_lines: &'a [String],
}

/// Format the last `max_lines` of context as a fenced block. Returns an empty
/// string when there is nothing useful to share. The host redacts the result.
pub fn capture_context(c: &TermContext, max_lines: usize) -> String {
    let start = c.recent_lines.len().saturating_sub(max_lines);
    let recent = &c.recent_lines[start..];
    if c.cwd.is_none() && c.shell.is_empty() && recent.iter().all(|l| l.trim().is_empty()) {
        return String::new();
    }
    let mut s = String::from("Recent terminal context (secrets redacted):\n```\n");
    // NOTE: the literal header is informational; the host applies the redaction.
    if let Some(cwd) = c.cwd {
        s.push_str("# cwd: ");
        s.push_str(cwd);
        s.push('\n');
    }
    if !c.shell.is_empty() {
        s.push_str("# shell: ");
        s.push_str(c.shell);
        s.push('\n');
    }
    for line in recent {
        s.push_str(line);
        s.push('\n');
    }
    s.push_str("```");
    s
}

#[cfg(test)]
mod tests {
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
}
