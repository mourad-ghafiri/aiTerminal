//! The world a scenario plays in.
//!
//! This is a real [`Gate`], a real mirror [`Term`], and the relay loop's **exact
//! ordering** — mirror the bytes, observe, then frame. It calls the same
//! `mirror_of`/`at_prompt` the product calls, so a scenario cannot pass against a
//! convenient reimplementation of the sequencing.
//!
//! What it deliberately does not have is a PTY or a process. Bytes "written to the
//! terminal" land in a `Vec<u8>`; nothing is ever executed. That is what makes it safe
//! to write a scenario about `rm -rf /` — the string is asserted never to reach the
//! buffer, and there is no mechanism by which it could run.

use std::sync::Arc;

use platform::term::Term;

use super::super::world::{self, World};
use super::gate_step::Step;
use corelib::wire::Toml;

use crate::gate::auth::Auth;
use crate::gate::driver::{Action, Gate, Settings};
use crate::gate::marks::MarkScanner;
use crate::gate::telegram::api::FileKind;
use crate::guard::Guard;

/// The relay's tick, so timers in the product fire on the same cadence here.
const TICK_MS: u64 = 30;
/// The pairing code every scenario uses.
pub const CODE: &str = "418207";
/// The chat a scenario is paired with unless it says otherwise.
pub const PEER: i64 = 7;

/// One thing the gate sent outward, with where it went.
#[derive(Clone, Debug)]
pub struct Said {
    pub chat_id: i64,
    pub html: String,
}

pub struct GateWorld {
    gate: Gate,
    term: Term,
    scanner: MarkScanner,
    now: u64,
    /// Bytes that reached the terminal, cleared by each `expect_pty`.
    pty: Vec<u8>,
    /// Messages sent, cleared by each expectation that reads them.
    said: Vec<Said>,
    local: String,
    /// Callback values on the most recent live screen.
    buttons: Vec<String>,
    /// The live message id, mirroring the relay's `live_id` atomic.
    live_id: i64,
    /// The most recent frame was a fresh message rather than an edit.
    live_reposted: bool,
    next_msg_id: i64,
    /// When the user last did something, for the frame-latency expectation.
    last_input_ms: u64,
    last_frame_ms: Option<u64>,
}

/// Everything a gate scenario can configure before the first step.
struct Setup {
    paired: bool,
    allow: Vec<String>,
    plain_runs: bool,
    attach: bool,
    deny: Vec<String>,
    confirm: Vec<String>,
    redact: Vec<String>,
    cols: u16,
}

impl Setup {
    fn read(t: &Toml) -> Setup {
        let flag = |k: &str, d: bool| world::flag(t, k).unwrap_or(d);
        let list = |k: &str| world::list(t, k).unwrap_or_default();
        Setup {
            paired: flag("paired", false),
            plain_runs: flag("plain_text_runs", true),
            attach: flag("attach", true),
            allow: list("allow"),
            deny: list("deny"),
            confirm: list("confirm"),
            redact: list("redact"),
            cols: world::int(t, "cols").unwrap_or(80).clamp(20, 400) as u16,
        }
    }
}

/// Build the gate world for a scenario folder.
pub fn build(setup: &Toml) -> Result<Box<dyn World>, String> {
    Ok(Box::new(GateWorld::new(&Setup::read(setup))))
}

impl World for GateWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        let step = Step::parse(step)?;
        self.run(&step)
    }
}

impl GateWorld {
    fn new(setup: &Setup) -> GateWorld {
        // The scenario's own rules, written in the guard's vocabulary — one parser, so a
        // gate journey and a config file cannot disagree about what a rule means.
        let mut doc = String::new();
        for (pattern, rule) in setup.deny.iter().map(|d| (d, "deny")).chain(setup.confirm.iter().map(|c| (c, "confirm"))) {
            doc.push_str(&format!("[[guard.command]]\npattern = \"{pattern}\"\nrule = \"{rule}\"\n"));
        }
        for pattern in &setup.redact {
            doc.push_str(&format!("[[guard.secret]]\npattern = \"{pattern}\"\n"));
        }
        let guard = Guard::from_toml(&doc);
        let auth = Auth::new(true, setup.allow.clone(), 0, CODE.to_string());
        let gate = Gate::new(
            auth,
            Arc::new(guard),
            Settings {
                plain_runs: setup.plain_runs,
                max_reply_messages: 3,
                screenshot: FileKind::Document,
                cols: setup.cols,
                attach: setup.attach,
            },
        );
        let mut w = GateWorld {
            gate,
            term: Term::with_scrollback(setup.cols, 24, 500),
            scanner: MarkScanner::new(),
            now: 0,
            pty: Vec::new(),
            said: Vec::new(),
            local: String::new(),
            buttons: Vec::new(),
            live_id: 0,
            live_reposted: false,
            next_msg_id: 5000,
            last_input_ms: 0,
            last_frame_ms: None,
        };
        if setup.paired {
            let acts = w.gate.on_chat(PEER, "Mourad", &format!("/pair {CODE}"), w.now);
            w.absorb(acts);
            w.said.clear();
            w.local.clear();
        }
        w
    }

    // ── the relay loop, reproduced ───────────────────────────────────────────

    /// One iteration of the real loop.
    fn pump(&mut self) {
        let now = self.now;
        self.gate.set_title(self.term.title());
        let mirror = crate::gate::mirror_of(&self.term, &self.gate, now);
        let acts = self.gate.observe(mirror, now);
        self.absorb(acts);

        let acts = self.gate.tick(now);
        self.absorb(acts);

        if self.gate.take_frame() {
            let screen = self.term.screen_text();
            let acts = self.gate.frame(&screen);
            self.absorb(acts);
        }
    }

    /// Advance the clock in the relay's own increments, so debounces and timeouts fire
    /// exactly as they would in the product.
    fn advance(&mut self, ms: u64) {
        let target = self.now + ms;
        while self.now < target {
            self.now = (self.now + TICK_MS).min(target);
            self.pump();
        }
    }

    /// Feed bytes as the shell printed them: marks out, mirror first, then the gate —
    /// the ordering invariant the relay depends on.
    fn shell_output(&mut self, bytes: &[u8]) {
        let (mut clean, mut marks) = (Vec::new(), Vec::new());
        self.scanner.feed(bytes, &mut clean, &mut marks);
        self.term.feed(&clean);
        let acts = self.gate.on_output(&clean, &marks, self.now);
        self.absorb(acts);
        self.pump();
    }

    /// Perform the actions, the way `Perform` does.
    fn absorb(&mut self, acts: Vec<Action>) {
        // Where a reply goes: the authorized chat, exactly as the relay resolves it.
        let peer = self.gate.peer_chat_id().unwrap_or(0);
        for act in acts {
            match act {
                Action::Pty(b) => self.pty.extend_from_slice(&b),
                Action::Say(html) => {
                    self.said.push(Said { chat_id: peer, html });
                    // Anything the gate posts pushes the live screen up the chat.
                    self.live_id = 0;
                }
                Action::SayTo(chat_id, html) => self.said.push(Said { chat_id, html }),
                Action::Local(t) => self.local.push_str(&t),
                Action::Live { html, keys } => {
                    self.live_reposted |= self.live_id == 0;
                    if self.live_id == 0 {
                        self.next_msg_id += 1;
                        self.live_id = self.next_msg_id;
                    }
                    self.buttons = keys.0.iter().flatten().map(|(_, d)| d.clone()).collect();
                    self.said.push(Said { chat_id: peer, html });
                    self.last_frame_ms = Some(self.now);
                }
                Action::Shot(_) | Action::File { .. } => {
                    self.live_id = 0; // a new message buries the live screen
                }
                Action::Peer(_) | Action::Answer(_) | Action::Sh(_) | Action::Stop(_) => {}
            }
        }
    }

    // ── running a scenario ───────────────────────────────────────────────────

    fn run(&mut self, step: &Step) -> Result<(), String> {
        match step {
            Step::Chat { text, from } => {
                self.last_input_ms = self.now;
                self.live_reposted = false;
                let id = from.unwrap_or(PEER);
                // A typed message lands below the live screen; the relay reposts.
                self.live_id = 0;
                let acts = self.gate.on_chat(id, "Mourad", text, self.now);
                self.absorb(acts);
                self.pump();
            }
            Step::Tap { data, from } => {
                self.last_input_ms = self.now;
                self.live_reposted = false;
                let id = from.unwrap_or(PEER);
                let acts = self.gate.on_callback(id, "Mourad", "cb1", data, self.now);
                self.absorb(acts);
                self.pump();
            }
            Step::Pty(s) => self.shell_output(s.as_bytes()),
            Step::Screen(lines) => {
                let mut out = String::from("\u{1b}[2J\u{1b}[H");
                out.push_str(&lines.join("\r\n"));
                self.shell_output(out.as_bytes());
            }
            Step::Local(s) => {
                self.gate.on_local(s.as_bytes());
                self.pump();
            }
            Step::RunLocal(cmd) => {
                // Typed at the keyboard, submitted, then the shell reports it started.
                self.gate.on_local(format!("{cmd}\r").as_bytes());
                self.shell_output(b"\x1b[?2004l\x1b[?1l\x1b]1339;S\x07");
                self.shell_output(format!("{cmd}\r\n").as_bytes());
            }
            // A real shell disarms its line editor before handing the terminal to a
            // command — that is why bracketed paste at a prompt says nothing about
            // whether a program is running.
            Step::ShellStart => self.shell_output(b"\x1b[?2004l\x1b[?1l\x1b]1339;S\x07"),
            Step::ShellEnd(n) => self.shell_output(format!("\x1b]1339;E;{n}\x07").as_bytes()),
            Step::ShellPrompt => {
                // What zsh and bash actually emit at every prompt.
                self.shell_output(b"\x1b[?2004h\x1b[?1h")
            }
            Step::AppModes(modes) => {
                let mut seq = String::new();
                for m in modes {
                    seq.push_str(match m.as_str() {
                        "alt" => "\u{1b}[?1049h",
                        "mouse" => "\u{1b}[?1000;1006h",
                        "bracketed" => "\u{1b}[?2004h",
                        "app_cursor" => "\u{1b}[?1h",
                        other => return Err(format!("unknown app mode {other:?}")),
                    });
                }
                self.shell_output(seq.as_bytes());
            }
            Step::AppRelease => self.shell_output(b"\x1b[?1000l\x1b[?1l\x1b[?2004l\x1b[?1049l"),
            Step::Wait(ms) => self.advance(*ms),

            Step::ExpectSays(want) => {
                let all = self.said_text();
                for w in want {
                    if !all.contains(w.as_str()) {
                        return Err(format!("expected {w:?} in what was said; got {}", world::show(&all)));
                    }
                }
                self.said.clear();
            }
            Step::ExpectNotSays(bad) => {
                let all = self.said_text();
                for b in bad {
                    if all.contains(b.as_str()) {
                        return Err(format!("{b:?} must NOT have been said; got {}", world::show(&all)));
                    }
                }
            }
            Step::ExpectPty(want) => {
                let got = String::from_utf8_lossy(&self.pty).into_owned();
                if got != *want {
                    return Err(format!("terminal received {} — expected {}", world::show(&got), world::show(want)));
                }
                self.pty.clear();
            }
            Step::ExpectNoPty => {
                if !self.pty.is_empty() {
                    let got = String::from_utf8_lossy(&self.pty).into_owned();
                    return Err(format!("nothing should have reached the terminal, but {} did", world::show(&got)));
                }
            }
            Step::ExpectAttached(want) => {
                if self.gate.attached() != *want {
                    return Err(format!(
                        "expected attached = {want}, but it is {}",
                        self.gate.attached()
                    ));
                }
            }
            Step::ExpectButtons(want) => {
                for w in want {
                    if !self.buttons.iter().any(|b| b == w) {
                        return Err(format!("expected a {w:?} button; the live screen offers {:?}", self.buttons));
                    }
                }
            }
            Step::ExpectLocal(want) => {
                for w in want {
                    if !self.local.contains(w.as_str()) {
                        return Err(format!("expected {w:?} in the pane; got {}", world::show(&self.local)));
                    }
                }
                self.local.clear();
            }
            Step::ExpectChatId(want) => {
                for s in &self.said {
                    if s.chat_id != *want {
                        return Err(format!(
                            "a message went to chat {} instead of {want}: {}",
                            s.chat_id,
                            world::show(&s.html)
                        ));
                    }
                }
                if self.said.is_empty() {
                    return Err("nothing was said, so there is no recipient to check".into());
                }
                self.said.clear();
            }
            Step::ExpectFrameWithin(ms) => {
                let deadline = self.last_input_ms + ms;
                while self.now < deadline && !self.framed_since_input() {
                    self.advance(TICK_MS);
                }
                if !self.framed_since_input() {
                    return Err(format!(
                        "no live frame within {ms}ms of the last action — a tap that shows nothing reads as broken"
                    ));
                }
            }
            Step::ExpectLiveReposted(want) => {
                let got = self.live_reposted;
                if got != *want {
                    return Err(format!(
                        "expected the live screen to be {}, but it was {}",
                        if *want { "re-posted" } else { "edited in place" },
                        if got { "re-posted" } else { "edited in place" }
                    ));
                }
            }
            Step::ExpectNothingQueued => {
                if !self.gate.capture().is_idle() {
                    return Err(format!(
                        "something is waiting to run later: {:?} ({} queued)",
                        self.gate.capture().running(),
                        self.gate.capture().queued()
                    ));
                }
            }
        }
        Ok(())
    }

    fn framed_since_input(&self) -> bool {
        self.last_frame_ms.is_some_and(|f| f >= self.last_input_ms)
    }

    fn said_text(&self) -> String {
        self.said.iter().map(|s| s.html.as_str()).collect::<Vec<_>>().join("\n")
    }
}

