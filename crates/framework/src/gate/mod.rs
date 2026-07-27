//! `@gate` — hand this pane to a chat app and drive the terminal from anywhere.
//!
//! `@gate telegram start` spawns your shell inside the pane and relays it. You keep
//! typing locally; a **paired** chat drives the same shell — same cwd, same history,
//! same running program. Every remote action prints a dim line in the pane, so nothing
//! happens invisibly.
//!
//! The pieces, roughly in the order a byte meets them:
//!
//! | module | job |
//! |---|---|
//! | [`session`] | the PTY, the mirror terminal, and the write-everything-twice invariant |
//! | [`marks`] | pull the shell's OSC 1339 command marks out of the stream |
//! | [`capture`] | decide when a remote command finished, and hold its output |
//! | [`driver`] | authorize, guard, and turn events into actions — the whole policy |
//! | [`reply`] / [`shot`] | render the answer as text or as a picture |
//! | [`telegram`] | long-poll in, paced messages out |
//! | [`auth`] | the pairing handshake |
//! | [`record`] | the on-disk trace, so another pane can stop this one |
//! | [`chrome`] | the local one-liners, and putting the terminal back |
//!
//! **This is remote code execution over a chat app.** It is off until `[gates] enabled`
//! is set, nothing is accepted until a chat sends the code printed in the pane, and
//! every command passes the same `[security]` guard as an AI suggestion. See
//! `docs/gate.md`, which also states plainly what that guard cannot do.

pub mod auth;
pub mod capture;
pub mod chrome;
pub mod command;
pub mod driver;
pub mod marks;
pub mod record;
pub mod reply;
pub mod session;
pub mod shot;
pub mod telegram;

use std::io::{IsTerminal, Read};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;

use crate::config::{Config, GateSpec};
use auth::Auth;
use capture::{Clock, SystemClock};
use chrome::{Chrome, Style};
use driver::{Action, Gate, Settings};
use marks::MarkScanner;
use record::GateRecord;
use session::{GateSession, PaneSink, Sink};
use telegram::api::{ApiError, BotApi, FileKind};
use telegram::{CurlBotApi, PollStep, Poller};

/// The main loop's tick. Also the ceiling on how long a resize can go unnoticed.
const TICK_MS: u64 = 30;
/// How often to re-read our own record to see whether another pane asked us to stop.
const RECORD_POLL_MS: u64 = 500;
/// Queued PTY bytes above which the reader parks. Without this, `cat /dev/urandom`
/// would grow the channel until the process died.
const MAX_QUEUED_BYTES: usize = 4 << 20;

/// Channels with an adapter today. Discord is next: the `BotApi` seam and the generic
/// `[gates.<channel>]` config parser are already in place for it.
const AVAILABLE: &[&str] = &["telegram"];

/// What the relay loop is reacting to.
enum Ev {
    Local(Vec<u8>),
    Pty(Vec<u8>),
    PtyEof,
    Chat { chat_id: i64, name: String, text: String },
    Note(String),
    Fatal(String),
}

/// Something to send, handled off the main thread so a slow upload never stalls the
/// relay.
enum Out {
    Text(String),
    File { kind: FileKind, name: String, mime: String, bytes: Vec<u8>, caption: Option<String> },
}

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("--help") | Some("-h") | Some("help") => {
            println!("{}", usage());
            0
        }
        None => {
            eprintln!("{}", usage());
            2
        }
        Some("status") | Some("list") => status(),
        Some("stop") => stop(args.get(1).map(String::as_str)),
        Some(channel) => match args.get(1).map(String::as_str) {
            Some("start") => start(channel),
            Some("stop") => stop(Some(channel)),
            Some(other) => {
                eprintln!("@gate: unknown action '{other}'\n{}", usage());
                2
            }
            None => {
                eprintln!("@gate: say what to do — `@gate {channel} start`\n{}", usage());
                2
            }
        },
    }
}

fn usage() -> String {
    format!(
        "@gate — drive this terminal from a chat app\n\
         \n\
         @gate telegram start     hand this pane to Telegram (you keep using it too)\n\
         @gate telegram stop      stop it — from this pane or any other\n\
         @gate status             what is running right now\n\
         \n\
         Channels available: {}. Configure under [gates] in {}.\n\
         Setup walkthrough: docs/gate.md",
        AVAILABLE.join(", "),
        Config::path().display()
    )
}

/// `@gate status`
fn status() -> i32 {
    let gates = record::list();
    if gates.is_empty() {
        println!("no gate is running");
        return 0;
    }
    for g in gates {
        let peer = if g.peer.is_empty() { "waiting to pair".to_string() } else { g.peer.clone() };
        println!("{}  {}  pid {}  ·  {peer}", g.channel, g.id, g.pid);
    }
    0
}

/// `@gate stop [channel]`
fn stop(channel: Option<&str>) -> i32 {
    let gates: Vec<_> = record::list()
        .into_iter()
        .filter(|g| channel.map(|c| g.channel.eq_ignore_ascii_case(c)).unwrap_or(true))
        .collect();
    if gates.is_empty() {
        eprintln!("@gate: no matching gate is running");
        return 1;
    }
    for g in &gates {
        // Flag the record; the gate notices within half a second and shuts down
        // through its own guards. Signalling would skip them and leave the pane in
        // raw mode with no echo.
        record::request_stop(&g.id);
        println!("stopping {} gate ({})", g.channel, g.id);
    }
    0
}

/// Resolve `token`, which may be the secret itself or `$VAR` / `${VAR}`.
fn resolve_token(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let Some(rest) = raw.strip_prefix('$') else {
        return (!raw.is_empty()).then(|| raw.to_string());
    };
    let name = rest.strip_prefix('{').and_then(|r| r.strip_suffix('}')).unwrap_or(rest).trim();
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The variable a user should export, for the setup hint.
fn token_env_name(raw: &str) -> Option<&str> {
    let rest = raw.trim().strip_prefix('$')?;
    let name = rest.strip_prefix('{').and_then(|r| r.strip_suffix('}')).unwrap_or(rest).trim();
    (!name.is_empty()).then_some(name)
}

/// The setup walkthrough, shown whenever a gate cannot start.
fn setup_hint(channel: &str, why: &str) -> String {
    format!(
        "@gate: {why}\n\n\
         To set up the {channel} gate:\n\
         1. Message @BotFather on Telegram and send /newbot — it replies with a token.\n\
         2. Export it:  export TELEGRAM_BOT_TOKEN='123456:AA…'\n\
         3. In {}:\n\
         \x20     [gates]\n\
         \x20     enabled = true\n\
         \x20     [gates.telegram]\n\
         \x20     token = \"$TELEGRAM_BOT_TOKEN\"\n\
         4. Run `@gate {channel} start`, then send the pairing code to your bot.\n\n\
         Full walkthrough: docs/gate.md",
        Config::path().display()
    )
}

/// Everything that must be true before a shell is handed to a chat.
fn preflight(channel: &str, cfg: &Config) -> Result<GateSpec, String> {
    if !AVAILABLE.contains(&channel) {
        return Err(format!("@gate: '{channel}' is not available yet — today: {}", AVAILABLE.join(", ")));
    }
    if !cfg.gates_enabled {
        return Err(setup_hint(channel, "gates are off ([gates] enabled = false)"));
    }
    let Some(spec) = cfg.gates.iter().find(|g| g.channel == channel).cloned() else {
        return Err(setup_hint(channel, &format!("no [gates.{channel}] section is configured")));
    };
    if resolve_token(&spec.token).is_none() {
        let why = match token_env_name(&spec.token) {
            Some(var) => format!("${var} is not set (it holds the {channel} bot token)"),
            None => format!("no token is set for {channel}"),
        };
        return Err(setup_hint(channel, &why));
    }
    // Pairing off with nobody pre-authorized would mean "anyone who finds the bot
    // owns this machine". Refuse rather than quietly serving strangers.
    if !cfg.gates_require_pairing && spec.allow.is_empty() {
        return Err(format!(
            "@gate: refusing to start — [gates] require_pairing = false with an empty \
             [gates.{channel}] allow list would let ANY chat drive this terminal.\n\
             Either set require_pairing = true, or list the chat ids you trust."
        ));
    }
    Ok(spec)
}

/// `@gate <channel> start`
fn start(channel: &str) -> i32 {
    let cfg = Config::load();
    let spec = match preflight(channel, &cfg) {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    // `terminal_size` reads fd 2, and the relay owns fd 0 and fd 1 — all three must
    // really be a terminal, or the session would be subtly broken rather than absent.
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() || !std::io::stderr().is_terminal() {
        eprintln!("@gate: needs an interactive terminal (run it in a tab or split, not through a pipe).");
        return 2;
    }

    let token = resolve_token(&spec.token).unwrap_or_default();
    let api: Arc<dyn BotApi> = Arc::new(CurlBotApi::new(&token));

    // Check the token before taking over the pane, so a typo is a one-line error
    // rather than a shell that dies a moment after it appears.
    let bot = match api.whoami() {
        Ok(name) => name,
        Err(ApiError::Unauthorized) => {
            eprintln!("{}", setup_hint(channel, "the bot token was rejected by telegram"));
            return 2;
        }
        Err(e) => {
            eprintln!("@gate: cannot reach {channel}: {e}");
            return 1;
        }
    };
    let _ = api.set_commands(command::MENU);

    // Acknowledge anything already queued. Without this, starting a gate would replay
    // every command sent while it was off.
    let mut poller = Poller::default();
    let mut nap = |ms: u64| std::thread::sleep(std::time::Duration::from_millis(ms));
    let discarded = match poller.prime(&*api, &mut nap) {
        Ok(n) => n,
        Err(e) => {
            eprintln!(
                "@gate: could not establish the message offset ({e}).\n\
                 Refusing to start — it could replay commands sent while the gate was off."
            );
            return 1;
        }
    };
    if discarded > 0 {
        println!("  ignored {discarded} message(s) sent while the gate was off");
    }

    match relay(channel, &cfg, spec, api, poller, &bot) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("@gate: {e}");
            1
        }
    }
}

/// The gate proper: guards, threads, and the loop.
fn relay(
    channel: &str,
    cfg: &Config,
    spec: GateSpec,
    api: Arc<dyn BotApi>,
    poller: Poller,
    bot: &str,
) -> std::io::Result<i32> {
    let clock = SystemClock::default();
    let style = Style::default();
    let (cols, rows) = platform::os::terminal_size().unwrap_or((80, 24));

    // Guards drop in reverse declaration order: the shell is torn down first, then the
    // terminal is restored, then raw mode, and the record disappears last.
    let mut rec = GateRecord::create(channel)?;
    let Some(_raw) = platform::os::raw_mode() else {
        return Err(std::io::Error::other("could not enter raw mode"));
    };
    let chrome = Chrome::enter();
    let registry = crate::plugin::load_registry(cfg);
    let session = GateSession::spawn(cfg, &registry, cols, rows)?;

    let policy = Arc::new(crate::security::build_policy(cfg, &registry));
    let pre_authorized = !spec.allow.is_empty();
    let auth = Auth::new(cfg.gates_require_pairing, spec.allow.clone(), clock.now_ms(), auth::new_code());
    let code = auth.display_code();
    let mut gate = Gate::new(
        auth,
        policy,
        Settings {
            plain_runs: cfg.gates_plain_text == "run",
            max_reply_messages: cfg.gates_max_reply_messages,
            screenshot: FileKind::parse(&cfg.gates_screenshot),
            cols,
        },
    );

    let mut sink = PaneSink { term: platform::term::Term::with_scrollback(cols, rows, 2_000) };
    sink.emit(style.banner(channel, bot, (!pre_authorized).then_some(code.as_str())).as_bytes());

    let (tx, rx) = mpsc::channel::<Ev>();
    let queued = Arc::new(AtomicUsize::new(0));
    let stopping = Arc::new(AtomicBool::new(false));
    let peer_id = Arc::new(std::sync::atomic::AtomicI64::new(0));

    spawn_stdin_reader(tx.clone());
    spawn_pty_reader(session.pty(), tx.clone(), queued.clone());
    spawn_poller(api.clone(), poller, tx.clone(), stopping.clone());
    let out_tx = spawn_sender(api.clone(), stopping.clone(), peer_id.clone());

    let mut scanner = MarkScanner::new();
    let mut glyphs = corelib::gfx::text::GlyphCache::new(platform::os::text_shaper_with(&cfg.font_family));
    let theme = Config::resolve_theme(&cfg.theme);
    let shot_kind = FileKind::parse(&cfg.gates_screenshot);
    let idle_limit_ms = cfg.gates_idle_minutes.saturating_mul(60_000);

    let mut ctx = Perform {
        session: &session,
        out: &out_tx,
        rec: &mut rec,
        glyphs: &mut glyphs,
        theme: &theme,
        shot_kind,
        style: &style,
        peer_id: &peer_id,
        reason: String::from("the shell exited"),
        stop: false,
    };

    let mut record_checked = 0u64;
    let (mut cur_cols, mut cur_rows) = (cols, rows);
    let sigwinch = platform::os::sigwinch_flag();

    'main: loop {
        let now = clock.now_ms();

        if sigwinch.swap(false, Ordering::Relaxed) {
            if let Some((c, r)) = platform::os::terminal_size() {
                if (c, r) != (cur_cols, cur_rows) {
                    (cur_cols, cur_rows) = (c, r);
                    ctx.session.resize_to(c, r);
                    sink.term.resize(c, r);
                    gate.set_cols(c);
                }
            }
        }

        if now.saturating_sub(record_checked) >= RECORD_POLL_MS {
            record_checked = now;
            if ctx.rec.stop_requested() {
                ctx.reason = "stopped from another pane".into();
                break;
            }
            if idle_limit_ms > 0
                && gate.last_activity_ms > 0
                && now.saturating_sub(gate.last_activity_ms) > idle_limit_ms
            {
                ctx.reason = "idle timeout".into();
                break;
            }
            if let Some(fresh) = gate.auth_mut().tick(now, auth::new_code) {
                sink.emit(style.notice(&format!("new pairing code — send /pair {fresh}")).as_bytes());
            }
        }

        let alt = sink.term.in_alt_screen();
        chrome.set_alt(alt);
        let acts = gate.tick(alt, now);
        ctx.perform(acts, &mut sink);
        if ctx.stop {
            break;
        }

        match rx.recv_timeout(std::time::Duration::from_millis(TICK_MS)) {
            Ok(ev) => {
                let mut batch = vec![ev];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                for ev in batch {
                    let now = clock.now_ms();
                    let alt = sink.term.in_alt_screen();
                    let acts = match ev {
                        Ev::Local(bytes) => {
                            ctx.session.write(&bytes);
                            gate.on_local(&bytes);
                            Vec::new()
                        }
                        Ev::Pty(bytes) => {
                            let credit = queued.load(Ordering::Relaxed);
                            queued.fetch_sub(bytes.len().min(credit), Ordering::Relaxed);
                            let (mut clean, mut found) = (Vec::with_capacity(bytes.len()), Vec::new());
                            scanner.feed(&bytes, &mut clean, &mut found);
                            // Mirror first, so `in_alt_screen` is current for this chunk.
                            sink.emit(&clean);
                            gate.on_output(&clean, &found, sink.term.in_alt_screen(), now)
                        }
                        Ev::PtyEof => break 'main,
                        Ev::Chat { chat_id, name, text } => {
                            // The sender needs a destination even for a refusal.
                            let _ = peer_id.compare_exchange(0, chat_id, Ordering::Relaxed, Ordering::Relaxed);
                            gate.on_chat(chat_id, &name, &text, alt, now)
                        }
                        Ev::Note(msg) => {
                            sink.emit(style.notice(&msg).as_bytes());
                            Vec::new()
                        }
                        Ev::Fatal(msg) => {
                            // A dead link must not kill the shell someone is working in.
                            sink.emit(style.notice(&format!("{msg} — the shell keeps running")).as_bytes());
                            Vec::new()
                        }
                    };
                    ctx.perform(acts, &mut sink);
                    if ctx.stop {
                        break 'main;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    stopping.store(true, Ordering::Relaxed);
    api.shutdown();
    sink.emit(style.farewell(&ctx.reason).as_bytes());
    Ok(0)
}

/// The side-effect executor: everything an [`Action`] needs to touch.
struct Perform<'a> {
    session: &'a GateSession,
    out: &'a mpsc::Sender<Out>,
    rec: &'a mut GateRecord,
    glyphs: &'a mut corelib::gfx::text::GlyphCache,
    theme: &'a corelib::theme::Theme,
    shot_kind: FileKind,
    style: &'a Style,
    peer_id: &'a Arc<std::sync::atomic::AtomicI64>,
    reason: String,
    stop: bool,
}

impl Perform<'_> {
    fn perform(&mut self, acts: Vec<Action>, sink: &mut PaneSink) {
        for act in acts {
            match act {
                Action::Pty(bytes) => self.session.write(&bytes),
                Action::Local(text) => sink.emit(text.as_bytes()),
                Action::Say(html) => {
                    let _ = self.out.send(Out::Text(html));
                }
                Action::Peer(peer) => {
                    if let Some(id) = peer.rsplit_once('(').and_then(|(_, r)| r.trim_end_matches(')').parse().ok()) {
                        self.peer_id.store(id, Ordering::Relaxed);
                    }
                    self.rec.set_peer(&peer);
                }
                Action::Shot(note) => {
                    // Rendering stays on this thread: the text shaper is not `Send`. It
                    // costs a few milliseconds; the upload is the slow part, and that
                    // happens on the sender thread.
                    let shot = shot::capture(&sink.term, self.theme, self.glyphs);
                    let mut caption = shot::caption(&shot, &sink.term);
                    if !note.is_empty() {
                        caption = format!("{note}\n{caption}");
                    }
                    sink.emit(
                        self.style.outbound(&format!("sent a screenshot ({} KB)", shot.png.len() / 1024)).as_bytes(),
                    );
                    let _ = self.out.send(Out::File {
                        kind: self.shot_kind,
                        name: "terminal.png".into(),
                        mime: "image/png".into(),
                        bytes: shot.png,
                        caption: Some(caption),
                    });
                }
                Action::File { name, text, caption } => {
                    let _ = self.out.send(Out::File {
                        kind: FileKind::Document,
                        name,
                        mime: "text/plain".into(),
                        bytes: text.into_bytes(),
                        caption: Some(caption),
                    });
                }
                Action::Sh(cmd) => {
                    // Out-of-band: its own shell, so it works even while a full-screen
                    // program owns the shared one.
                    let text = run_detached(&cmd);
                    let lines: Vec<String> = text.lines().map(str::to_string).collect();
                    for m in reply::format(&format!("$ {cmd}"), &lines, 3).messages {
                        let _ = self.out.send(Out::Text(m));
                    }
                }
                Action::Stop(why) => {
                    self.reason = why;
                    self.stop = true;
                    return;
                }
            }
        }
    }
}

/// `/sh` — run a command in its own shell, bounded, and return its combined output.
fn run_detached(cmd: &str) -> String {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut c = std::process::Command::new(shell);
    c.arg("-lc").arg(cmd);
    match crate::procio::run_bounded(c, std::time::Duration::from_secs(60), 64 * 1024) {
        Ok(b) => {
            let mut s = b.stdout;
            if !b.stderr.trim().is_empty() {
                s.push_str(&b.stderr);
            }
            s
        }
        Err(e) => format!("failed: {e}"),
    }
}

// ── worker threads ───────────────────────────────────────────────────────────

/// Local keystrokes. This thread is **never joined**: it is parked in a blocking
/// `read` on fd 0 with no safe way to interrupt it (`framework` forbids `unsafe`, so
/// no `pthread_kill`, and closing fd 0 races). All guards drop when `relay` returns
/// and the process exits, which reaps it. Do not "fix" this into a join — it hangs.
fn spawn_stdin_reader(tx: mpsc::Sender<Ev>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut stdin = std::io::stdin();
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Ev::Local(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                // A window resize interrupts whichever thread catches the signal —
                // SIGWINCH is installed without SA_RESTART on purpose. Treating that
                // as fatal would kill the relay on the first resize.
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}

/// Shell output, with a byte credit so a runaway producer cannot outrun the loop.
fn spawn_pty_reader(pty: Arc<dyn platform::traits::Pty>, tx: mpsc::Sender<Ev>, queued: Arc<AtomicUsize>) {
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 65536];
        loop {
            while queued.load(Ordering::Relaxed) > MAX_QUEUED_BYTES {
                // Not reading the master makes the shell block on write — exactly the
                // back-pressure a terminal is supposed to apply.
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            match pty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    queued.fetch_add(n, Ordering::Relaxed);
                    if tx.send(Ev::Pty(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = tx.send(Ev::PtyEof);
    });
}

/// The long-poll worker.
fn spawn_poller(api: Arc<dyn BotApi>, mut poller: Poller, tx: mpsc::Sender<Ev>, stopping: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !stopping.load(Ordering::Relaxed) {
            match poller.poll(&*api) {
                PollStep::Updates(updates) => {
                    for u in updates {
                        if tx.send(Ev::Chat { chat_id: u.chat_id, name: u.from_name, text: u.text }).is_err() {
                            return;
                        }
                    }
                }
                PollStep::Wait(ms) => {
                    // Sleep in slices so a stop is noticed promptly.
                    let mut left = ms;
                    while left > 0 && !stopping.load(Ordering::Relaxed) {
                        let slice = left.min(200);
                        std::thread::sleep(std::time::Duration::from_millis(slice));
                        left -= slice;
                    }
                }
                PollStep::Down(msg) => {
                    if tx.send(Ev::Note(msg)).is_err() {
                        return;
                    }
                }
                PollStep::Stop(msg) => {
                    if !msg.is_empty() {
                        let _ = tx.send(Ev::Fatal(msg));
                    }
                    return;
                }
            }
        }
    });
}

/// Outbound messages, paced to stay under the per-chat rate limit.
fn spawn_sender(
    api: Arc<dyn BotApi>,
    stopping: Arc<AtomicBool>,
    peer_id: Arc<std::sync::atomic::AtomicI64>,
) -> mpsc::Sender<Out> {
    let (tx, rx) = mpsc::channel::<Out>();
    std::thread::spawn(move || {
        let clock = SystemClock::default();
        let mut last: Option<u64> = None;
        while let Ok(msg) = rx.recv() {
            if stopping.load(Ordering::Relaxed) {
                continue;
            }
            let chat_id = peer_id.load(Ordering::Relaxed);
            if chat_id == 0 {
                continue; // nobody to answer yet
            }
            let wait = telegram::pace_ms(last, clock.now_ms());
            if wait > 0 {
                std::thread::sleep(std::time::Duration::from_millis(wait));
            }
            let sent = match &msg {
                Out::Text(html) => api.send_message(chat_id, html),
                Out::File { kind, name, mime, bytes, caption } => {
                    api.send_file(chat_id, *kind, name, mime, bytes, caption.as_deref())
                }
            };
            // A message the API refuses to format is still worth delivering — resend it
            // once as plain text rather than losing it silently.
            if let (Err(ApiError::Request { .. }), Out::Text(html)) = (&sent, &msg) {
                let _ = api.send_message(chat_id, &strip_tags(html));
            }
            last = Some(clock.now_ms());
        }
    });
    tx
}

/// Drop HTML tags — the fallback when a formatted message is rejected.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(toml: &str) -> Config {
        Config::from_toml(toml)
    }

    #[test]
    fn a_token_is_read_literally_or_from_the_environment() {
        assert_eq!(resolve_token("123:ABC").as_deref(), Some("123:ABC"));
        std::env::set_var("TT_GATE_TEST_TOKEN", "from-env");
        assert_eq!(resolve_token("$TT_GATE_TEST_TOKEN").as_deref(), Some("from-env"));
        assert_eq!(resolve_token("${TT_GATE_TEST_TOKEN}").as_deref(), Some("from-env"));
        std::env::remove_var("TT_GATE_TEST_TOKEN");
        assert_eq!(resolve_token("$TT_GATE_TEST_TOKEN"), None, "an unset variable is not a token");
        assert_eq!(resolve_token("  "), None);
    }

    #[test]
    fn preflight_refuses_when_gates_are_off_and_says_how_to_turn_them_on() {
        let c = cfg_with("[gates]\nenabled = false\n[gates.telegram]\ntoken = \"x\"\n");
        let msg = preflight("telegram", &c).unwrap_err();
        assert!(msg.contains("enabled = false"), "{msg}");
        assert!(msg.contains("BotFather"), "the refusal doubles as the setup guide");
    }

    #[test]
    fn preflight_refuses_an_unconfigured_channel() {
        let c = cfg_with("[gates]\nenabled = true\n");
        assert!(preflight("telegram", &c).unwrap_err().contains("no [gates.telegram]"));
    }

    #[test]
    fn preflight_names_the_environment_variable_the_user_actually_wrote() {
        let c = cfg_with("[gates]\nenabled = true\n[gates.telegram]\ntoken = \"$MY_OWN_BOT_TOKEN\"\n");
        let msg = preflight("telegram", &c).unwrap_err();
        assert!(msg.contains("$MY_OWN_BOT_TOKEN"), "{msg}");
    }

    #[test]
    fn preflight_refuses_a_configuration_that_would_serve_strangers() {
        // pairing off + nobody allowed = anyone who finds the bot owns the machine.
        let c = cfg_with("[gates]\nenabled = true\nrequire_pairing = false\n[gates.telegram]\ntoken = \"t\"\n");
        let msg = preflight("telegram", &c).unwrap_err();
        assert!(msg.contains("refusing to start"), "{msg}");
        assert!(msg.contains("ANY chat"), "{msg}");
    }

    #[test]
    fn preflight_accepts_a_complete_configuration() {
        let c = cfg_with("[gates]\nenabled = true\n[gates.telegram]\ntoken = \"123:ABC\"\n");
        assert_eq!(preflight("telegram", &c).unwrap().token, "123:ABC");
    }

    #[test]
    fn an_unimplemented_channel_is_named_rather_than_silently_ignored() {
        let c = cfg_with("[gates]\nenabled = true\n[gates.discord]\ntoken = \"t\"\n");
        let msg = preflight("discord", &c).unwrap_err();
        assert!(msg.contains("not available yet"), "{msg}");
        assert!(msg.contains("telegram"), "and it says what IS available");
    }

    #[test]
    fn usage_lists_the_verbs_and_where_configuration_lives() {
        let u = usage();
        for part in ["telegram start", "stop", "status", "[gates]", "docs/gate.md"] {
            assert!(u.contains(part), "usage is missing {part}");
        }
    }

    #[test]
    fn tag_stripping_recovers_the_text_of_a_rejected_message() {
        assert_eq!(strip_tags("<pre>a &lt; b</pre>"), "a < b");
        assert_eq!(strip_tags("<b>x</b> plain"), "x plain");
    }
}
