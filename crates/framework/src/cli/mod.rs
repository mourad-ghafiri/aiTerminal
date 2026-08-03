//! The headless face of the subcommands — `plugin`, `config`, `theme`, `profile`,
//! `ai`. Each takes the raw subcommand argv tail and returns a process exit code;
//! the binary just dispatches the leading word here and propagates the code.
//!
//! ALL AI runs through here — the terminal window never talks to a model. The
//! shell plugin's `@ai` / `@<agent>` / `@flow` handlers call `aiTerminal ai …`,
//! stream to stdout (into the terminal), and background workflows are tracked as
//! job records under `~/.aiTerminal/ai/jobs/` (`aiTerminal ai jobs`).
//!
//! The `ai` subcommand stays offline-capable: it never reads keys off the machine —
//! the API key comes only from the configured env var (or an explicit `[ai] api_key`).

pub(crate) mod agentloop;
pub(crate) mod agents;
pub(crate) mod attach;
pub(crate) mod config;
pub(crate) mod flow;
pub(crate) mod format;
pub(crate) mod jobs;
pub(crate) mod live;
pub(crate) mod logs;
pub(crate) mod mcp;
pub(crate) mod md;
pub(crate) mod media;
pub(crate) mod observe;
pub(crate) mod plugin;
pub(crate) mod profile;
pub(crate) mod run;
pub(crate) mod runner;
pub(crate) mod style;
pub(crate) mod theme;
pub(crate) mod trace;

#[cfg(test)]
mod tests;

// ── the surface, kept where it always was ───────────────────────────────────
//
// Splitting this module into files changed where each item LIVES, not what a
// caller writes: `crate::cli::ai` is still `crate::cli::ai`. Every name below is
// named from outside `cli/`, and this block is the whole list of them.

/// The subcommands `crates/app` dispatches to.
pub use crate::cli::{agents::install_command_defaults, config::config, md::md, plugin::plugin, profile::profile, run::ai, theme::theme};

/// Terminal styling and inline media, shared with the Markdown editor and the flow board.
pub(crate) use crate::cli::{live::erase_seq, media::{diagram_lines, diagram_rows, image_placement, is_native_terminal}};
pub(crate) use crate::cli::style::{accent, bold, danger, md_style, muted, reset, success, term_cols, term_rows, warn};

/// Named only by the scenario worlds, which are themselves `#[cfg(test)]`.
#[cfg(test)]
pub(crate) use crate::cli::{
    agentloop::{drive_loop_for_test, scripted_verdict, LoopState},
    jobs::{args::{parse_job_args, JobCmd}, create::{resolve_spec, Resolved}, shell::guard_refusal},
    run::{command_marker, error_comment, ANSWER_MARK},
};

/// `aiTerminal gate <channel> start|stop` / `status` — the `@gate` remote-control
/// gateway. The whole implementation lives in [`crate::gate`]; this is only the
/// argv seam, matching how every other subcommand is wired.
pub fn gate(args: &[String]) -> i32 {
    crate::gate::run(args)
}
