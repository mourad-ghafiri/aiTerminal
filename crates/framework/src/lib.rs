//! `framework` — the Framework layer, one crate with internal modules. A light,
//! AI-first terminal: the window runtime, the plugin/theme/keymap constructs, and
//! the AI runtime the `@ai` / `@<agent>` shell integration drives.
//!
//! - `security` — the command guard + redactor over the in-house regex engine.
//! - `caps`     — the native-object standard library (the AI agents' tools).
//! - `plugin`   — the declarative plugin construct: manifests, registry, store.
//! - `config` / `theme` / `keymap` — configuration, theme resolution, keymaps.
//! - `ai`       — the streaming, provider-agnostic AI runtime (agents, flows,
//!   memory, MCP).
//! - `gui`      — the interactive window runtime (the multiplexer, terminal
//!   panes, tab switcher, status bar).
//! - `render`   — the headless renderers (one frame to PPM/PNG bytes or a file).
//! - `cli`      — the headless face of the subcommands (plugin/config/theme/ai):
//!   each returns printable `String`/`i32`; the binary just prints.
//!
//! No facade and no flat re-export: callers name the real module path
//! (`framework::ai::Client`, `framework::plugin::store`). Depends on `platform`
//! and `corelib` directly.
#![forbid(unsafe_code)]

pub mod ai;
pub mod caps;
pub mod cli;
pub mod config;
pub mod gui;
pub mod i18n;
pub mod keymap;
pub mod plugin;
pub mod procio;
pub mod profile;
pub mod render;
pub mod security;
pub mod shell;
pub mod theme;

#[cfg(test)]
mod test_home;
