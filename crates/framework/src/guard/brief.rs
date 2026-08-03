//! What the model is told about the guard, before it starts.
//!
//! A model that learns the rules by being refused spends turns discovering them, retries
//! what it cannot do, and ends its run at a step limit that says nothing about why. Told
//! up front, it works around what it cannot have and stops honestly when it cannot.
//!
//! Two things it must know, and one it must do:
//!
//! * what is refused, so it does not aim there;
//! * that a placeholder is a real value it may use but not see — without this the vault
//!   cannot work at all, because the model would report the secret as missing;
//! * and, on a refusal, either find another way or stop and say what it needed.
//!
//! Plain prose, no vendor dialect, and capped: a policy with two hundred rules must not
//! spend the window describing itself.

use super::rules::{CommandRule, PathRule};
use super::Guard;

/// How many patterns of one tier are named before the rest are counted.
const SHOWN: usize = 8;

pub(crate) fn briefing(g: &Guard) -> String {
    let mut lines: Vec<String> = Vec::new();
    let cmd = |rule, label| tier(&g.commands.patterns(rule), label);
    lines.extend(cmd(CommandRule::Deny, "Commands refused outright"));
    lines.extend(cmd(CommandRule::Confirm, "Commands a person must approve (so they are refused in an unattended run)"));
    lines.extend(cmd(CommandRule::Allow, "The ONLY commands that may run"));
    let path = |rule, label| tier(&g.paths.patterns(rule), label);
    lines.extend(path(PathRule::Deny, "Paths that may be neither read nor changed"));
    lines.extend(path(PathRule::ReadOnly, "Paths that may be read but never changed"));
    lines.extend(path(PathRule::Allow, "The ONLY paths that may be touched"));

    let names = g.secrets.hidden_names();
    if lines.is_empty() && names.is_empty() {
        return String::new();
    }

    let mut s = String::from("## This machine's guard\n");
    if !lines.is_empty() {
        s.push_str(
            "Some things here are refused, and a refusal comes back as that tool's result. When one \
             happens, do NOT retry it: do the task another way if there is one, and if there is not, \
             stop and say plainly what you needed and that the guard refused it. Never report a \
             refused step as done.\n\n",
        );
        for line in lines {
            s.push_str(&line);
            s.push('\n');
        }
        s.push('\n');
    }
    if !names.is_empty() {
        s.push_str(&format!(
            "Secrets on this machine are hidden from you. A value written {} is a PLACEHOLDER \
             standing for a real one ({}). Use it exactly as you found it — in a command, in a tool \
             argument, anywhere the real value would go — and this machine puts the value back \
             before anything runs. Never expand, rewrite, unquote or guess at what is behind one, \
             never ask for it, and never report it as missing or empty: it is neither.\n",
            "\u{ab}name-1\u{bb}",
            names.join(", ")
        ));
    }
    s
}

/// One tier as a line, capped — `label: /a/, /b/ (and 4 more)`.
fn tier(patterns: &[&str], label: &str) -> Option<String> {
    if patterns.is_empty() {
        return None;
    }
    let shown: Vec<String> = patterns.iter().take(SHOWN).map(|p| format!("/{p}/")).collect();
    let more = patterns.len().saturating_sub(SHOWN);
    Some(match more {
        0 => format!("- {label}: {}", shown.join(", ")),
        n => format!("- {label}: {} (and {n} more)", shown.join(", ")),
    })
}
