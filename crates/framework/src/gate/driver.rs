//! The gate's decision layer, and the loop that runs it.
//!
//! [`Gate`] is the whole policy — authorization, the command guard, capture handling,
//! reply building — expressed as a pure function from an event to a list of
//! [`Action`]s. It touches no PTY, no network and no clock, so every rule below is
//! tested directly. [`run`] is the thin part: threads, guards, and executing actions.

use std::sync::Arc;

use super::attach::{self, Attacher};
use super::auth::{Access, Auth};
use super::capture::{self, Capture, Progress, Submit};
use super::command::{self, Command};
use super::keys;
use super::marks::Mark;
use super::reply;
use super::telegram::api::{FileKind, Keyboard};
use crate::security::{Policy, RedactScope, Verdict};

/// How long a guard confirmation waits for `/yes` before it is dropped. Long enough
/// to walk back to your phone, short enough that a stale "yes" can't fire much later.
const CONFIRM_TTL_MS: u64 = 120_000;
/// Scrollback lines kept when rendering a capture.
const CAPTURE_LINES: usize = 400;
/// Rows of an attached program's screen shown in the live message. A frame has to fit
/// ONE message (it is edited in place), so the newest rows win.
const FRAME_ROWS: usize = 40;

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
    /// The live screen of an attached program: one message, edited in place, with the
    /// program's current choices as buttons.
    Live { html: String, keys: Keyboard },
    /// Acknowledge a button tap so the sender's client stops spinning.
    Answer(String),
    /// End the gate.
    Stop(String),
}

/// What the mirror terminal currently reports. Cheap to read, so the relay hands it to
/// the gate on every iteration; the screen itself is rendered only when a frame is due.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mirror {
    /// A program has taken the terminal (see `Term::app_control`).
    pub app_control: bool,
    /// It wants pasted text bracketed.
    pub bracketed: bool,
    /// It put the cursor keys in application mode.
    pub app_cursor: bool,
    /// The cursor looks parked at a REPL prompt.
    pub at_prompt: bool,
    /// `Term::generation()` — the change counter frames are debounced on.
    pub generation: u64,
}

/// Everything the gate needs to know about its configuration.
pub struct Settings {
    pub plain_runs: bool,
    pub max_reply_messages: usize,
    pub screenshot: FileKind,
    pub cols: u16,
    /// Whether to attach to interactive programs at all (`[gates] attach`).
    pub attach: bool,
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
    /// Whether a program owns the terminal, and when to ship a frame.
    attach: Attacher,
    /// The mirror's latest report — so every path can encode keys the way the attached
    /// program expects without threading four booleans through each call.
    mirror: Mirror,
    /// A frame is due; the relay renders the screen and calls [`Gate::frame`].
    frame_due: bool,
    /// The running command was attached at some point, so its byte capture is escape
    /// soup and must not be dumped into the chat when it ends.
    was_attached: bool,
    /// The terminal's window title — how we name a program the chat did not launch.
    title: String,
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
            attach: Attacher::new(),
            mirror: Mirror::default(),
            frame_due: false,
            was_attached: false,
            title: String::new(),
        }
    }

    pub fn attached(&self) -> bool {
        self.attach.attached()
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
    pub fn on_chat(&mut self, chat_id: i64, name: &str, text: &str, now: u64) -> Vec<Action> {
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
                        self.help_html()
                    )),
                ]
            }
            Access::Allowed => {
                self.last_activity_ms = now;
                self.dispatch(cmd, name, now)
            }
        }
    }

    fn dispatch(&mut self, cmd: Command, who: &str, now: u64) -> Vec<Action> {
        match cmd {
            Command::Pair(_) => vec![Action::Say("already paired".into())],
            Command::Help => vec![Action::Say(self.help_html())],
            Command::Ignored(t) => vec![Action::Say(format!(
                "not run — send <code>/run {}</code> (or set <code>[gates] plain_text = \"run\"</code>)",
                reply::escape_html(&t)
            ))],
            Command::Status => vec![Action::Say(self.status_html())],
            Command::Stop => vec![Action::Say("stopping the gate — bye".into()), Action::Stop("chat asked to stop".into())],
            Command::Shot => vec![Action::Shot(String::new())],
            Command::Full => vec![self.full_action()],
            Command::Cancel => {
                self.attach.invalidate();
                let mut acts =
                    vec![Action::Local(self.style.inbound(who, "^C")), Action::Pty(vec![0x03])];
                // While attached the live screen answers; a separate line would be noise.
                if !self.attach.attached() {
                    acts.push(Action::Say("sent Ctrl-C".into()));
                }
                acts
            }
            Command::Key(name) => match keys::key_bytes(&name, self.mirror.app_cursor) {
                Some(bytes) => {
                    self.attach.invalidate();
                    vec![Action::Local(self.style.inbound(who, &format!("key {name}"))), Action::Pty(bytes)]
                }
                // Never fall back to typing the name as text — `/key rm -rf` must not
                // become input at the prompt.
                None => vec![Action::Say(format!("unknown key <code>{}</code> — see /help", reply::escape_html(&name)))],
            },
            Command::Keys(text) => self.type_into_app(&text, who, false, "typed — /key enter to submit"),
            Command::Sh(c) => match self.policy.check_command(&c) {
                Verdict::Deny { reason } => self.blocked(who, &c, &reason),
                _ => vec![Action::Local(self.style.inbound(who, &format!("sh {c}"))), Action::Sh(c)],
            },
            Command::Ai(prompt) => self.run_line(format!("@ai {prompt}"), who, now),
            Command::Yes => match self.pending.take() {
                Some((c, at)) if now.saturating_sub(at) <= CONFIRM_TTL_MS => {
                    let mut acts = vec![Action::Say(format!("running <code>{}</code>", reply::escape_html(&c)))];
                    acts.extend(self.submit(c, who, now));
                    acts
                }
                Some(_) => vec![Action::Say("that confirmation expired — send the command again".into())],
                None => vec![Action::Say("nothing is waiting for confirmation".into())],
            },
            Command::No => {
                let had = self.pending.take().is_some();
                vec![Action::Say(if had { "dropped".into() } else { "nothing to drop".into() })]
            }
            // Plain text: shell command when detached, input to the program when attached.
            Command::Text(line) => {
                if self.attach.attached() {
                    return self.type_into_app(&line, who, true, "sent");
                }
                self.run_line(line, who, now)
            }
            // An explicit `/run` is always a shell command — and while a program owns
            // the terminal there is no shell to run it in. Refusing beats the old
            // behaviour, where it queued silently and fired after the program exited.
            Command::Run(line) => {
                if self.attach.attached() {
                    return vec![Action::Say(format!(
                        "the terminal is busy with <b>{}</b> — send text to type into it, \
                         <code>/sh {}</code> to run this out-of-band, or <code>/key ctrl-c</code> to interrupt.",
                        reply::escape_html(&self.app_name()),
                        reply::escape_html(&line)
                    ))];
                }
                self.run_line(line, who, now)
            }
        }
    }

    /// A command line from the chat, before the guard sees it.
    fn run_line(&mut self, line: String, who: &str, now: u64) -> Vec<Action> {
        // A command that has gone quiet is waiting for an ANSWER, not a new command —
        // `sudo` asking for a password is the everyday case, and running the password
        // as a shell line would both fail and echo it. (An interactive program that
        // announced itself is handled earlier, by the attach path.)
        if self.capture.awaiting_input() {
            return self.type_into_app(&line, who, true, "sent to the running command");
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
            Verdict::Allow => self.submit(line, who, now),
        }
    }

    /// Send text to whatever owns the terminal, encoded the way it asked for.
    ///
    /// This deliberately does **not** go through the command guard: it is input to a
    /// program, not a shell command. The guard's job is to stand between the chat and
    /// your shell; once a program is attached, that program's own prompts (which you
    /// answer from the chat) are the control.
    fn type_into_app(&mut self, text: &str, who: &str, submit: bool, note: &str) -> Vec<Action> {
        self.attach.invalidate();
        let bytes = if submit {
            keys::typed_line(text, self.mirror.bracketed)
        } else {
            keys::typed_text(text, self.mirror.bracketed)
        };
        let mut acts = vec![Action::Local(self.style.inbound(who, text)), Action::Pty(bytes)];
        // Attached, the live screen shows the result — an extra "sent" line would just
        // push it up the conversation.
        if !self.attach.attached() {
            acts.push(Action::Say(note.into()));
        }
        acts
    }

    /// What is holding the terminal, for messages. The running command if we know it,
    /// else the window title (most full-screen programs set one).
    fn app_name(&self) -> String {
        self.capture.running().map(|c| c.split_whitespace().next().unwrap_or(c).to_string()).unwrap_or_else(|| {
            let t = self.title.trim();
            if t.is_empty() {
                "the program".to_string()
            } else {
                t.to_string()
            }
        })
    }

    /// Help, which reads differently depending on who is listening.
    fn help_html(&self) -> String {
        if !self.attach.attached() {
            return command::help_html(self.settings.plain_runs);
        }
        format!(
            "<b>attached to {}</b>\n\
             Anything you send is typed into it and submitted. The screen above updates \
             as it redraws.\n\n\
             <code>/keys</code> — type without pressing enter\n\
             <code>/key</code> — a key: <code>enter tab esc up down ctrl-c ctrl-r shift-tab f5</code>, or any character\n\
             <code>/cancel</code> — Ctrl-C\n\
             <code>/shot</code> — a picture of the screen, in colour\n\
             <code>/sh &lt;cmd&gt;</code> — run a shell command out-of-band\n\
             <code>/status</code> · <code>/stop</code>",
            reply::escape_html(&self.app_name())
        )
    }

    // ── attaching ────────────────────────────────────────────────────────────

    /// Fold in what the mirror currently reports. Called every loop iteration.
    pub fn observe(&mut self, m: Mirror, now: u64) -> Vec<Action> {
        self.mirror = m;
        if !self.settings.attach {
            return Vec::new();
        }
        match self.attach.observe(m.app_control, m.at_prompt, m.generation, now) {
            Some(attach::Event::Attached(why)) => {
                self.was_attached = true;
                self.frame_due = true;
                let how = match why {
                    attach::Why::AppControl => "it has taken over the terminal",
                    attach::Why::Prompt => "it is waiting at a prompt",
                };
                vec![
                    Action::Local(self.style.notice(&format!("attached to {} — the chat drives it", self.app_name()))),
                    Action::Say(format!(
                        "▶ <b>attached to {}</b> — {how}.\nSend text to type into it; buttons appear for its choices.",
                        reply::escape_html(&self.app_name())
                    )),
                ]
            }
            Some(attach::Event::Frame) => {
                self.frame_due = true;
                Vec::new()
            }
            Some(attach::Event::Detached) => {
                vec![
                    Action::Local(self.style.notice("detached — back at the shell")),
                    Action::Say("◀ <b>detached</b> — back at the shell.".into()),
                ]
            }
            None => Vec::new(),
        }
    }

    /// Whether the relay should render the screen and call [`Gate::frame`].
    pub fn take_frame(&mut self) -> bool {
        std::mem::take(&mut self.frame_due)
    }

    /// Build the live screen message from the mirror's visible grid.
    pub fn frame(&mut self, screen: &[String]) -> Vec<Action> {
        // Redacted like every other path out of this machine — a screenshot bypasses
        // the policy, but text must not.
        let lines: Vec<String> = screen
            .iter()
            .map(|l| {
                let l = self.policy.redact(l, RedactScope::Terminal);
                self.policy.redact(&l, RedactScope::Ai)
            })
            .collect();
        // One message, so the newest rows win rather than splitting across messages.
        let shown = &lines[lines.len().saturating_sub(FRAME_ROWS)..];

        let mut body = String::new();
        for l in shown {
            body.push_str(&reply::escape_html(l));
            body.push('\n');
        }
        let html = format!(
            "<b>{}</b>\n<pre>{}</pre>",
            reply::escape_html(&self.app_name()),
            body.trim_end_matches('\n')
        );

        // Buttons: the program's own choices, then the keys you always want.
        let mut kb = Keyboard::new();
        let choices = attach::choices(shown);
        for row in choices.chunks(3) {
            kb = kb.row(row.iter().cloned());
        }
        kb = kb.row([
            ("↑".to_string(), "k:up".to_string()),
            ("↓".to_string(), "k:down".to_string()),
            ("⏎".to_string(), "k:enter".to_string()),
            ("esc".to_string(), "k:esc".to_string()),
            ("^C".to_string(), "k:ctrl-c".to_string()),
            ("📷".to_string(), "shot".to_string()),
        ]);
        vec![Action::Live { html, keys: kb }]
    }

    /// A button tap. Same authorization as a typed message — a tap must never be a way
    /// in for a chat that was refused.
    pub fn on_callback(&mut self, chat_id: i64, name: &str, id: &str, data: &str, now: u64) -> Vec<Action> {
        let mut acts = vec![Action::Answer(id.to_string())];
        if !matches!(self.auth.check(chat_id, name, None), Access::Allowed) {
            return acts;
        }
        self.last_activity_ms = now;
        acts.extend(match data.strip_prefix("k:") {
            Some(key) => self.dispatch(Command::Key(key.to_string()), name, now),
            None if data == "shot" => self.dispatch(Command::Shot, name, now),
            None => Vec::new(),
        });
        acts
    }

    /// The window title, for naming an attached program we did not launch.
    pub fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
    }

    fn blocked(&self, who: &str, line: &str, reason: &str) -> Vec<Action> {
        vec![
            // Recorded locally too: a blocked attempt is exactly what the person at
            // the keyboard would want to know about.
            Action::Local(self.style.notice(&format!("blocked from {who}: {line}"))),
            Action::Say(format!("⛔ blocked by the command guard: {}", reply::escape_html(reason))),
        ]
    }

    fn submit(&mut self, line: String, who: &str, now: u64) -> Vec<Action> {
        let echo = Action::Local(self.style.inbound(who, &line));
        match self.capture.submit(line, self.mirror.app_control, now) {
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
                // While a program owns the terminal its byte stream is repaint escapes,
                // not output. The live screen is the report; a progress note here would
                // be soup.
                capture::Event::Progress { .. } if self.attach.attached() => {}
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
                    // A program we were attached to captured a session's worth of
                    // repaint escapes. Rendering that would dump thousands of lines of
                    // nonsense at the moment it exits; the exit line is the useful part.
                    if std::mem::take(&mut self.was_attached) {
                        if let Some(ev) = self.attach.release() {
                            let _ = ev;
                        }
                        acts.push(Action::Say(format!("◀ <code>{}</code> exited · {mark} · {}",
                            reply::escape_html(&cmd), human_ms(elapsed_ms))));
                        continue;
                    }
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
        if self.attach.attached() {
            let why = match self.attach.why() {
                Some(attach::Why::AppControl) => "it has taken over the terminal",
                _ => "it is waiting at a prompt",
            };
            return format!(
                "<b>attached to {}</b> — {why}.\nText you send is typed into it; the screen above updates as it redraws.\nhost <code>{}</code>",
                reply::escape_html(&self.app_name()),
                reply::escape_html(&hostname())
            );
        }
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
        Gate::new(auth, policy, Settings { plain_runs, max_reply_messages: 3, screenshot: FileKind::Document, cols: 80, attach: true })
    }

    fn paired() -> Gate {
        let mut g = gate_with(policy_with(&[], &[]), true);
        g.on_chat(7, "Mourad", "/pair 418-207", 0);
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
        let acts = g.on_chat(7, "Stranger", "rm -rf /", 0);
        assert!(acts.is_empty(), "no reply, no echo, and above all no shell write");
    }

    #[test]
    fn pairing_welcomes_and_publishes_the_peer() {
        let mut g = gate_with(policy_with(&[], &[]), true);
        let acts = g.on_chat(7, "Mourad", "/pair 418-207", 0);
        assert!(acts.iter().any(|a| matches!(a, Action::Peer(p) if p.contains("Mourad"))));
        assert!(said(&acts).contains("paired"));
        assert!(said(&acts).contains("/shot"), "the welcome doubles as help");
        assert!(pty_bytes(&acts).is_empty());
    }

    #[test]
    fn a_paired_chat_runs_a_plain_command() {
        let mut g = paired();
        let acts = g.on_chat(7, "Mourad", "git status", 100);
        assert_eq!(pty_bytes(&acts), b"git status\r");
        assert!(acts.iter().any(|a| matches!(a, Action::Local(l) if l.contains("git status"))), "echoed locally");
    }

    #[test]
    fn a_denied_command_never_reaches_the_shell() {
        // The single most important test in this module.
        let mut g = gate_with(policy_with(&["^sudo\\b"], &[]), true);
        g.on_chat(7, "M", "/pair 418207", 0);
        let acts = g.on_chat(7, "M", "sudo rm -rf /", 1);
        assert!(pty_bytes(&acts).is_empty(), "a blocked command must not be written");
        assert!(said(&acts).contains("blocked"));
        assert!(acts.iter().any(|a| matches!(a, Action::Local(l) if l.contains("blocked"))), "and it is surfaced locally");
    }

    #[test]
    fn a_confirm_command_waits_for_an_explicit_yes() {
        let mut g = gate_with(policy_with(&[], &["rm"]), true);
        g.on_chat(7, "M", "/pair 418207", 0);
        let acts = g.on_chat(7, "M", "rm -rf build", 1);
        assert!(pty_bytes(&acts).is_empty(), "nothing runs before confirmation");
        assert!(said(&acts).contains("/yes"));

        let acts = g.on_chat(7, "M", "/yes", 2);
        assert_eq!(pty_bytes(&acts), b"rm -rf build\r");
    }

    #[test]
    fn a_confirmation_can_be_declined_and_expires() {
        let mut g = gate_with(policy_with(&[], &["rm"]), true);
        g.on_chat(7, "M", "/pair 418207", 0);
        g.on_chat(7, "M", "rm -rf build", 1);
        let acts = g.on_chat(7, "M", "/no", 2);
        assert!(pty_bytes(&acts).is_empty());
        assert!(said(&acts).contains("dropped"));

        g.on_chat(7, "M", "rm -rf build", 10);
        let acts = g.on_chat(7, "M", "/yes", 10 + CONFIRM_TTL_MS + 1);
        assert!(pty_bytes(&acts).is_empty(), "a stale yes must not fire");
        assert!(said(&acts).contains("expired"));
    }

    #[test]
    fn an_unknown_slash_command_produces_help_and_no_shell_write() {
        let mut g = paired();
        let acts = g.on_chat(7, "M", "/rm -rf /", 1);
        assert!(pty_bytes(&acts).is_empty());
        assert!(said(&acts).contains("/shot"), "help was sent");
    }

    #[test]
    fn plain_text_is_inert_when_configured_that_way() {
        let mut g = gate_with(policy_with(&[], &[]), false);
        g.on_chat(7, "M", "/pair 418207", 0);
        let acts = g.on_chat(7, "M", "rm -rf /", 1);
        assert!(pty_bytes(&acts).is_empty());
        assert!(said(&acts).contains("/run"));
    }

    #[test]
    fn keys_and_named_keys_reach_the_shell_verbatim() {
        let mut g = paired();
        assert_eq!(pty_bytes(&g.on_chat(7, "M", "/keys hello", 1)), b"hello");
        assert_eq!(pty_bytes(&g.on_chat(7, "M", "/key enter", 2)), b"\r");
        assert_eq!(pty_bytes(&g.on_chat(7, "M", "/cancel", 3)), &[0x03]);
    }

    #[test]
    fn an_unknown_key_name_is_refused_rather_than_typed() {
        let mut g = paired();
        let acts = g.on_chat(7, "M", "/key destroy-everything", 1);
        assert!(pty_bytes(&acts).is_empty(), "the name must never be typed as text");
        assert!(said(&acts).contains("unknown key"));
    }


    #[test]
    fn text_becomes_stdin_while_a_command_is_waiting_for_input() {
        let mut g = paired();
        g.on_chat(7, "M", "sudo ls", 0);
        g.on_output(b"", &[Mark::Start], false, 1);
        g.on_output(b"Password:", &[], false, 2);
        g.tick(false, 2 + 8_000); // the quiet note fires: it is waiting

        let acts = g.on_chat(7, "M", "hunter2", 20_000);
        assert_eq!(pty_bytes(&acts), b"hunter2\r");
        assert!(said(&acts).contains("running command"), "and it is not treated as a new command");
    }

    #[test]
    fn a_finished_command_is_reported_with_its_output_and_status() {
        let mut g = paired();
        g.on_chat(7, "M", "ls", 0);
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
        g.on_chat(7, "M", "false", 0);
        g.on_output(b"", &[Mark::Start], false, 1);
        assert!(said(&g.on_output(b"", &[Mark::End(1)], false, 2)).contains("✗ 1"));
    }

    #[test]
    fn secrets_are_redacted_before_output_leaves_the_machine() {
        let mut p = Policy::new();
        p.add_redaction("AKIA[A-Z0-9]+", "«redacted»", RedactScope::Ai, false).unwrap();
        let mut g = gate_with(Arc::new(p), true);
        g.on_chat(7, "M", "/pair 418207", 0);
        g.on_chat(7, "M", "env", 1);
        g.on_output(b"", &[Mark::Start], false, 2);
        g.on_output(b"AWS_KEY=AKIA1234567890\r\n", &[], false, 3);
        let text = said(&g.on_output(b"", &[Mark::End(0)], false, 4));
        assert!(!text.contains("AKIA1234567890"), "a secret reached the chat: {text}");
        assert!(text.contains("redacted"), "{text}");
    }

    #[test]
    fn a_full_screen_program_answers_with_a_picture_not_empty_text() {
        let mut g = paired();
        g.on_chat(7, "M", "htop", 0);
        g.on_output(b"", &[Mark::Start], false, 1);
        g.on_output(b"\x1b[?1049h", &[], true, 2);
        let acts = g.on_output(b"", &[Mark::End(0)], false, 3);
        assert!(acts.iter().any(|a| matches!(a, Action::Shot(_))), "expected a screenshot, got {acts:?}");
    }

    #[test]
    fn full_resends_the_last_capture_as_a_file() {
        let mut g = paired();
        assert!(said(&g.on_chat(7, "M", "/full", 1)).contains("nothing captured"));
        g.on_chat(7, "M", "ls", 2);
        g.on_output(b"", &[Mark::Start], false, 3);
        g.on_output(b"a.txt\r\n", &[], false, 4);
        g.on_output(b"", &[Mark::End(0)], false, 5);
        match &g.on_chat(7, "M", "/full", 6)[0] {
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
        let acts = g.on_chat(7, "M", "/stop", 1);
        assert!(acts.iter().any(|a| matches!(a, Action::Stop(_))));
    }

    #[test]
    fn status_admits_when_completion_detection_is_only_approximate() {
        let mut g = paired();
        assert!(g.on_chat(7, "M", "/status", 1)[0] == Action::Say(g.status_html()));
        assert!(g.status_html().contains("approximate"), "a degraded session must say so");
        g.on_output(b"", &[Mark::Start], false, 2);
        g.on_output(b"", &[Mark::End(0)], false, 3);
        assert!(g.status_html().contains("exact"));
    }

    #[test]
    fn the_ai_command_is_submitted_to_the_shell() {
        let mut g = paired();
        let acts = g.on_chat(7, "M", "/ai why did the build fail", 1);
        assert_eq!(pty_bytes(&acts), b"@ai why did the build fail\r");
    }


    #[test]
    fn durations_read_naturally() {
        assert_eq!(human_ms(420), "420ms");
        assert_eq!(human_ms(1_400), "1.4s");
        assert_eq!(human_ms(95_000), "1m35s");
    }

    /// Put the gate in the state a program taking the terminal produces.
    fn attach_app(g: &mut Gate) {
        g.observe(Mirror { app_control: true, bracketed: true, app_cursor: true, generation: 1, ..Default::default() }, 0);
        assert!(g.attached());
    }

    #[test]
    fn a_program_taking_the_terminal_attaches_and_announces_it() {
        let mut g = paired();
        let acts = g.observe(Mirror { app_control: true, generation: 1, ..Default::default() }, 0);
        assert!(said(&acts).contains("attached"), "{:?}", said(&acts));
        assert!(acts.iter().any(|a| matches!(a, Action::Local(_))), "and the pane says so too");
        assert!(g.take_frame(), "the first screen goes out immediately");
    }

    #[test]
    fn while_attached_plain_text_is_typed_into_the_program_and_submitted() {
        let mut g = paired();
        attach_app(&mut g);
        let acts = g.on_chat(7, "M", "refactor the parser", 100);
        // Bracketed, because the program asked for it — so a multi-line prompt would
        // arrive as one paste rather than N submissions.
        assert_eq!(pty_bytes(&acts), b"\x1b[200~refactor the parser\x1b[201~\r");
    }

    #[test]
    fn while_attached_text_never_reaches_the_shell_machinery() {
        // The capture is for shell commands; feeding it app input would make the gate
        // think a command is running and start capturing repaint escapes.
        let mut g = paired();
        attach_app(&mut g);
        g.on_chat(7, "M", "some prompt", 100);
        assert!(g.capture().is_idle(), "no shell command was started");
    }

    #[test]
    fn while_attached_an_explicit_run_is_refused_rather_than_queued() {
        // The old behaviour queued it and fired it AFTER the program exited — a command
        // running minutes later, unattended, that nobody was watching for.
        let mut g = paired();
        attach_app(&mut g);
        let acts = g.on_chat(7, "M", "/run rm -rf build", 100);
        assert!(pty_bytes(&acts).is_empty(), "nothing may reach the shell");
        assert!(said(&acts).contains("busy"), "{:?}", said(&acts));
        assert!(said(&acts).contains("/sh"), "and it says how to run it anyway");
        assert!(g.capture().is_idle(), "and nothing is waiting to fire later");
    }

    #[test]
    fn keys_are_encoded_the_way_the_attached_program_asked() {
        let mut g = paired();
        attach_app(&mut g); // app_cursor = true
        assert_eq!(pty_bytes(&g.on_chat(7, "M", "/key up", 1)), b"\x1bOA");
        // Detached, the same key is the ordinary CSI form.
        let mut g2 = paired();
        assert_eq!(pty_bytes(&g2.on_chat(7, "M", "/key up", 1)), b"\x1b[A");
    }

    #[test]
    fn the_live_screen_carries_the_programs_own_choices_as_buttons() {
        let mut g = paired();
        attach_app(&mut g);
        let screen: Vec<String> = ["Do you want to make this edit?", "❯ 1. Yes", "  2. No"]
            .iter().map(|s| s.to_string()).collect();
        match &g.frame(&screen)[0] {
            Action::Live { html, keys } => {
                assert!(html.contains("Do you want to make this edit?"));
                let data: Vec<&str> =
                    keys.0.iter().flatten().map(|(_, d)| d.as_str()).collect();
                assert!(data.contains(&"k:1") && data.contains(&"k:2"), "{data:?}");
                // …and the keys you always want, whatever is on screen.
                assert!(data.contains(&"k:enter") && data.contains(&"k:ctrl-c") && data.contains(&"shot"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn the_live_screen_is_redacted_like_every_other_path_off_the_machine() {
        let mut p = Policy::new();
        p.add_redaction("AKIA[A-Z0-9]+", "«redacted»", RedactScope::Ai, false).unwrap();
        let mut g = gate_with(Arc::new(p), true);
        g.on_chat(7, "M", "/pair 418207", 0);
        attach_app(&mut g);
        match &g.frame(&["AWS_KEY=AKIA1234567890".to_string()])[0] {
            Action::Live { html, .. } => {
                assert!(!html.contains("AKIA1234567890"), "a secret reached the chat: {html}");
                assert!(html.contains("redacted"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_button_tap_is_acknowledged_and_acts_like_the_key_it_shows() {
        let mut g = paired();
        attach_app(&mut g);
        let acts = g.on_callback(7, "M", "cb1", "k:1", 100);
        assert!(acts.iter().any(|a| matches!(a, Action::Answer(id) if id == "cb1")), "the client must stop spinning");
        assert_eq!(pty_bytes(&acts), b"1");
    }

    #[test]
    fn a_tap_from_an_unpaired_chat_is_acknowledged_but_does_nothing() {
        // Buttons live on a message anyone in the chat can see; a tap must re-enter the
        // same authorization as a typed message.
        let mut g = gate_with(policy_with(&[], &[]), true);
        let acts = g.on_callback(99, "Stranger", "cb9", "k:ctrl-c", 1);
        assert!(pty_bytes(&acts).is_empty(), "no key may reach the terminal");
        assert!(acts.iter().any(|a| matches!(a, Action::Answer(_))));
    }

    #[test]
    fn an_attached_program_exiting_reports_its_status_not_its_repaint_soup() {
        let mut g = paired();
        g.on_chat(7, "M", "vim notes.md", 0);
        g.on_output(b"", &[Mark::Start], false, 1);
        attach_app(&mut g);
        g.on_output(b"\x1b[?1049h\x1b[2J\x1b[Hlots of repaint escapes", &[], true, 2);
        let acts = g.on_output(b"", &[Mark::End(0)], false, 3_000);
        let text = said(&acts);
        assert!(text.contains("exited"), "{text}");
        assert!(!text.contains("repaint escapes"), "the capture must not be dumped: {text}");
    }

    #[test]
    fn help_explains_the_program_when_one_is_attached() {
        let mut g = paired();
        assert!(said(&g.on_chat(7, "M", "/help", 1)).contains("/run"), "the shell menu when detached");
        attach_app(&mut g);
        let h = said(&g.on_chat(7, "M", "/help", 2));
        assert!(h.contains("typed into it"), "{h}");
        assert!(h.contains("/keys"));
    }

    #[test]
    fn attaching_can_be_turned_off_entirely() {
        let auth = Auth::new(true, Vec::new(), 0, "418207".into());
        let mut g = Gate::new(auth, policy_with(&[], &[]), Settings {
            plain_runs: true, max_reply_messages: 3, screenshot: FileKind::Document, cols: 80, attach: false,
        });
        g.on_chat(7, "M", "/pair 418207", 0);
        assert!(g.observe(Mirror { app_control: true, generation: 1, ..Default::default() }, 0).is_empty());
        assert!(!g.attached(), "[gates] attach = false keeps the old shell-only behaviour");
    }

    #[test]
    fn a_command_arriving_mid_typing_is_queued_not_spliced() {
        // The corruption case: dispatching here would splice `ls` into `git comm`.
        let mut g = paired();
        g.on_local(b"git comm");
        let acts = g.on_chat(7, "M", "ls", 1);
        assert!(pty_bytes(&acts).is_empty(), "splicing would run a command neither party asked for");
        assert!(said(&acts).contains("queued"));
    }
}
