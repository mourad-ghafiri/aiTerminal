//! Plain configuration/data structs that cross the platform seam (the OS-seam
//! traits that consume them live in the Platform layer's `platform-api`).

use crate::types::geom::Size;

/// How a window should be created.
#[derive(Clone, Debug)]
pub struct WindowConfig {
    pub title: String,
    /// Initial logical size in points.
    pub logical_size: Size,
    pub min_logical_size: Size,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: crate::brand::NAME.into(),
            logical_size: Size::new(1024.0, 680.0),
            min_logical_size: Size::new(360.0, 240.0),
        }
    }
}

/// What to launch in a PTY.
#[derive(Clone, Debug)]
pub struct PtyCommand {
    /// Program to exec; empty resolves to the user's shell (config → `$SHELL` →
    /// the password-db shell → `/bin/zsh`/`bash`/`sh`).
    pub program: String,
    pub args: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    /// Launch as an **interactive login shell** (argv[0] prefixed with `-`, cwd at
    /// `$HOME`). The backend always exports `TERM`/`COLORTERM`/`TERM_PROGRAM` so a
    /// a desktop (GUI) launch (no inherited shell env) still gets a correct session.
    pub login: bool,
    /// Host-supplied environment overrides applied on top of the inherited env (and
    /// the backend's `TERM`/`COLORTERM`). A higher layer uses this for shell
    /// integration: `ZDOTDIR`, `LS_COLORS`/`LSCOLORS`/`CLICOLOR`, etc. A key here
    /// overrides any inherited or backend-owned value.
    pub env: Vec<(String, String)>,
    /// Working directory to start the shell in. `None` keeps the default (a login shell
    /// starts at `$HOME`); `Some(path)` overrides it — used to restore a saved workspace's
    /// terminal pane in the folder it was last in.
    pub cwd: Option<String>,
}

impl Default for PtyCommand {
    fn default() -> Self {
        Self {
            program: String::new(),
            args: Vec::new(),
            cols: 80,
            rows: 24,
            login: false,
            env: Vec::new(),
            cwd: None,
        }
    }
}



