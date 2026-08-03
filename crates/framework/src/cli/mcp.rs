//! `ai mcp` — the declared MCP servers, actually connected.
//!
//! Visibility is the spec's own ask ("make clear which tools are being exposed to
//! the model"), and it is also the only way to debug a declaration: this command
//! launches every declared server exactly as a run would, then reports what each
//! one is — transport, negotiated era, self-reported identity, tool count — or why
//! it is not running, with the tail of its stderr, which is where a dying server
//! writes its reason.

use crate::cli::style::{accent, muted, reset};

pub(crate) fn ai_mcp_cmd() -> i32 {
    crate::config::Config::ensure_default();
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    let dir = crate::config::Config::mcp_dir();
    let servers = crate::ai::load_servers(&[dir.clone()]);
    let (dim, r) = (muted(), reset());
    if servers.is_empty() {
        println!("no MCP servers declared.");
        println!("{dim}declare one in {}/<name>.toml \u{2014} `command = \u{2026}` spawns a local server,", dir.display());
        println!("`url = \u{2026}` reaches a remote one; tools appear to agents as mcp.<name>.<tool>{r}");
        return 0;
    }
    let hub = crate::ai::McpHub::launch(&servers);
    for s in hub.report() {
        let mut facts: Vec<String> = Vec::new();
        if !s.era.is_empty() {
            facts.push(s.era.clone());
        }
        if !s.info.is_empty() {
            facts.push(s.info.clone());
        }
        if s.error.is_empty() {
            facts.push(format!("{} tool{}", s.tools, if s.tools == 1 { "" } else { "s" }));
            if s.resources {
                facts.push("resources".into());
            }
        }
        println!("  {}{:<14}{r} {dim}{} \u{b7} {}{r}", accent(), s.name, s.reach, facts.join(" \u{b7} "));
        if !s.error.is_empty() {
            println!("      \u{2717} {}", s.error);
            for line in s.stderr.iter().rev().take(5).rev() {
                println!("      {dim}stderr \u{2502} {line}{r}");
            }
        }
    }
    0
}
