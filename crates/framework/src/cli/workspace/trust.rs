//! The trust gate: nothing a project supplies loads before the person says so.
//!
//! A repo's `.aiTerminal/` can declare MCP servers — code that runs as you — and a
//! config that reshapes a session. So the FIRST open of a folder asks, showing
//! exactly what the project would inject (counted, never loaded); the answer is
//! remembered in the folder's session dir together with a fingerprint of the parts
//! that execute. A later open asks again only when that fingerprint moved — the
//! `git pull` that quietly added an MCP server is precisely the change that must
//! not ride in on an old yes.

use std::path::Path;

use crate::config::overlay::Workspace;

/// The remembered decision for a folder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Trust {
    Granted,
    Declined,
}

/// What `establish` needs to ask a human — a seam, so the gate is testable and the
/// REPL can route the question through its own input.
pub(crate) type Ask<'a> = &'a mut dyn FnMut(&str) -> bool;

/// The gate. Reads/writes `trust.toml` in `session_dir`; asks through `ask` when the
/// stored answer cannot stand.
pub(crate) fn establish(root: &Path, session_dir: &Path, ask: Ask) -> Trust {
    let offering = Workspace::offering(root);
    let fingerprint = Workspace::fingerprint(root);
    let stamp = session_dir.join("trust.toml");
    if let Some((granted, stored)) = read_stamp(&stamp) {
        // The decision stands while what-executes is unchanged. A declined folder is
        // not re-nagged either — `/trust` re-opens the question deliberately.
        if stored == fingerprint || !granted {
            return if granted { Trust::Granted } else { Trust::Declined };
        }
    }
    let granted = ask(&prompt_text(root, &offering));
    write_stamp(&stamp, granted, &fingerprint);
    if granted {
        Trust::Granted
    } else {
        Trust::Declined
    }
}

/// Forget the stored decision, so the next `establish` asks again (`/trust`).
pub(crate) fn reset(session_dir: &Path) {
    let _ = std::fs::remove_file(session_dir.join("trust.toml"));
}

/// The question, with the inventory that makes it answerable.
fn prompt_text(root: &Path, offering: &crate::config::overlay::Offering) -> String {
    let mut lines = vec![format!("open {} in workspace mode?", root.display())];
    let mut parts = Vec::new();
    for (n, what) in [
        (offering.agents, "agent(s)"),
        (offering.skills, "skill(s)"),
        (offering.prompts, "prompt(s)"),
        (offering.flows, "flow(s)"),
        (offering.mcp, "MCP server(s) \u{2014} these run code as you"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    if offering.config {
        parts.push("a config.toml (settings override; guard rules can only tighten)".into());
    }
    if let Some(name) = offering.instructions {
        parts.push(format!("instructions ({name})"));
    }
    match parts.is_empty() {
        true => lines.push("  this folder declares no .aiTerminal/ config of its own".into()),
        false => lines.push(format!("  this project would add: {}", parts.join(" \u{b7} "))),
    }
    lines.join("\n")
}

fn read_stamp(stamp: &Path) -> Option<(bool, String)> {
    let doc = corelib::wire::Toml::parse(&std::fs::read_to_string(stamp).ok()?).ok()?;
    Some((
        doc.get("granted").and_then(|v| v.as_bool())?,
        doc.get("fingerprint").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
    ))
}

fn write_stamp(stamp: &Path, granted: bool, fingerprint: &str) {
    if let Some(parent) = stamp.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(stamp, format!("granted = {granted}\nfingerprint = \"{fingerprint}\"\n"));
}
