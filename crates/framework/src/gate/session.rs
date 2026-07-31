//! The shared shell: a PTY, and a mirror of everything it printed.
//!
//! The gate spawns the user's own shell — same integration, same aliases, same prompt
//! — and sits between it and the pane. That is what makes the session genuinely
//! *shared*: the chat and the keyboard drive one shell, with one cwd and one history.
//!
//! The keystone invariant is [`Sink`]: **every byte written to the pane is also fed to
//! the mirror terminal, and nothing else writes to the pane.** Break it and `/shot`
//! quietly starts lying. Holding it in one place also makes the driver testable — a
//! test swaps in a sink that records instead of printing, and no tty is involved.

use std::io::Write;
use std::sync::Arc;

use corelib::types::PtyCommand;
use platform::term::Term;
use platform::traits::Pty;

use crate::config::Config;

/// Where relayed bytes go. One method, so the invariant has exactly one enforcement
/// point.
pub trait Sink {
    fn emit(&mut self, bytes: &[u8]);
}

/// The real pane: standard output, mirrored into `term`.
pub struct PaneSink {
    pub term: Term,
}

impl Sink for PaneSink {
    fn emit(&mut self, bytes: &[u8]) {
        let mut out = std::io::stdout();
        let _ = out.write_all(bytes);
        let _ = out.flush();
        // A malformed byte run is a parser edge case, not a reason to lose the pane:
        // the bytes are already on screen, so a mirror hiccup only affects `/shot`.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.term.feed(bytes)));
    }
}

/// A sink that keeps what it was given — the test double for the whole driver.
#[derive(Default)]
pub struct RecordingSink {
    pub written: Vec<u8>,
    pub term: Option<Term>,
}

impl RecordingSink {
    pub fn with_mirror(cols: u16, rows: u16) -> Self {
        RecordingSink { written: Vec::new(), term: Some(Term::new(cols, rows)) }
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.written).into_owned()
    }
}

impl Sink for RecordingSink {
    fn emit(&mut self, bytes: &[u8]) {
        self.written.extend_from_slice(bytes);
        if let Some(t) = &mut self.term {
            t.feed(bytes);
        }
    }
}

/// The environment variable that switches the shell integration's gate marks on. It
/// is set only for a gated shell, so ordinary panes are untouched.
pub const MARK_ENV: &str = "TT_GATE";

/// The spawned shell.
pub struct GateSession {
    pty: Arc<dyn Pty>,
    pub cols: u16,
    pub rows: u16,
    pub shell_name: String,
}

impl GateSession {
    /// Spawn the user's shell with the same integration a normal pane gets, plus the
    /// gate's command marks.
    pub fn spawn(config: &Config, registry: &crate::plugin::PluginRegistry, cols: u16, rows: u16) -> std::io::Result<GateSession> {
        let theme = Config::resolve_theme(&config.theme);
        let shell = if config.shell.trim().is_empty() {
            std::env::var("SHELL").unwrap_or_default()
        } else {
            config.shell.clone()
        };
        let integ = crate::shell::prepare(config, registry, &theme, &shell);
        let mut env = integ.env;
        // Ask the shell integration to emit OSC 1339 command marks (see `marks`).
        env.push((MARK_ENV.to_string(), "1".to_string()));

        let cmd = PtyCommand {
            program: shell.clone(),
            args: integ.args,
            cols,
            rows,
            login: integ.login,
            env,
            // Start where the user already is, not at $HOME: they ran `@gate` in a
            // project and expect the shared shell to be in it.
            cwd: std::env::current_dir().ok().map(|p| p.display().to_string()),
        };
        let pty: Arc<dyn Pty> = Arc::from(platform::os::spawn_pty(&cmd)?);
        let shell_name = shell.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("shell").to_string();
        Ok(GateSession { pty, cols, rows, shell_name })
    }

    pub fn pty(&self) -> Arc<dyn Pty> {
        self.pty.clone()
    }

    /// Send input to the shell.
    pub fn write(&self, bytes: &[u8]) {
        let _ = self.pty.write(bytes);
    }

    /// Submit a command line.
    pub fn submit(&self, cmd: &str) {
        self.write(cmd.as_bytes());
        self.write(b"\r");
    }

    pub fn resize_to(&self, cols: u16, rows: u16) {
        let _ = self.pty.resize(cols, rows);
    }
}

#[cfg(test)]
mod tests;
