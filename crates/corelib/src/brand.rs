//! The product brand — the ONE place the product name lives, so a rename is a
//! single edit. Everything downstream derives from [`NAME`]: the per-user data
//! directory (`~/.<NAME>` via `Config::dir()`), the window title, `TERM_PROGRAM`,
//! the MCP client id, the shell-integration header, and the CLI usage text.
//!
//! It lives in `corelib` (the base layer that depends on nothing), so every higher
//! crate names it directly as `corelib::brand::NAME` — no facade, no duplication.

/// The product/brand name. Change this one line to rename the product everywhere
/// that derives from it (the data dir, window title, env vars, MCP id, …).
pub const NAME: &str = "aiTerminal";

/// The global AI instructions file (`~/.<NAME>/ai/aiTerminal.md`) — the
/// system-prompt base every `@ai` / agent / flow / loop run is grounded on.
pub const INSTRUCTIONS_FILE: &str = "aiTerminal.md";
