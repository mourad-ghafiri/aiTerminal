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
use crate::guard::{Act, Decision, Guard};

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
    /// A message for the **authorized** chat (already HTML).
    Say(String),
    /// A message for one specific chat — a refusal, which by definition goes to
    /// someone who is not the authorized peer. Addressing these to the peer would
    /// deliver "wrong code" to the owner and silence to the person who typed it.
    SayTo(i64, String),
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
    /// What the terminal reports about who is driving it.
    pub signals: attach::Signals,
    /// The cursor looks parked at a REPL prompt.
    pub at_prompt: bool,
    /// `Term::generation()` — the change counter frames are debounced on.
    pub generation: u64,
}

impl Mirror {
    /// A program, not the shell, is driving the terminal.
    pub fn owns(&self) -> bool {
        self.signals.owns_terminal()
    }
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
    guard: Arc<Guard>,
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
    pub fn new(auth: Auth, guard: Arc<Guard>, settings: Settings) -> Gate {
        Gate {
            auth,
            capture: Capture::new(),
            guard,
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

    /// The chat authorized to drive this terminal, if one is.
    ///
    /// This is the **only** thing that decides where output goes. Deriving the
    /// destination from "whoever messaged the bot first" would let a stranger who
    /// found the bot receive the owner's command output.
    pub fn peer_chat_id(&self) -> Option<i64> {
        self.auth.paired().map(|p| p.chat_id)
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
            Access::Refused(msg) => vec![Action::SayTo(chat_id, reply::escape_html(&msg))],
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
            // While a program owns the terminal there is no shell command to run, so
            // pointing at `/run` would be a closed loop: `/run` is itself refused.
            Command::Ignored(t) if self.attach.attached() => vec![Action::Say(format!(
                "not sent — <code>[gates] plain_text</code> is off. Use <code>/keys {}</code> to type it into {}.",
                reply::escape_html(&t),
                reply::escape_html(&self.app_name())
            ))],
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
            Command::Key(name) => match keys::key_bytes(&name, self.mirror.signals.app_cursor) {
                Some(bytes) => {
                    self.attach.invalidate();
                    vec![Action::Local(self.style.inbound(who, &format!("key {name}"))), Action::Pty(bytes)]
                }
                // Never fall back to typing the name as text — `/key rm -rf` must not
                // become input at the prompt.
                None => vec![Action::Say(format!("unknown key <code>{}</code> — see /help", reply::escape_html(&name)))],
            },
            Command::Keys(text) => self.type_into_app(&text, who, false, "typed — /key enter to submit"),
            Command::Sh(c) => match self.guard.judge(Act::Run(&c)) {
                Decision::Deny { reason } => self.blocked(who, &c, &reason),
                _ => match self.arriving(&c) {
                    Err(why) => vec![Action::Say(reply::escape_html(&why))],
                    Ok(c) => vec![Action::Local(self.style.inbound(who, &format!("sh {c}"))), Action::Sh(c)],
                },
            },
            // `/ai` builds a shell line, so it carries exactly the same hazard as
            // `/run`: queued now, it fires unattended when the program exits.
            Command::Ai(_) if self.attach.attached() => vec![self.busy_with_the_program("/ai")],
            Command::Ai(prompt) => self.run_line(format!("@ai {prompt}"), who, now),
            Command::Yes if self.attach.attached() => vec![self.busy_with_the_program("/yes")],
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
        match self.guard.judge(Act::Run(&line)) {
            Decision::Deny { reason } => self.blocked(who, &line, &reason),
            Decision::Confirm { reason } => {
                self.pending = Some((line.clone(), now));
                vec![Action::Say(format!(
                    "⚠ <b>{}</b>\n<pre>{}</pre>\n/yes to run · /no to drop",
                    reply::escape_html(&reason),
                    reply::escape_html(&line)
                ))]
            }
            Decision::Allow => self.submit(line, who, now),
        }
    }

    /// Send text to whatever owns the terminal, encoded the way it asked for.
    ///
    /// This deliberately does **not** go through the command guard: it is input to a
    /// program, not a shell command. The guard's job is to stand between the chat and
    /// your shell; once a program is attached, that program's own prompts (which you
    /// answer from the chat) are the control.
    fn type_into_app(&mut self, text: &str, who: &str, submit: bool, note: &str) -> Vec<Action> {
        // Typing into a program is the terminal too — a password answered from a phone
        // reaches a REPL exactly the way a command reaches the shell, so the placeholders
        // become values here as well.
        let text = &match self.arriving(text) {
            Ok(t) => t,
            Err(why) => return vec![Action::Say(reply::escape_html(&why))],
        };
        self.attach.invalidate();
        let bytes = if submit {
            keys::typed_line(text, self.mirror.signals.bracketed)
        } else {
            keys::typed_text(text, self.mirror.signals.bracketed)
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
        match self.attach.observe(m.owns(), m.at_prompt, m.generation, now) {
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
                // Everything scoped to that attachment goes with it. Leaving
                // `was_attached` latched would make the NEXT command's `Finished` look
                // like the program's exit, and its output would be silently dropped.
                self.was_attached = false;
                self.title.clear();
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
        // the guard, but text must not.
        let lines: Vec<String> = screen
            .iter()
            .map(|l| {
                self.leaving(l)
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

    /// The refusal every shell-bound verb shares while a program holds the terminal.
    /// Refusing beats queuing: a queued command fires unattended minutes later, when
    /// the program exits and nobody is watching for it.
    fn busy_with_the_program(&self, verb: &str) -> Action {
        Action::Say(format!(
            "<code>{verb}</code> needs the shell, and <b>{}</b> has it — \
             <code>/sh &lt;cmd&gt;</code> runs out-of-band, or <code>/key ctrl-c</code> interrupts.",
            reply::escape_html(&self.app_name())
        ))
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
        // The placeholders become values HERE, at the edge of the terminal — after the
        // guard has judged the line and before anything types it. A placeholder the vault
        // does not know is refused rather than typed: `«db-password-1»` reaching a database
        // as literal text is a failure nobody could explain from the other end.
        let line = match self.arriving(&line) {
            Ok(l) => l,
            Err(why) => return vec![Action::Say(reply::escape_html(&why))],
        };
        let echo = Action::Local(self.style.inbound(who, &line));
        match self.capture.submit(line, self.mirror.owns(), now) {
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

    pub fn on_output(&mut self, chunk: &[u8], marks: &[Mark], now: u64) -> Vec<Action> {
        // The SAME condition the submit was gated on. Releasing a queue on a weaker one
        // (the alternate screen alone) would dispatch into an inline program.
        let owns = self.mirror.owns();
        self.capture.on_output(chunk, marks, owns, now);
        self.drain(now)
    }

    pub fn on_local(&mut self, bytes: &[u8]) {
        self.capture.on_local(bytes);
    }

    pub fn tick(&mut self, now: u64) -> Vec<Action> {
        let owns = self.mirror.owns();
        self.capture.tick(owns, now);
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
                        self.attach.release();
                        self.title.clear();
                        // `/full` must still work: keep the capture even though the
                        // rendered form is repaint escapes rather than output.
                        self.last = Some((header.clone(), self.lines(&bytes)));
                        acts.push(Action::Say(format!(
                            "◀ <code>{}</code> exited · {mark} · {}",
                            reply::escape_html(&cmd),
                            human_ms(elapsed_ms)
                        )));
                        continue;
                    }
                    let _ = saw_alt;
                    let lines = self.lines(&bytes);
                    self.last = Some((header.clone(), lines.clone()));
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

    /// Render captured bytes to plain text, with nothing secret left in it.
    fn lines(&self, bytes: &[u8]) -> Vec<String> {
        reply::to_lines(bytes, self.settings.cols, CAPTURE_LINES).into_iter().map(|l| self.leaving(&l)).collect()
    }

    /// A line on its way to the chat.
    ///
    /// A chat app is off this machine, so BOTH kinds of rule apply: someone who scoped a
    /// secret to `terminal` or to `ai` certainly meant "not to my phone" as well. What
    /// leaves is either a hard mask or a placeholder — and a placeholder is the useful
    /// one, because a command your phone sends back carrying it runs here with the real
    /// value in it. Your phone can use a password it never sees.
    fn leaving(&self, line: &str) -> String {
        self.guard.mask(&self.guard.hide(line))
    }

    /// A line on its way from the chat to this terminal — the moment the placeholders in
    /// it stop being placeholders.
    fn arriving(&self, line: &str) -> Result<String, String> {
        self.guard.vault().restore(line)
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
mod tests;
