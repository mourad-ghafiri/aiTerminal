//! The gate's decision layer, and the loop that runs it.
//!
//! [`Gate`] is the whole policy — authorization, the command guard, capture handling,
//! reply building — expressed as a pure function from an event to a list of
//! [`Action`]s. It touches no PTY, no network and no clock, so every rule below is
//! tested directly. [`run`] is the thin part: threads, guards, and executing actions.

use std::sync::Arc;

use super::auth::{Access, Auth};
use super::capture::{self, Capture, Progress, Submit};
use super::command::{self, Command};
use super::marks::Mark;
use super::reply;
use super::telegram::api::FileKind;
use crate::security::{Policy, RedactScope, Verdict};

/// How long a guard confirmation waits for `/yes` before it is dropped. Long enough
/// to walk back to your phone, short enough that a stale "yes" can't fire much later.
const CONFIRM_TTL_MS: u64 = 120_000;
/// Scrollback lines kept when rendering a capture.
const CAPTURE_LINES: usize = 400;

/// A side effect for the driver to perform.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Raw bytes to the shell.
    Pty(Vec<u8>),
    /// A message for the chat (already HTML).
    Say(String),
    /// Capture and send a screenshot, with this note in the caption.
    Shot(String),
    /// Send the last capture as a text file.
    File { name: String, text: String, caption: String },
    /// A line for the local pane.
    Local(String),
    /// Publish the paired chat to the gate's record.
    Peer(String),
    /// Run out-of-band, without touching the shared shell.
    Sh(String),
    /// End the gate.
    Stop(String),
}

/// Everything the gate needs to know about its configuration.
pub struct Settings {
    pub plain_runs: bool,
    pub max_reply_messages: usize,
    pub screenshot: FileKind,
    pub cols: u16,
}

/// The decision layer.
pub struct Gate {
    auth: Auth,
    capture: Capture,
    policy: Arc<Policy>,
    settings: Settings,
    /// A guard-confirm command waiting for `/yes`.
    pending: Option<(String, u64)>,
    /// The most recent finished capture, for `/full`.
    last: Option<(String, Vec<String>)>,
    /// Remote traffic timestamp, for the idle timeout.
    pub last_activity_ms: u64,
    style: super::chrome::Style,
}

impl Gate {
    pub fn new(auth: Auth, policy: Arc<Policy>, settings: Settings) -> Gate {
        Gate {
            auth,
            capture: Capture::new(),
            policy,
            settings,
            pending: None,
            last: None,
            last_activity_ms: 0,
            style: super::chrome::Style::default(),
        }
    }

    pub fn auth_mut(&mut self) -> &mut Auth {
        &mut self.auth
    }

    pub fn capture(&self) -> &Capture {
        &self.capture
    }

    pub fn set_cols(&mut self, cols: u16) {
        self.settings.cols = cols;
    }

    // ── inbound: the chat ────────────────────────────────────────────────────

    /// Handle one chat message.
    pub fn on_chat(&mut self, chat_id: i64, name: &str, text: &str, alt: bool, now: u64) -> Vec<Action> {
        let cmd = command::parse(text, self.settings.plain_runs);
        let pair_code = match &cmd {
            Command::Pair(c) => Some(c.as_str()),
            _ => None,
        };
        match self.auth.check(chat_id, name, pair_code) {
            // A stranger who found the bot learns nothing — not even that it is live.
            Access::Silent => Vec::new(),
            Access::Refused(msg) => vec![Action::Say(reply::escape_html(&msg))],
            Access::JustPaired => {
                self.last_activity_ms = now;
                vec![
                    Action::Local(self.style.inbound(name, "paired")),
                    Action::Peer(format!("{name} ({chat_id})")),
                    Action::Say(format!(
                        "<b>paired</b> — you are driving <code>{}</code>\n\n{}",
                        reply::escape_html(&hostname()),
                        command::help_html(self.settings.plain_runs)
                    )),
                ]
            }
            Access::Allowed => {
                self.last_activity_ms = now;
                self.dispatch(cmd, name, alt, now)
            }
        }
    }

    fn dispatch(&mut self, cmd: Command, who: &str, alt: bool, now: u64) -> Vec<Action> {
        match cmd {
            Command::Pair(_) => vec![Action::Say("already paired".into())],
            Command::Help => vec![Action::Say(command::help_html(self.settings.plain_runs))],
            Command::Ignored(t) => vec![Action::Say(format!(
                "not run — send <code>/run {}</code> (or set <code>[gates] plain_text = \"run\"</code>)",
                reply::escape_html(&t)
            ))],
            Command::Status => vec![Action::Say(self.status_html())],
            Command::Stop => vec![Action::Say("stopping the gate — bye".into()), Action::Stop("chat asked to stop".into())],
            Command::Shot => vec![Action::Shot(String::new())],
            Command::Full => vec![self.full_action()],
            Command::Cancel => vec![
                Action::Local(self.style.inbound(who, "^C")),
                Action::Pty(vec![0x03]),
                Action::Say("sent Ctrl-C".into()),
            ],
            Command::Key(name) => match command::key_bytes(&name) {
                Some(bytes) => vec![Action::Local(self.style.inbound(who, &format!("key {name}"))), Action::Pty(bytes)],
                // Never fall back to typing the name as text — `/key rm -rf` must not
                // become input at the prompt.
                None => vec![Action::Say(format!("unknown key <code>{}</code> — see /help", reply::escape_html(&name)))],
            },
            Command::Keys(text) => vec![
                Action::Local(self.style.inbound(who, &format!("type {text:?}"))),
                Action::Pty(text.into_bytes()),
            ],
            Command::Sh(c) => match self.policy.check_command(&c) {
                Verdict::Deny { reason } => self.blocked(who, &c, &reason),
                _ => vec![Action::Local(self.style.inbound(who, &format!("sh {c}"))), Action::Sh(c)],
            },
            Command::Ai(prompt) => self.run_line(format!("@ai {prompt}"), who, alt, now),
            Command::Yes => match self.pending.take() {
                Some((c, at)) if now.saturating_sub(at) <= CONFIRM_TTL_MS => {
                    let mut acts = vec![Action::Say(format!("running <code>{}</code>", reply::escape_html(&c)))];
                    acts.extend(self.submit(c, who, alt, now));
                    acts
                }
                Some(_) => vec![Action::Say("that confirmation expired — send the command again".into())],
                None => vec![Action::Say("nothing is waiting for confirmation".into())],
            },
            Command::No => {
                let had = self.pending.take().is_some();
                vec![Action::Say(if had { "dropped".into() } else { "nothing to drop".into() })]
            }
            Command::Run(line) => self.run_line(line, who, alt, now),
        }
    }

    /// A command line from the chat, before the guard sees it.
    fn run_line(&mut self, line: String, who: &str, alt: bool, now: u64) -> Vec<Action> {
        // A command that is waiting for input wants an ANSWER, not a new command.
        // `sudo` asking for a password is the everyday case, and typing the password
        // as a fresh shell command would both fail and echo it.
        if self.capture.awaiting_input() {
            return vec![
                Action::Local(self.style.inbound(who, &format!("stdin: {line}"))),
                Action::Pty(format!("{line}\r").into_bytes()),
                Action::Say("sent to the running command".into()),
            ];
        }
        // While a full-screen program owns the terminal, text is keystrokes.
        if alt {
            return vec![
                Action::Local(self.style.inbound(who, &format!("keys: {line}"))),
                Action::Pty(line.into_bytes()),
                Action::Say("sent as keystrokes (a full-screen app is open) — /key enter to submit".into()),
            ];
        }
        match self.policy.check_command(&line) {
            Verdict::Deny { reason } => self.blocked(who, &line, &reason),
            Verdict::Confirm { reason } => {
                self.pending = Some((line.clone(), now));
                vec![Action::Say(format!(
                    "⚠ <b>{}</b>\n<pre>{}</pre>\n/yes to run · /no to drop",
                    reply::escape_html(&reason),
                    reply::escape_html(&line)
                ))]
            }
            Verdict::Allow => self.submit(line, who, alt, now),
        }
    }

    fn blocked(&self, who: &str, line: &str, reason: &str) -> Vec<Action> {
        vec![
            // Recorded locally too: a blocked attempt is exactly what the person at
            // the keyboard would want to know about.
            Action::Local(self.style.notice(&format!("blocked from {who}: {line}"))),
            Action::Say(format!("⛔ blocked by the command guard: {}", reply::escape_html(reason))),
        ]
    }

    fn submit(&mut self, line: String, who: &str, alt: bool, now: u64) -> Vec<Action> {
        let echo = Action::Local(self.style.inbound(who, &line));
        match self.capture.submit(line, alt, now) {
            Submit::Running => {
                let mut acts = vec![echo];
                acts.extend(self.drain(now));
                acts
            }
            Submit::Queued(n) => vec![Action::Say(format!(
                "queued — the terminal is busy ({n} ahead). It will run when the shell is free."
            ))],
            Submit::Full => vec![Action::Say("too many commands are already waiting — try again shortly".into())],
        }
    }

    // ── inbound: the shell ───────────────────────────────────────────────────

    pub fn on_output(&mut self, chunk: &[u8], marks: &[Mark], alt: bool, now: u64) -> Vec<Action> {
        self.capture.on_output(chunk, marks, alt, now);
        self.drain(now)
    }

    pub fn on_local(&mut self, bytes: &[u8]) {
        self.capture.on_local(bytes);
    }

    pub fn tick(&mut self, alt: bool, now: u64) -> Vec<Action> {
        self.capture.tick(alt, now);
        self.drain(now)
    }

    /// Turn capture events into actions.
    fn drain(&mut self, _now: u64) -> Vec<Action> {
        let mut acts = Vec::new();
        for ev in self.capture.drain() {
            match ev {
                capture::Event::Dispatch(cmd) => acts.push(Action::Pty(format!("{cmd}\r").into_bytes())),
                capture::Event::Progress { cmd, kind, elapsed_ms, bytes } => {
                    let note = match kind {
                        Progress::StillRunning => format!("still running · {}", human_ms(elapsed_ms)),
                        Progress::AwaitingInput => "waiting — reply to send input, or /cancel".to_string(),
                    };
                    let header = format!("❯ {cmd} · {note}");
                    let lines = self.lines(&bytes);
                    let r = reply::format(&header, &lines, 1);
                    acts.extend(r.messages.into_iter().map(Action::Say));
                }
                capture::Event::Finished { cmd, status, elapsed_ms, bytes, saw_alt, elided } => {
                    let mark = match status {
                        Some(0) => "✓".to_string(),
                        Some(n) => format!("✗ {n}"),
                        None => "·".to_string(),
                    };
                    let header = format!("❯ {cmd} · {mark} · {}", human_ms(elapsed_ms));
                    let lines = self.lines(&bytes);
                    self.last = Some((header.clone(), lines.clone()));
                    // A program that took over the screen leaves no meaningful text —
                    // a picture is the only honest answer.
                    if saw_alt && lines.iter().all(|l| l.trim().is_empty()) {
                        acts.push(Action::Shot(header));
                        continue;
                    }
                    let r = reply::format(&header, &lines, self.settings.max_reply_messages);
                    let truncated = r.truncated || elided;
                    acts.extend(r.messages.into_iter().map(Action::Say));
                    if truncated {
                        acts.push(Action::Say("… output was trimmed — /full for the whole thing".into()));
                    }
                }
            }
        }
        acts
    }

    /// Render captured bytes to redacted plain text.
    fn lines(&self, bytes: &[u8]) -> Vec<String> {
        reply::to_lines(bytes, self.settings.cols, CAPTURE_LINES)
            .into_iter()
            .map(|l| {
                // A chat app is egress off this machine, so BOTH scopes apply: someone
                // who scoped a secret to `terminal` or to `ai` certainly meant "not to
                // my phone" as well.
                let l = self.policy.redact(&l, RedactScope::Terminal);
                self.policy.redact(&l, RedactScope::Ai)
            })
            .collect()
    }

    fn full_action(&self) -> Action {
        match &self.last {
            Some((header, lines)) => Action::File {
                name: "output.txt".into(),
                text: reply::plain(header, lines),
                caption: header.clone(),
            },
            None => Action::Say("nothing captured yet".into()),
        }
    }

    fn status_html(&self) -> String {
        let running = match self.capture.running() {
            Some(c) => format!("running <code>{}</code>", reply::escape_html(c)),
            None => "idle".to_string(),
        };
        let queued = self.capture.queued();
        let marks = if self.capture.marks_active() {
            "exact (shell reports command boundaries)"
        } else {
            "approximate (this shell doesn't report boundaries — output is detected by pauses)"
        };
        format!(
            "<b>gate</b> · {running}{}\nhost <code>{}</code>\ncompletion detection: {marks}",
            if queued > 0 { format!(" · {queued} queued") } else { String::new() },
            reply::escape_html(&hostname()),
        )
    }
}

/// `1400` → `1.4s`, `95000` → `1m35s`.
pub fn human_ms(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let s = ms / 1000;
    if s < 60 {
        return format!("{}.{}s", s, (ms % 1000) / 100);
    }
    format!("{}m{:02}s", s / 60, s % 60)
}

/// The machine's name, so a reply says which terminal answered.
pub fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this terminal".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_with(deny: &[&str], confirm: &[&str]) -> Arc<Policy> {
        let mut p = Policy::new();
        for d in deny {
            p.add_deny(d).unwrap();
        }
        for c in confirm {
            p.add_confirm(c).unwrap();
        }
        Arc::new(p)
    }

    fn gate_with(policy: Arc<Policy>, plain_runs: bool) -> Gate {
        let auth = Auth::new(true, Vec::new(), 0, "418207".into());
        Gate::new(auth, policy, Settings { plain_runs, max_reply_messages: 3, screenshot: FileKind::Document, cols: 80 })
    }

    fn paired() -> Gate {
        let mut g = gate_with(policy_with(&[], &[]), true);
        g.on_chat(7, "Mourad", "/pair 418-207", false, 0);
        g
    }

    /// Everything the gate would write to the shell.
    fn pty_bytes(acts: &[Action]) -> Vec<u8> {
        acts.iter()
            .filter_map(|a| match a {
                Action::Pty(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn said(acts: &[Action]) -> String {
        acts.iter()
            .filter_map(|a| match a {
                Action::Say(s) => Some(s.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn an_unpaired_chat_gets_nothing_and_reaches_nothing() {
        let mut g = gate_with(policy_with(&[], &[]), true);
        let acts = g.on_chat(7, "Stranger", "rm -rf /", false, 0);
        assert!(acts.is_empty(), "no reply, no echo, and above all no shell write");
    }

    #[test]
    fn pairing_welcomes_and_publishes_the_peer() {
        let mut g = gate_with(policy_with(&[], &[]), true);
        let acts = g.on_chat(7, "Mourad", "/pair 418-207", false, 0);
        assert!(acts.iter().any(|a| matches!(a, Action::Peer(p) if p.contains("Mourad"))));
        assert!(said(&acts).contains("paired"));
        assert!(said(&acts).contains("/shot"), "the welcome doubles as help");
        assert!(pty_bytes(&acts).is_empty());
    }

    #[test]
    fn a_paired_chat_runs_a_plain_command() {
        let mut g = paired();
        let acts = g.on_chat(7, "Mourad", "git status", false, 100);
        assert_eq!(pty_bytes(&acts), b"git status\r");
        assert!(acts.iter().any(|a| matches!(a, Action::Local(l) if l.contains("git status"))), "echoed locally");
    }

    #[test]
    fn a_denied_command_never_reaches_the_shell() {
        // The single most important test in this module.
        let mut g = gate_with(policy_with(&["^sudo\\b"], &[]), true);
        g.on_chat(7, "M", "/pair 418207", false, 0);
        let acts = g.on_chat(7, "M", "sudo rm -rf /", false, 1);
        assert!(pty_bytes(&acts).is_empty(), "a blocked command must not be written");
        assert!(said(&acts).contains("blocked"));
        assert!(acts.iter().any(|a| matches!(a, Action::Local(l) if l.contains("blocked"))), "and it is surfaced locally");
    }

    #[test]
    fn a_confirm_command_waits_for_an_explicit_yes() {
        let mut g = gate_with(policy_with(&[], &["rm"]), true);
        g.on_chat(7, "M", "/pair 418207", false, 0);
        let acts = g.on_chat(7, "M", "rm -rf build", false, 1);
        assert!(pty_bytes(&acts).is_empty(), "nothing runs before confirmation");
        assert!(said(&acts).contains("/yes"));

        let acts = g.on_chat(7, "M", "/yes", false, 2);
        assert_eq!(pty_bytes(&acts), b"rm -rf build\r");
    }

    #[test]
    fn a_confirmation_can_be_declined_and_expires() {
        let mut g = gate_with(policy_with(&[], &["rm"]), true);
        g.on_chat(7, "M", "/pair 418207", false, 0);
        g.on_chat(7, "M", "rm -rf build", false, 1);
        let acts = g.on_chat(7, "M", "/no", false, 2);
        assert!(pty_bytes(&acts).is_empty());
        assert!(said(&acts).contains("dropped"));

        g.on_chat(7, "M", "rm -rf build", false, 10);
        let acts = g.on_chat(7, "M", "/yes", false, 10 + CONFIRM_TTL_MS + 1);
        assert!(pty_bytes(&acts).is_empty(), "a stale yes must not fire");
        assert!(said(&acts).contains("expired"));
    }

    #[test]
    fn an_unknown_slash_command_produces_help_and_no_shell_write() {
        let mut g = paired();
        let acts = g.on_chat(7, "M", "/rm -rf /", false, 1);
        assert!(pty_bytes(&acts).is_empty());
        assert!(said(&acts).contains("/shot"), "help was sent");
    }

    #[test]
    fn plain_text_is_inert_when_configured_that_way() {
        let mut g = gate_with(policy_with(&[], &[]), false);
        g.on_chat(7, "M", "/pair 418207", false, 0);
        let acts = g.on_chat(7, "M", "rm -rf /", false, 1);
        assert!(pty_bytes(&acts).is_empty());
        assert!(said(&acts).contains("/run"));
    }

    #[test]
    fn keys_and_named_keys_reach_the_shell_verbatim() {
        let mut g = paired();
        assert_eq!(pty_bytes(&g.on_chat(7, "M", "/keys hello", false, 1)), b"hello");
        assert_eq!(pty_bytes(&g.on_chat(7, "M", "/key enter", false, 2)), b"\r");
        assert_eq!(pty_bytes(&g.on_chat(7, "M", "/cancel", false, 3)), &[0x03]);
    }

    #[test]
    fn an_unknown_key_name_is_refused_rather_than_typed() {
        let mut g = paired();
        let acts = g.on_chat(7, "M", "/key destroy-everything", false, 1);
        assert!(pty_bytes(&acts).is_empty(), "the name must never be typed as text");
        assert!(said(&acts).contains("unknown key"));
    }

    #[test]
    fn text_becomes_keystrokes_while_a_full_screen_program_is_open() {
        let mut g = paired();
        let acts = g.on_chat(7, "M", ":wq", true, 1);
        assert_eq!(pty_bytes(&acts), b":wq", "no trailing newline — /key enter submits");
        assert!(said(&acts).contains("keystrokes"));
    }

    #[test]
    fn text_becomes_stdin_while_a_command_is_waiting_for_input() {
        let mut g = paired();
        g.on_chat(7, "M", "sudo ls", false, 0);
        g.on_output(b"", &[Mark::Start], false, 1);
        g.on_output(b"Password:", &[], false, 2);
        g.tick(false, 2 + 8_000); // the quiet note fires: it is waiting

        let acts = g.on_chat(7, "M", "hunter2", false, 20_000);
        assert_eq!(pty_bytes(&acts), b"hunter2\r");
        assert!(said(&acts).contains("running command"), "and it is not treated as a new command");
    }

    #[test]
    fn a_finished_command_is_reported_with_its_output_and_status() {
        let mut g = paired();
        g.on_chat(7, "M", "ls", false, 0);
        g.on_output(b"", &[Mark::Start], false, 1);
        g.on_output(b"a.txt\r\nb.txt\r\n", &[], false, 2);
        let acts = g.on_output(b"", &[Mark::End(0)], false, 1_400);
        let text = said(&acts);
        assert!(text.contains("a.txt") && text.contains("b.txt"), "{text}");
        assert!(text.contains('✓'), "{text}");
    }

    #[test]
    fn a_failing_command_reports_its_exit_code() {
        let mut g = paired();
        g.on_chat(7, "M", "false", false, 0);
        g.on_output(b"", &[Mark::Start], false, 1);
        assert!(said(&g.on_output(b"", &[Mark::End(1)], false, 2)).contains("✗ 1"));
    }

    #[test]
    fn secrets_are_redacted_before_output_leaves_the_machine() {
        let mut p = Policy::new();
        p.add_redaction("AKIA[A-Z0-9]+", "«redacted»", RedactScope::Ai, false).unwrap();
        let mut g = gate_with(Arc::new(p), true);
        g.on_chat(7, "M", "/pair 418207", false, 0);
        g.on_chat(7, "M", "env", false, 1);
        g.on_output(b"", &[Mark::Start], false, 2);
        g.on_output(b"AWS_KEY=AKIA1234567890\r\n", &[], false, 3);
        let text = said(&g.on_output(b"", &[Mark::End(0)], false, 4));
        assert!(!text.contains("AKIA1234567890"), "a secret reached the chat: {text}");
        assert!(text.contains("redacted"), "{text}");
    }

    #[test]
    fn a_full_screen_program_answers_with_a_picture_not_empty_text() {
        let mut g = paired();
        g.on_chat(7, "M", "htop", false, 0);
        g.on_output(b"", &[Mark::Start], false, 1);
        g.on_output(b"\x1b[?1049h", &[], true, 2);
        let acts = g.on_output(b"", &[Mark::End(0)], false, 3);
        assert!(acts.iter().any(|a| matches!(a, Action::Shot(_))), "expected a screenshot, got {acts:?}");
    }

    #[test]
    fn full_resends_the_last_capture_as_a_file() {
        let mut g = paired();
        assert!(said(&g.on_chat(7, "M", "/full", false, 1)).contains("nothing captured"));
        g.on_chat(7, "M", "ls", false, 2);
        g.on_output(b"", &[Mark::Start], false, 3);
        g.on_output(b"a.txt\r\n", &[], false, 4);
        g.on_output(b"", &[Mark::End(0)], false, 5);
        match &g.on_chat(7, "M", "/full", false, 6)[0] {
            Action::File { text, name, .. } => {
                assert!(text.contains("a.txt"));
                assert_eq!(name, "output.txt");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn stop_from_the_chat_ends_the_gate() {
        let mut g = paired();
        let acts = g.on_chat(7, "M", "/stop", false, 1);
        assert!(acts.iter().any(|a| matches!(a, Action::Stop(_))));
    }

    #[test]
    fn status_admits_when_completion_detection_is_only_approximate() {
        let mut g = paired();
        assert!(g.on_chat(7, "M", "/status", false, 1)[0] == Action::Say(g.status_html()));
        assert!(g.status_html().contains("approximate"), "a degraded session must say so");
        g.on_output(b"", &[Mark::Start], false, 2);
        g.on_output(b"", &[Mark::End(0)], false, 3);
        assert!(g.status_html().contains("exact"));
    }

    #[test]
    fn the_ai_command_is_submitted_to_the_shell() {
        let mut g = paired();
        let acts = g.on_chat(7, "M", "/ai why did the build fail", false, 1);
        assert_eq!(pty_bytes(&acts), b"@ai why did the build fail\r");
    }

    #[test]
    fn a_command_arriving_mid_typing_is_queued_not_spliced() {
        let mut g = paired();
        g.on_local(b"git comm");
        let acts = g.on_chat(7, "M", "ls", false, 1);
        assert!(pty_bytes(&acts).is_empty(), "splicing would run a command neither party asked for");
        assert!(said(&acts).contains("queued"));
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human_ms(420), "420ms");
        assert_eq!(human_ms(1_400), "1.4s");
        assert_eq!(human_ms(95_000), "1m35s");
    }
}
