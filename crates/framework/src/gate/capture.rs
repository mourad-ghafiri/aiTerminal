//! When did a remote command finish, and which bytes were its output?
//!
//! This is the hard part of relaying a shell. The module is a **pure state machine**:
//! it performs no I/O, takes time as a parameter, and answers with [`Event`]s the
//! driver executes. That is what makes every case below testable without a terminal.
//!
//! Two invariants it exists to protect:
//!
//! **A remote command is never interleaved with local typing.** Writing `ls -la\n`
//! while the local user has `git comm` half-typed runs `git commls -la` — a *different
//! command than either party asked for*. So a remote command dispatches only when the
//! line is known to be clear, and otherwise waits its turn. The local human owns the
//! terminal; we never clear their line to make room.
//!
//! **A slow command is never abandoned.** A nine-minute build still gets its final
//! reply with the real exit status; progress notes go out along the way rather than
//! the capture timing out and desynchronizing.

use std::collections::VecDeque;

use super::marks::Mark;

/// The first bytes of a command's output, always kept: the invocation and whatever
/// it printed first.
const HEAD_CAP: usize = 8 * 1024;
/// The last bytes, kept in a ring: where the error usually is.
const TAIL_CAP: usize = 56 * 1024;
/// No `Start` mark this long after dispatch means the shell isn't emitting marks
/// (no shell integration, or an exotic shell) — fall back to quiet-period detection.
const MARK_GRACE_MS: u64 = 5_000;
/// Unmarked captures end after this much silence.
const DEBOUNCE_QUIET_MS: u64 = 1_200;
/// A marked command that has been silent this long is probably waiting for input
/// (`sudo`, `ssh`), so ship what we have once.
const QUIET_NOTE_MS: u64 = 8_000;
/// First progress note for a long-running command, then doubling up to the cap.
const FIRST_INTERIM_MS: u64 = 120_000;
const MAX_INTERIM_MS: u64 = 900_000;
/// How many remote commands may wait behind local typing.
const QUEUE_CAP: usize = 8;

/// A monotonic millisecond clock — a seam so tests drive time directly.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// The real clock, counting from gate start.
pub struct SystemClock {
    start: std::time::Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock { start: std::time::Instant::now() }
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

/// Why a progress note is being sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Still producing output after a long time.
    StillRunning,
    /// Silent for a while — very likely prompting for input.
    AwaitingInput,
}

/// What the driver should do next.
#[derive(Debug, PartialEq)]
pub enum Event {
    /// Write this command line to the PTY (the driver adds the newline).
    Dispatch(String),
    /// Send an interim update; the command is still running.
    Progress { cmd: String, kind: Progress, elapsed_ms: u64, bytes: Vec<u8> },
    /// The command ended. `status` is `None` when marks were unavailable and the end
    /// was inferred from silence.
    Finished { cmd: String, status: Option<i32>, elapsed_ms: u64, bytes: Vec<u8>, saw_alt: bool, elided: bool },
}

/// The outcome of offering a command to the machine.
#[derive(Debug, PartialEq, Eq)]
pub enum Submit {
    /// Dispatched immediately (an `Event::Dispatch` is waiting in [`Capture::drain`]).
    Running,
    /// Something else has the shell; this many commands are now waiting.
    Queued(usize),
    /// The queue is full — the caller should tell the user to wait.
    Full,
}

/// A bounded window over a command's output: the first `HEAD_CAP` bytes and the last
/// `TAIL_CAP`. Head *and* tail, because the invocation and the error are usually at
/// opposite ends of a long build log.
struct Ring {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
}

impl Ring {
    fn new() -> Self {
        Ring { head: Vec::new(), tail: VecDeque::new(), total: 0 }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        let mut rest = chunk;
        if self.head.len() < HEAD_CAP {
            let n = (HEAD_CAP - self.head.len()).min(rest.len());
            self.head.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
        }
        for &b in rest {
            if self.tail.len() == TAIL_CAP {
                self.tail.pop_front();
            }
            self.tail.push_back(b);
        }
    }

    /// True when output was dropped between head and tail.
    fn elided(&self) -> bool {
        self.total > (self.head.len() + self.tail.len()) as u64
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = self.head.clone();
        if self.elided() {
            let gap = self.total - (self.head.len() + self.tail.len()) as u64;
            out.extend_from_slice(format!("\r\n… {gap} bytes elided …\r\n").as_bytes());
        }
        out.extend(self.tail.iter().copied());
        out
    }

    /// The tail only — enough context for a progress note without resending the world.
    fn recent(&self, limit: usize) -> Vec<u8> {
        let skip = self.tail.len().saturating_sub(limit);
        if skip == 0 && self.tail.len() < limit {
            let mut out = self.head[self.head.len().saturating_sub(limit - self.tail.len())..].to_vec();
            out.extend(self.tail.iter().copied());
            return out;
        }
        self.tail.iter().skip(skip).copied().collect()
    }
}

enum State {
    Idle,
    /// Written to the PTY; waiting for the shell to confirm it started.
    Pending { cmd: String, at: u64 },
    Active {
        cmd: String,
        start: u64,
        quiet_since: u64,
        ring: Ring,
        saw_alt: bool,
        /// A real `Start` mark was seen; the end will be authoritative.
        marked: bool,
        /// Elapsed time at which the next progress note is due.
        next_interim: u64,
        /// Gap to the note after that — doubles, so a very long build reports
        /// often at first and then settles down.
        interim_step: u64,
        quiet_noted: bool,
    },
}

/// The relay's command bookkeeping.
pub struct Capture {
    state: State,
    queue: VecDeque<String>,
    events: Vec<Event>,
    /// The local user has an unsubmitted line at the prompt.
    local_line_dirty: bool,
    /// The local user's own command is running.
    local_busy: bool,
    /// Any mark has ever been seen, i.e. the shell really is reporting.
    marks_seen: bool,
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

impl Capture {
    pub fn new() -> Self {
        Capture {
            state: State::Idle,
            queue: VecDeque::new(),
            events: Vec::new(),
            local_line_dirty: false,
            local_busy: false,
            marks_seen: false,
        }
    }

    /// Whether the shell is reporting command boundaries. Surfaced by `/status` so a
    /// degraded session is never a silent surprise.
    pub fn marks_active(&self) -> bool {
        self.marks_seen
    }

    /// Nothing running and nothing waiting.
    pub fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle) && self.queue.is_empty()
    }

    /// A command is running and has already been reported as silent — almost always
    /// because it is prompting. The driver routes the next chat message to its stdin
    /// instead of treating it as a new command.
    pub fn awaiting_input(&self) -> bool {
        matches!(&self.state, State::Active { quiet_noted, .. } if *quiet_noted)
    }

    /// The command currently running, if any.
    pub fn running(&self) -> Option<&str> {
        match &self.state {
            State::Pending { cmd, .. } | State::Active { cmd, .. } => Some(cmd),
            State::Idle => None,
        }
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn drain(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Offer a remote command. It runs now if the shell is free, otherwise it waits —
    /// it is never spliced into a half-typed local line.
    pub fn submit(&mut self, cmd: String, alt: bool, now: u64) -> Submit {
        if self.can_dispatch(alt) {
            self.start(cmd, now);
            return Submit::Running;
        }
        if self.queue.len() >= QUEUE_CAP {
            return Submit::Full;
        }
        self.queue.push_back(cmd);
        Submit::Queued(self.queue.len())
    }

    /// Bytes the local user typed, already relayed to the PTY.
    pub fn on_local(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match b {
                // Submitting or discarding a line clears it. Enter also means the shell
                // is about to be busy — but only trust that when marks can clear it
                // again, otherwise a single local Enter would block remote use forever.
                b'\r' | b'\n' => {
                    self.local_line_dirty = false;
                    if self.marks_seen {
                        self.local_busy = true;
                    }
                }
                0x03 | 0x15 => {
                    self.local_line_dirty = false; // Ctrl-C / Ctrl-U
                }
                0x7f | 0x08 => {} // backspace neither dirties nor clears
                _ => {
                    if b >= 0x20 {
                        self.local_line_dirty = true;
                    }
                }
            }
        }
    }

    /// PTY output plus any marks extracted from it. `alt` is the mirror terminal's
    /// alt-screen state *after* feeding this chunk.
    pub fn on_output(&mut self, chunk: &[u8], marks: &[Mark], alt: bool, now: u64) {
        for m in marks {
            self.marks_seen = true;
            match m {
                Mark::Start => self.on_start(now),
                Mark::End(status) => self.on_end(*status, now),
            }
        }
        if let State::Active { ring, quiet_since, saw_alt, .. } = &mut self.state {
            if !chunk.is_empty() {
                ring.push(chunk);
                *quiet_since = now;
            }
            *saw_alt |= alt;
        }
        self.pump(alt, now);
    }

    /// Time passed. Drives the grace period, the debounce fallback, and progress notes.
    pub fn tick(&mut self, alt: bool, now: u64) {
        // No mark arrived in time: this shell isn't reporting, so switch this command
        // to silence-based detection rather than hanging forever.
        if let State::Pending { cmd, at } = &self.state {
            if now.saturating_sub(*at) >= MARK_GRACE_MS {
                let (cmd, at) = (cmd.clone(), *at);
                self.state = State::Active {
                    cmd,
                    start: at,
                    quiet_since: now,
                    ring: Ring::new(),
                    saw_alt: alt,
                    marked: false,
                    next_interim: FIRST_INTERIM_MS,
                    interim_step: FIRST_INTERIM_MS,
                    quiet_noted: false,
                };
            }
        }

        let mut finish_unmarked = false;
        if let State::Active { start, quiet_since, marked, next_interim, interim_step, quiet_noted, cmd, ring, .. } =
            &mut self.state
        {
            let quiet = now.saturating_sub(*quiet_since);
            let elapsed = now.saturating_sub(*start);
            if !*marked {
                finish_unmarked = quiet >= DEBOUNCE_QUIET_MS;
            } else if elapsed >= *next_interim {
                self.events.push(Event::Progress {
                    cmd: cmd.clone(),
                    kind: Progress::StillRunning,
                    elapsed_ms: elapsed,
                    bytes: ring.recent(4096),
                });
                *interim_step = (*interim_step * 2).min(MAX_INTERIM_MS);
                *next_interim = elapsed + *interim_step;
            } else if !*quiet_noted && quiet >= QUIET_NOTE_MS {
                *quiet_noted = true;
                self.events.push(Event::Progress {
                    cmd: cmd.clone(),
                    kind: Progress::AwaitingInput,
                    elapsed_ms: elapsed,
                    bytes: ring.recent(4096),
                });
            }
        }
        if finish_unmarked {
            self.finish(None, now);
        }
        self.pump(alt, now);
    }

    // ── internals ────────────────────────────────────────────────────────────

    fn can_dispatch(&self, alt: bool) -> bool {
        matches!(self.state, State::Idle) && !self.local_busy && !self.local_line_dirty && !alt
    }

    fn start(&mut self, cmd: String, now: u64) {
        self.events.push(Event::Dispatch(cmd.clone()));
        self.state = State::Pending { cmd, at: now };
    }

    /// Send the next queued command if the shell has come free.
    fn pump(&mut self, alt: bool, now: u64) {
        while self.can_dispatch(alt) {
            let Some(cmd) = self.queue.pop_front() else { break };
            self.start(cmd, now);
        }
    }

    fn on_start(&mut self, now: u64) {
        match std::mem::replace(&mut self.state, State::Idle) {
            // Ours: the command we just wrote is now running.
            State::Pending { cmd, at } => {
                self.state = State::Active {
                    cmd,
                    start: at,
                    quiet_since: now,
                    ring: Ring::new(),
                    saw_alt: false,
                    marked: true,
                    next_interim: FIRST_INTERIM_MS,
                    interim_step: FIRST_INTERIM_MS,
                    quiet_noted: false,
                };
            }
            // Not ours: the local user ran something. Positional pairing is enough
            // because only one remote command is ever in flight.
            State::Idle => self.local_busy = true,
            other => self.state = other,
        }
    }

    fn on_end(&mut self, status: i32, now: u64) {
        match &self.state {
            State::Active { .. } => self.finish(Some(status), now),
            // The local user's command finished — or this is the very first prompt,
            // whose `precmd` fires with nothing before it.
            _ => self.local_busy = false,
        }
    }

    fn finish(&mut self, status: Option<i32>, now: u64) {
        if let State::Active { cmd, start, ring, saw_alt, .. } = std::mem::replace(&mut self.state, State::Idle) {
            self.events.push(Event::Finished {
                cmd,
                status,
                elapsed_ms: now.saturating_sub(start),
                bytes: ring.bytes(),
                saw_alt,
                elided: ring.elided(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap() -> Capture {
        Capture::new()
    }

    #[test]
    fn a_marked_command_replies_with_its_output_and_exit_status() {
        let mut c = cap();
        assert_eq!(c.submit("ls".into(), false, 0), Submit::Running);
        assert_eq!(c.drain(), vec![Event::Dispatch("ls".into())]);
        c.on_output(b"", &[Mark::Start], false, 10);
        c.on_output(b"a.txt\r\n", &[], false, 20);
        c.on_output(b"", &[Mark::End(0)], false, 30);
        match &c.drain()[..] {
            [Event::Finished { cmd, status, bytes, elapsed_ms, .. }] => {
                assert_eq!(cmd, "ls");
                assert_eq!(*status, Some(0));
                assert_eq!(bytes, b"a.txt\r\n");
                assert_eq!(*elapsed_ms, 30);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(c.is_idle());
    }

    #[test]
    fn output_printed_before_the_start_mark_is_not_captured() {
        // Leftovers from the previous prompt must not be attributed to our command.
        let mut c = cap();
        c.submit("ls".into(), false, 0);
        c.drain();
        c.on_output(b"stale banner\r\n", &[], false, 5);
        c.on_output(b"", &[Mark::Start], false, 10);
        c.on_output(b"real\r\n", &[], false, 15);
        c.on_output(b"", &[Mark::End(0)], false, 20);
        let Event::Finished { bytes, .. } = &c.drain()[0] else { panic!() };
        assert_eq!(bytes, b"real\r\n");
    }

    #[test]
    fn a_local_command_is_recognized_and_never_reported_to_the_chat() {
        let mut c = cap();
        c.on_output(b"", &[Mark::Start], false, 0); // the human ran something
        c.on_output(b"their output\r\n", &[], false, 5);
        c.on_output(b"", &[Mark::End(0)], false, 10);
        assert!(c.drain().is_empty(), "the chat must not receive the local user's output");
        assert!(c.is_idle());
    }

    #[test]
    fn a_remote_command_waits_while_the_local_user_is_typing() {
        // The corruption case: dispatching here would splice `ls` into `git comm`.
        let mut c = cap();
        c.on_local(b"git comm");
        assert_eq!(c.submit("ls".into(), false, 0), Submit::Queued(1));
        assert!(c.drain().is_empty(), "nothing may reach the PTY yet");

        c.on_local(b"it -m x\r"); // the human submits their own line
        c.tick(false, 10);
        assert_eq!(c.drain(), vec![Event::Dispatch("ls".into())], "queued command runs once the line is clear");
    }

    #[test]
    fn after_enter_the_shell_is_treated_as_busy_until_the_prompt_returns() {
        let mut c = cap();
        c.on_output(b"", &[Mark::Start], false, 0); // teach it that marks work
        c.on_output(b"", &[Mark::End(0)], false, 1);
        c.on_local(b"sleep 30\r");
        assert_eq!(c.submit("ls".into(), false, 5), Submit::Queued(1));
        c.on_output(b"", &[Mark::Start], false, 6);
        c.tick(false, 10);
        assert!(c.drain().is_empty(), "still the local user's turn");
        c.on_output(b"", &[Mark::End(0)], false, 20);
        assert_eq!(c.drain(), vec![Event::Dispatch("ls".into())]);
    }

    #[test]
    fn without_marks_a_local_enter_does_not_block_the_gate_forever() {
        // A shell with no integration never reports, so the optimistic "busy after
        // Enter" latch must not engage — otherwise the gate dies at first local use.
        let mut c = cap();
        c.on_local(b"echo hi\r");
        assert_eq!(c.submit("ls".into(), false, 0), Submit::Running);
    }

    #[test]
    fn a_command_is_never_dispatched_into_a_full_screen_program() {
        let mut c = cap();
        assert_eq!(c.submit("ls".into(), true, 0), Submit::Queued(1), "vim is up; typing a command would go to vim");
        c.tick(true, 100);
        assert!(c.drain().is_empty());
        c.tick(false, 200); // the user quit vim
        assert_eq!(c.drain(), vec![Event::Dispatch("ls".into())]);
    }

    #[test]
    fn the_queue_is_bounded() {
        let mut c = cap();
        c.on_local(b"x");
        for i in 1..=QUEUE_CAP {
            assert_eq!(c.submit(format!("c{i}"), false, 0), Submit::Queued(i));
        }
        assert_eq!(c.submit("one too many".into(), false, 0), Submit::Full);
    }

    #[test]
    fn a_shell_without_marks_falls_back_to_silence_detection() {
        let mut c = cap();
        c.submit("ls".into(), false, 0);
        c.drain();
        c.tick(false, MARK_GRACE_MS); // grace expires, no Start ever came
        c.on_output(b"a.txt\r\n", &[], false, MARK_GRACE_MS + 10);
        c.tick(false, MARK_GRACE_MS + 100);
        assert!(c.drain().is_empty(), "still producing output");
        c.tick(false, MARK_GRACE_MS + 10 + DEBOUNCE_QUIET_MS);
        match &c.drain()[..] {
            [Event::Finished { status, bytes, .. }] => {
                assert_eq!(*status, None, "an inferred end has no trustworthy exit status");
                assert_eq!(bytes, b"a.txt\r\n");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_silent_marked_command_prompts_once_then_stays_running() {
        // `sudo` printing `Password:` and waiting: ship what we have, once.
        let mut c = cap();
        c.submit("sudo ls".into(), false, 0);
        c.drain();
        c.on_output(b"", &[Mark::Start], false, 1);
        c.on_output(b"Password:", &[], false, 2);
        c.tick(false, 2 + QUIET_NOTE_MS);
        match &c.drain()[..] {
            [Event::Progress { kind, bytes, .. }] => {
                assert_eq!(*kind, Progress::AwaitingInput);
                assert_eq!(bytes, b"Password:");
            }
            other => panic!("unexpected {other:?}"),
        }
        c.tick(false, 2 + QUIET_NOTE_MS * 3);
        assert!(c.drain().is_empty(), "the note must not repeat");
        assert!(!c.is_idle(), "the command is still running");
    }

    #[test]
    fn a_long_build_gets_progress_notes_and_still_reports_its_real_status() {
        let mut c = cap();
        c.submit("cargo build".into(), false, 0);
        c.drain();
        c.on_output(b"", &[Mark::Start], false, 0);
        let mut t = 0;
        let mut notes = 0;
        // Ten minutes of a compiler chattering away.
        while t < 600_000 {
            t += 30_000;
            c.on_output(b"   Compiling something\r\n", &[], false, t);
            c.tick(false, t);
            notes += c.drain().iter().filter(|e| matches!(e, Event::Progress { .. })).count();
        }
        assert!(notes >= 2, "expected periodic progress, got {notes}");
        c.on_output(b"", &[Mark::End(101)], false, t);
        match &c.drain()[..] {
            [Event::Finished { status, .. }] => assert_eq!(*status, Some(101), "never abandoned mid-run"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn huge_output_is_bounded_keeping_the_head_and_the_tail() {
        let mut c = cap();
        c.submit("dump".into(), false, 0);
        c.drain();
        c.on_output(b"", &[Mark::Start], false, 0);
        c.on_output(b"FIRST\r\n", &[], false, 1);
        for _ in 0..1024 {
            c.on_output(&vec![b'x'; 1024], &[], false, 2); // 1 MiB
        }
        c.on_output(b"LAST\r\n", &[], false, 3);
        c.on_output(b"", &[Mark::End(0)], false, 4);
        let Event::Finished { bytes, elided, .. } = &c.drain()[0] else { panic!() };
        assert!(*elided);
        assert!(bytes.len() <= HEAD_CAP + TAIL_CAP + 64, "buffer grew to {}", bytes.len());
        assert!(bytes.starts_with(b"FIRST"), "the invocation's first output is kept");
        assert!(bytes.ends_with(b"LAST\r\n"), "the tail — where errors live — is kept");
        assert!(String::from_utf8_lossy(bytes).contains("elided"), "the gap is disclosed");
    }

    #[test]
    fn entering_a_full_screen_program_is_recorded_on_the_capture() {
        let mut c = cap();
        c.submit("vim x".into(), false, 0);
        c.drain();
        c.on_output(b"", &[Mark::Start], false, 1);
        c.on_output(b"\x1b[?1049h", &[], true, 2);
        c.on_output(b"", &[Mark::End(0)], false, 3);
        let Event::Finished { saw_alt, .. } = &c.drain()[0] else { panic!() };
        assert!(*saw_alt, "the driver answers with a screenshot instead of empty text");
    }

    #[test]
    fn a_stray_end_mark_before_anything_ran_is_ignored() {
        // `precmd` fires for the very first prompt, with no command before it.
        let mut c = cap();
        c.on_output(b"", &[Mark::End(0)], false, 0);
        assert!(c.drain().is_empty());
        assert!(c.is_idle());
        assert_eq!(c.submit("ls".into(), false, 1), Submit::Running);
    }
}
