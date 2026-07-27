//! The gate's step vocabulary — what a user, a shell, and a program can do.
//!
//! A closed set, so this is an enum rather than a registry of trait objects: the parser
//! rejects an unknown verb outright (a typo in a scenario must fail loudly, never pass
//! silently), and `match` in the runner is exhaustive at compile time — adding a verb
//! cannot be half-implemented.

use corelib::wire::Toml;

use super::super::world;

/// One line of a scenario.
#[derive(Clone, Debug)]
pub enum Step {
    // ── what people and programs do ─────────────────────────────────────────
    /// A chat message from the paired user (or from `from`, when set on the same step).
    Chat { text: String, from: Option<i64> },
    /// A button tap — carries the callback data, e.g. `k:1`.
    Tap { data: String, from: Option<i64> },
    /// Bytes the shell printed.
    Pty(String),
    /// Paint the mirror with these lines, as a program repainting its screen.
    Screen(Vec<String>),
    /// Keys typed at the local keyboard.
    Local(String),
    /// The local user runs a command: the shell's start mark, then its echo.
    RunLocal(String),
    /// The shell's `preexec` mark — a command is starting.
    ShellStart,
    /// The shell's `precmd` mark — the command ended with this status.
    ShellEnd(i32),
    /// A program declares itself: any of `alt`, `mouse`, `bracketed`, `app_cursor`.
    AppModes(Vec<String>),
    /// The program hands the terminal back.
    AppRelease,
    /// The shell's line editor arms itself at the prompt. This is what zsh and bash
    /// actually do between commands, and the reason the detector cannot trust
    /// bracketed paste or application cursor keys on their own.
    ShellPrompt,
    /// Advance the clock.
    Wait(u64),

    // ── what must be true ───────────────────────────────────────────────────
    /// Every fragment appears somewhere in what was said to the chat.
    ExpectSays(Vec<String>),
    /// No fragment appears anywhere in what was said.
    ExpectNotSays(Vec<String>),
    /// Exactly these bytes reached the terminal since the last expectation.
    ExpectPty(String),
    /// Nothing at all reached the terminal.
    ExpectNoPty,
    ExpectAttached(bool),
    /// The live screen currently offers these callback values.
    ExpectButtons(Vec<String>),
    /// Every fragment appears in what was printed to the local pane.
    ExpectLocal(Vec<String>),
    /// Everything said since the last expectation went to this chat id.
    ExpectChatId(i64),
    /// A live frame was produced within this long of the last user action.
    ExpectFrameWithin(u64),
    /// The next frame is a NEW message rather than an edit of the previous one.
    ExpectLiveReposted(bool),
    /// Nothing is queued waiting to run later.
    ExpectNothingQueued,
}

use world::{list as list_at, text as str_at};

impl Step {
    pub fn parse(t: &Toml) -> Result<Step, String> {
        let from = t.get("from").and_then(|v| v.as_int());

        if let Some(text) = str_at(t, "chat") {
            return Ok(Step::Chat { text, from });
        }
        if let Some(data) = str_at(t, "tap") {
            return Ok(Step::Tap { data, from });
        }
        if let Some(s) = str_at(t, "pty") {
            return Ok(Step::Pty(s));
        }
        if let Some(lines) = list_at(t, "screen") {
            return Ok(Step::Screen(lines));
        }
        if let Some(s) = str_at(t, "local") {
            return Ok(Step::Local(s));
        }
        if let Some(s) = str_at(t, "run_local") {
            return Ok(Step::RunLocal(s));
        }
        if t.get("shell_start").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(Step::ShellStart);
        }
        if let Some(n) = t.get("shell_end").and_then(|v| v.as_int()) {
            return Ok(Step::ShellEnd(n as i32));
        }
        if t.get("shell_prompt").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(Step::ShellPrompt);
        }
        if let Some(modes) = str_at(t, "app_modes") {
            return Ok(Step::AppModes(modes.split(',').map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect()));
        }
        if t.get("app_release").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(Step::AppRelease);
        }
        if let Some(ms) = t.get("wait_ms").and_then(|v| v.as_int()) {
            return Ok(Step::Wait(ms.max(0) as u64));
        }

        if let Some(v) = list_at(t, "expect_says") {
            return Ok(Step::ExpectSays(v));
        }
        if let Some(v) = list_at(t, "expect_not_says") {
            return Ok(Step::ExpectNotSays(v));
        }
        if let Some(s) = str_at(t, "expect_pty") {
            return Ok(Step::ExpectPty(s));
        }
        if t.get("expect_no_pty").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(Step::ExpectNoPty);
        }
        if let Some(b) = t.get("expect_attached").and_then(|v| v.as_bool()) {
            return Ok(Step::ExpectAttached(b));
        }
        if let Some(v) = list_at(t, "expect_buttons") {
            return Ok(Step::ExpectButtons(v));
        }
        if let Some(v) = list_at(t, "expect_local") {
            return Ok(Step::ExpectLocal(v));
        }
        if let Some(n) = t.get("expect_chat_id").and_then(|v| v.as_int()) {
            return Ok(Step::ExpectChatId(n));
        }
        if let Some(ms) = t.get("expect_frame_within_ms").and_then(|v| v.as_int()) {
            return Ok(Step::ExpectFrameWithin(ms.max(0) as u64));
        }
        if let Some(b) = t.get("expect_live_reposted").and_then(|v| v.as_bool()) {
            return Ok(Step::ExpectLiveReposted(b));
        }
        if t.get("expect_nothing_queued").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(Step::ExpectNothingQueued);
        }

        Err(world::unknown_verb(t))
    }
}
