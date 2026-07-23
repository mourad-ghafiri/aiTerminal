//! The interactive GUI front-end — a light terminal window: tabs + splits of PTY
//! panes, the tab quick-switcher, the plugin status bar, and per-profile workspace
//! persistence. All AI lives behind the `@ai` / `@<agent>` shell integration (the
//! `aiTerminal ai` CLI), so the window itself stays a pure terminal.

// The GUI front-end is split across sibling submodules; they reach this module's
// items through `use super::*`, so the shared imports, types, constants, and
// helper fns are `pub(crate)`.
mod action;
mod boot;
mod focus;
mod frame;
mod handlers;
mod input;
mod link;
mod mouse;
mod panes;
pub(crate) mod persist;
pub mod render;
mod setup;
mod switcher;
mod termlink;
mod workspace;

pub(crate) use boot::{build_keymap, start_status_worker};
pub use switcher::render_switcher_proof;
use switcher::{draw_switcher, SwitcherEntry, TabSwitcher};

pub(crate) use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
pub(crate) use std::sync::{Arc, Mutex};
pub(crate) use std::thread;
pub(crate) use std::time::{Duration, Instant};

pub(crate) use corelib::gfx::text::GlyphCache;
pub(crate) use corelib::gfx::{Canvas, Surface};
pub(crate) use corelib::types::{
    Event, KeyCode, Modifiers, MouseButton, Point, PtyCommand, Rect, ScrollDelta,
};
pub(crate) use corelib::types::Chord;
pub(crate) use platform::traits::{EventHandler, Gpu, Pty, Window};
pub(crate) use crate::keymap::Keymap;

pub(crate) use action::Action;
pub(crate) use panes::{Axis, Dir, PaneId, Tabs};
pub(crate) use platform::term::{Pos, Selection, SelectionMode, Term};
pub(crate) use corelib::theme::Theme;
pub(crate) use setup::PaneFactory;

pub(crate) use crate::config::Config;
pub(crate) use render::{
    render_pane, render_status_bar, render_tab_bar_side, render_tab_bar_top, status_bar_height,
    tab_bar_height, CursorStyle, TabInfo, PAD, SIDE_TAB_W,
};

pub(crate) const MULTI_CLICK_MS: u128 = 400;
pub(crate) const ZOOM_STEP: f32 = 1.1;
/// The terminal pane's tab/switcher icon.
pub(crate) const TERMINAL_ICON: &str = "\u{1F5A5}";

/// The frame-dirty flag + event-loop waker, shared by every producer (PTY readers,
/// the status worker, input handlers). `set()` marks the frame dirty and — only on
/// the clean→dirty edge — wakes the (possibly idle-blocked) OS event loop, so a
/// flooding producer posts at most one wake per consumed frame.
#[derive(Clone)]
pub(crate) struct DirtyFlag {
    flag: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl DirtyFlag {
    /// The production flag: wakes the OS event loop. Starts DIRTY so the first
    /// frame always renders.
    pub(crate) fn new() -> Self {
        Self::with_waker(Arc::new(platform::os::post_wake_event))
    }
    /// A flag with a custom waker (tests count wakes through this).
    pub(crate) fn with_waker(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        DirtyFlag { flag: Arc::new(AtomicBool::new(true)), wake }
    }
    /// Mark the frame dirty; wake the event loop on the clean→dirty edge only.
    pub(crate) fn set(&self) {
        if !self.flag.swap(true, SeqCst) {
            (self.wake)();
        }
    }
    /// Consume the flag for this frame: returns whether a render is due.
    pub(crate) fn take(&self) -> bool {
        self.flag.swap(false, SeqCst)
    }
}

/// Launch the interactive window (tabs, splits, the user's login shell) and run
/// the OS event loop. Never returns — owns the window + the `GuiApp` event handler
/// internally, so the binary calls this single function for the interactive path.
pub fn run(config: Config) -> ! {
    // Start the diagnostic logger before anything else runs in the interactive path,
    // so boot + every later subsystem can log. The level + retention come from config
    // (`[logging]`, default error). Pure render/CLI tooling never calls this, so it
    // doesn't pay the logger (or trigger config bootstrap) just to draw one frame.
    platform::log::init(Config::logs_dir(), platform::log::Level::parse(&config.log_level), config.log_retention_days);
    platform::info!("{} starting (log level {})", corelib::brand::NAME, config.log_level);
    let app = GuiApp::new(config);
    // The window title is the brand name (the `WindowConfig` default already uses it);
    // the size restores from the active profile's saved workspace, so the window
    // reopens exactly as it was left.
    let mut cfg = corelib::types::WindowConfig::default();
    if let Some((w, h)) = workspace::saved_window(&crate::profile::active_id()) {
        cfg.logical_size = corelib::types::Size::new(w, h);
    }
    platform::os::boot().run(cfg, Box::new(app));
}

/// Install a global panic hook that LOGS every panic (thread + payload + source location)
/// to stderr and an appendable `~/.aiTerminal/crash.log`. The event-loop resilience
/// boundaries (`platform::os::macos::window::guarded`) catch a panic and drop the frame, so
/// the app survives — this hook makes that recovery DIAGNOSABLE instead of silent. It is
/// allocation-light and never locks app state (safe to run during unwind).
pub fn install_panic_hook() {
    use std::io::Write;
    std::panic::set_hook(Box::new(|info| {
        let name = std::thread::current().name().unwrap_or("<unnamed>").to_string();
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic>".to_string());
        let line = format!("[panic] thread '{name}' at {loc}: {msg}\n");
        let _ = std::io::stderr().write_all(line.as_bytes());
        append_crash_line(&Config::crash_log(), &line);
        // Also route the panic into the diagnostic log (if the logger is up), then flush
        // so the record survives even if the process is about to die.
        platform::error!("panic in thread '{name}' at {loc}: {msg}");
        platform::log::flush();
    }));
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TabBarPos {
    Top,
    Bottom,
    Left,
    Right,
}

impl TabBarPos {
    fn next(self) -> Self {
        match self {
            TabBarPos::Top => TabBarPos::Bottom,
            TabBarPos::Bottom => TabBarPos::Left,
            TabBarPos::Left => TabBarPos::Right,
            TabBarPos::Right => TabBarPos::Top,
        }
    }
    fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "bottom" => TabBarPos::Bottom,
            "left" | "vertical" | "vertical-left" | "v" => TabBarPos::Left,
            "right" | "vertical-right" => TabBarPos::Right,
            // "top" | "horizontal" | "h" | anything else
            _ => TabBarPos::Top,
        }
    }
    /// Top/Bottom strips lay tabs along **x** (drag reorders horizontally); Left/Right
    /// strips lay them along **y**. Drives the drag's drop-slot axis.
    pub(crate) fn horizontal(self) -> bool {
        matches!(self, TabBarPos::Top | TabBarPos::Bottom)
    }
    /// The canonical name persisted into a profile's workspace.
    pub(crate) fn name(self) -> &'static str {
        match self {
            TabBarPos::Top => "top",
            TabBarPos::Bottom => "bottom",
            TabBarPos::Left => "left",
            TabBarPos::Right => "right",
        }
    }
}

/// An in-progress tab-strip drag (reorder). `from` is the grabbed tab; `gap` is the live
/// insertion slot (`0..=len`, in *visual* order including the grabbed tab) recomputed from the
/// tab rects as the pointer moves; `moved` flips once the pointer passes a small threshold, so a
/// click that doesn't move just focuses. The renderer reads it to draw the floating pill +
/// insertion bar; release commits it via [`Tabs::move_tab`].
pub(crate) struct TabDrag {
    pub from: usize,
    pub grab: Point,
    pub cursor: Point,
    pub moved: bool,
    pub gap: usize,
}

pub(crate) struct Session {
    pty: Arc<dyn Pty>,
    term: Arc<Mutex<Term>>,
    cols: u16,
    rows: u16,
    selection: Option<Selection>,
    shell_name: String,
    /// Set by the reader thread when the shell process ends (EOF / error) — the host
    /// reaps the pane so `exit` closes the tab instead of leaving a frozen terminal.
    exited: Arc<AtomicBool>,
}

impl Session {
    fn spawn(
        dirty: &DirtyFlag,
        shell: &str,
        policy: Arc<crate::security::Policy>,
        integ: crate::shell::ShellSpawn,
        scrollback: usize,
        cwd: Option<&str>,
        restore: Option<&str>,
    ) -> std::io::Result<Session> {
        // An interactive login shell: argv[0]=`-<name>`, cwd=$HOME (or an explicit `cwd`
        // when restoring a saved workspace), TERM exported — so the window works correctly
        // even when launched from the desktop (Dock). Shell integration (aliases / file
        // colors / prompt) rides in via env+args.
        let cmd = PtyCommand {
            program: shell.to_string(),
            args: integ.args,
            cols: 80,
            rows: 24,
            login: integ.login,
            env: integ.env,
            cwd: cwd.map(str::to_string),
        };
        let pty: Arc<dyn Pty> = Arc::from(platform::os::spawn_pty(&cmd)?);
        let term = Arc::new(Mutex::new(Term::with_scrollback(80, 24, scrollback)));
        // Replay the saved session CONTENT (with its ANSI styling) into the buffer
        // BEFORE the reader thread starts — the restored pane silently shows exactly
        // what was on screen, colors included, with the fresh prompt right below.
        if let Some(text) = restore.filter(|t| !t.trim().is_empty()) {
            let mut t = term.lock().unwrap_or_else(|e| e.into_inner());
            for line in text.lines() {
                t.feed(line.as_bytes());
                t.feed(b"\r\n");
            }
        }
        let exited = Arc::new(AtomicBool::new(false));
        {
            let (pty, term, dirty, exited) = (pty.clone(), term.clone(), dirty.clone(), exited.clone());
            // Only redact when terminal-scope rules exist, so the default path
            // stays a raw byte feed (no lossy UTF-8 round-trip, zero overhead).
            let redact = policy.has_scope(crate::security::RedactScope::Terminal);
            thread::spawn(move || {
                // 64 KiB reads: a fast producer needs 8× fewer lock acquisitions
                // (and wakes) than the old 8 KiB buffer for the same throughput.
                let mut buf = vec![0u8; 65536];
                loop {
                    match pty.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            // Redact BEFORE taking the term lock — the regex pass must
                            // never extend the window the render thread waits on.
                            let redacted =
                                redact.then(|| redact_terminal(&String::from_utf8_lossy(&buf[..n]), &policy));
                            // Poison-tolerant lock + parser isolation: a panic on one byte
                            // chunk (a terminal-emulator edge case) is caught + logged by the
                            // panic hook and skipped — the reader keeps this PTY alive instead
                            // of dying or aborting the app. The render side is bounds-safe.
                            let mut guard = term.lock().unwrap_or_else(|e| e.into_inner());
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                match &redacted {
                                    Some(s) => guard.feed(s.as_bytes()),
                                    None => guard.feed(&buf[..n]),
                                }
                            }));
                            drop(guard);
                            dirty.set();
                        }
                        Err(_) => break,
                    }
                }
                // The shell process ended (`exit`, EOF, or a read error): flag the session so
                // the host reaps its pane next frame (closing the tab/split cleanly).
                exited.store(true, SeqCst);
                dirty.set();
            });
        }
        let base =
            if shell.trim().is_empty() { std::env::var("SHELL").unwrap_or_default() } else { shell.to_string() };
        let shell_name = base
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| "shell".into());
        Ok(Session { pty, term, cols: 80, rows: 24, selection: None, shell_name, exited })
    }

    /// Whether the shell process has ended (so the host can reap this pane).
    fn exited(&self) -> bool {
        self.exited.load(SeqCst)
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            self.term.lock().unwrap_or_else(|e| e.into_inner()).resize(cols, rows);
            let _ = self.pty.resize(cols, rows);
        }
    }

    fn write(&self, bytes: &[u8]) {
        let _ = self.pty.write(bytes);
    }

    /// The shell-reported working directory `(host, path)` from OSC 7, if any. Drives the
    /// status bar instantly (and, over SSH, with the remote folder + host).
    fn cwd(&self) -> Option<(String, String)> {
        self.term
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cwd()
            .map(|(h, p)| (h.to_string(), p.to_string()))
    }

    /// Monotonic counter bumped whenever the reported cwd changes (cheap `cd` detection).
    fn cwd_seq(&self) -> u64 {
        self.term.lock().unwrap_or_else(|e| e.into_inner()).cwd_seq()
    }

    /// The terminal's content generation (bumped per feed/resize) — one lock + one
    /// load, the cheap "did anything change?" probe for per-frame consumers.
    fn generation(&self) -> u64 {
        self.term.lock().unwrap_or_else(|e| e.into_inner()).generation()
    }

    /// The buffer's styled content (scrollback tail + screen, ANSI escapes intact)
    /// for the workspace snapshot — what a restored pane silently replays.
    fn content_ansi(&self, max_lines: usize, strip_bg: Option<(u8, u8, u8)>) -> Vec<String> {
        self.term.lock().unwrap_or_else(|e| e.into_inner()).content_ansi(max_lines, strip_bg)
    }

    fn title(&self) -> String {
        let t = self.term.lock().unwrap_or_else(|e| e.into_inner()).title().to_string();
        if !t.trim().is_empty() {
            return t; // a program (vim / ssh / …) set its own title — keep it
        }
        // No program title — name the open folder + the shell. With the tab's index +
        // icon prefix this reads e.g. "3 - 🖥 Terminal [the-terminal][zsh]".
        let word = crate::i18n::translate("term.title", &[]);
        let shell = self.shell_name.trim_start_matches('-');
        match self.cwd().and_then(|(_host, path)| folder_label(&path)) {
            Some(folder) => format!("{word} [{folder}][{shell}]"),
            None => format!("{word} [{shell}]"),
        }
    }
}

/// The display folder name for a tab — the **basename** (last path component) of `path`,
/// with `~` / `/` kept as-is. `None` for an empty path. So `/Users/me/proj` → `proj`.
fn folder_label(path: &str) -> Option<String> {
    let p = path.trim();
    if p.is_empty() {
        return None;
    }
    if p == "~" || p == "/" {
        return Some(p.to_string());
    }
    std::path::Path::new(p)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| Some(p.to_string()))
}

/// What the status worker needs about the focused pane: its PTY pid (for the `lsof`
/// fallback) and its shell-reported `(host, path)` from OSC 7 (the instant path).
#[derive(Default, Clone)]
pub(crate) struct FocusState {
    pub pid: i32,
    pub cwd: Option<(String, String)>,
}

/// Shared focus state plus a `Condvar` the worker blocks on — the main thread pulses it
/// on every focus/cwd change so the status recomputes immediately (Observer pattern).
pub(crate) type FocusSignal = Arc<(Mutex<FocusState>, std::sync::Condvar)>;

/// A scroll command for the focused pane. `Lines(n)`/`Page(d)` use n>0/d>0 = toward
/// the bottom (content moves down).
#[derive(Clone, Copy)]
enum ScrollCmd {
    Lines(i32),
    Page(i32),
    Top,
    Bottom,
}

/// A pane: a font-zoom level plus its terminal session.
pub(crate) struct Pane {
    zoom: f32,
    session: Session,
}

impl Pane {
    fn terminal(s: Session, zoom: f32) -> Pane {
        Pane { zoom, session: s }
    }
    /// The pane NAME only (no icon): the program/`Terminal [shell]` title. The
    /// renderer composes `index - icon name`.
    fn title(&self) -> String {
        self.session.title()
    }
}

pub struct GuiApp {
    tabs: Tabs<Pane>,
    /// Builds terminal panes (shell integration + policy + zoom baked in).
    factory: PaneFactory,
    keymap: Keymap<Action>,
    dirty: DirtyFlag,
    theme: Theme,
    scale: f64,
    base_pt: f32,
    cache: Option<GlyphCache>,
    surface: Option<Surface>,
    win_px: (u32, u32),
    layout: Vec<(PaneId, Rect)>,
    panes_area: Rect,
    tab_bar: TabBarPos,
    /// Clickable tab rects paired with their 0-based tab index (the strip may be scrolled,
    /// so the rect's position is not its index).
    tab_rects: Vec<(usize, Rect)>,
    dragging: Option<PaneId>,
    /// An active tab-strip reorder drag (`None` when not dragging a tab). See [`TabDrag`].
    tab_drag: Option<TabDrag>,
    /// The last terminal click `(when, which pane, which cell)` — multi-click
    /// escalation (char→word→line) requires the *same pane* and cell within
    /// `MULTI_CLICK_MS`, so a quick click in another pane can't inherit the count.
    last_click: Option<(Instant, PaneId, Pos)>,
    click_count: u32,
    status: Arc<Mutex<crate::plugin::StatusLine>>,
    /// Shared focus state + a `Condvar` the status worker waits on — switching tab/pane (or
    /// a `cd`) snapshots the focused pid + OSC-7 cwd here and wakes the worker, so the top
    /// bar updates within milliseconds instead of on the next 1 s poll.
    focus: FocusSignal,
    /// The focused session's last-seen `cwd_seq`, so an in-session `cd` is detected per frame.
    last_cwd_seq: u64,
    /// The active profile's id — compared (throttled) against the on-disk pointer so a
    /// `aiTerminal profile switch` from any shell applies to this window live.
    active_profile: String,
    /// The active profile's `(emoji, name)`, shown as a status-bar chip.
    profile_chip: (String, String),
    /// Last unix-time the active-profile pointer / config files were polled (throttle).
    last_profile_check: u64,
    /// Mtime stamp of the effective config files (global + active overlay) at the
    /// last apply — a moved stamp means `@theme` / a hand edit landed; reload live.
    config_stamp: u64,
    /// Set when the active profile's saved workspace is out of date (a tab/pane change);
    /// a debounced autosave in the frame loop flushes it to `profiles/<id>/workspace.toml`.
    workspace_dirty: bool,
    /// Unix time of the last workspace autosave (throttles writes).
    last_workspace_save: u64,
    /// The panes' summed content stamp at the last save — the periodic autosave
    /// skips its content dump + disk write while this is unchanged.
    last_saved_content: u64,
    /// The chrome stamp (status bar, tab strip, theme, layout) of the last FULL
    /// frame — unchanged chrome enables the incremental pane-only render path.
    frame_chrome: u64,
    /// Per-pane content stamps at their last render (generation, scroll,
    /// selection, hover, focus, zoom) — an unmoved stamp skips the pane redraw.
    pane_stamps: std::collections::HashMap<PaneId, u64>,
    config: Config,
    default_zoom: f32,
    /// The security policy (command guard + redaction), from config + plugins.
    policy: Arc<crate::security::Policy>,
    /// The tab quick-switcher overlay (Cmd+P / Cmd+K), if open.
    switcher: TabSwitcher,
    /// The last redacted session context written to `Config::session_context_path()`
    /// for `@ai`/agents — cached so we only rewrite the file when the focused terminal
    /// actually changed (never per-frame).
    session_ctx: String,
    /// The focused terminal's `generation()` at the last session-context build — the
    /// cheap gate that keeps the build (grid scan + redaction) off clean frames.
    session_ctx_gen: u64,
    /// When the session context was last built (throttles bursty output to ~2 Hz).
    session_ctx_at: Instant,
    /// The live config shared with the status worker: `(generation, Config)` —
    /// `apply_config` bumps the generation; the worker rebuilds its plugin
    /// registry when it moves.
    shared_config: Arc<Mutex<(u64, Config)>>,
    /// The terminal link under the pointer while ⌘ is held — `(pane, display-row, col0, col1)`
    /// — so `render_grid` underlines it as a "⌘-click to open" cue. `None` otherwise.
    link_hover: Option<(PaneId, u16, u16, u16)>,
}

/// Append one line to the crash log, rotating at 1 MiB (rename to `.log.1` +
/// fresh file). Allocation-light and lock-free — safe to run mid-unwind from the
/// panic hook — and bounded: a panic loop (e.g. one bad byte sequence per PTY
/// chunk) can never grow the log without limit.
fn append_crash_line(path: &std::path::Path, line: &str) {
    use std::io::Write;
    if std::fs::metadata(path).is_ok_and(|m| m.len() > 1024 * 1024) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Redact terminal output, applying rules only to printable text runs and never
/// to ANSI escape sequences (so colours/cursor moves are never corrupted).
pub(crate) fn redact_terminal(text: &str, policy: &crate::security::Policy) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut run = String::new();
    let mut i = 0;
    let flush = |run: &mut String, out: &mut String| {
        if !run.is_empty() {
            out.push_str(&policy.redact(run, crate::security::RedactScope::Terminal));
            run.clear();
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\u{1b}' {
            flush(&mut run, &mut out);
            out.push(c);
            i += 1;
            match chars.get(i) {
                Some('[') => {
                    // CSI: parameters until a final byte 0x40..=0x7E
                    out.push('[');
                    i += 1;
                    while i < chars.len() {
                        let p = chars[i];
                        out.push(p);
                        i += 1;
                        if ('@'..='~').contains(&p) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: until BEL or ESC '\'
                    out.push(']');
                    i += 1;
                    while i < chars.len() {
                        let p = chars[i];
                        out.push(p);
                        i += 1;
                        if p == '\u{07}' {
                            break;
                        }
                        if p == '\u{1b}' {
                            if chars.get(i) == Some(&'\\') {
                                out.push('\\');
                                i += 1;
                            }
                            break;
                        }
                    }
                }
                Some(&other) => {
                    out.push(other);
                    i += 1;
                }
                None => {}
            }
        } else {
            run.push(c);
            i += 1;
        }
    }
    flush(&mut run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_log_rotates_at_its_cap_instead_of_growing_forever() {
        let dir = std::env::temp_dir().join(format!("tt-crashlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("crash.log");
        std::fs::write(&log, "x".repeat(2 * 1024 * 1024)).unwrap();
        append_crash_line(&log, "[panic] boom\n");
        assert!(std::fs::metadata(&log).unwrap().len() < 1024, "fresh file after rotation");
        assert!(log.with_extension("log.1").exists(), "the old log is kept aside");
        // Under the cap → plain append, no rotation.
        append_crash_line(&log, "[panic] again\n");
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("boom") && text.contains("again"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dirty_flag_coalesces_wakes_to_the_clean_to_dirty_edge() {
        use std::sync::atomic::AtomicUsize;
        let wakes = Arc::new(AtomicUsize::new(0));
        let flag = {
            let wakes = wakes.clone();
            DirtyFlag::with_waker(Arc::new(move || {
                wakes.fetch_add(1, SeqCst);
            }))
        };
        // The flag starts dirty (first frame always renders) — a flooding producer
        // must not wake the loop again until the frame is consumed.
        flag.set();
        flag.set();
        assert_eq!(wakes.load(SeqCst), 0, "already dirty → no wake");
        assert!(flag.take(), "the initial dirty state renders");
        assert!(!flag.take(), "consumed");
        flag.set();
        flag.set();
        flag.set();
        assert_eq!(wakes.load(SeqCst), 1, "one wake per clean→dirty edge, not per set");
        assert!(flag.take());
        flag.set();
        assert_eq!(wakes.load(SeqCst), 2, "a fresh edge wakes again");
    }

    #[test]
    fn folder_label_is_the_basename() {
        assert_eq!(folder_label("/Users/me/testclaude").as_deref(), Some("testclaude"));
        assert_eq!(folder_label("/Users/me/My Project").as_deref(), Some("My Project"));
        assert_eq!(folder_label("/a/b/proj/").as_deref(), Some("proj"), "a trailing slash is ignored");
        assert_eq!(folder_label("~/مجلد").as_deref(), Some("مجلد"), "non-ASCII basename");
        assert_eq!(folder_label("~").as_deref(), Some("~"), "home stays ~");
        assert_eq!(folder_label("/").as_deref(), Some("/"), "root stays /");
        assert_eq!(folder_label(""), None, "empty path → no label");
        assert_eq!(folder_label("  "), None, "blank path → no label");
    }

    fn redacting_policy() -> crate::security::Policy {
        let mut p = crate::security::Policy::new();
        p.add_redaction("AKIA[0-9A-Z]{6}", "«key»", crate::security::RedactScope::Terminal, false).unwrap();
        p
    }

    #[test]
    fn redact_terminal_masks_plain_text() {
        let p = redacting_policy();
        assert_eq!(redact_terminal("token AKIA123ABC done", &p), "token «key» done");
    }

    #[test]
    fn redact_terminal_preserves_ansi_escapes() {
        let p = redacting_policy();
        // SGR colour + an OSC title around the secret — escape bytes must survive
        // untouched while only the printable run is masked.
        let input = "\u{1b}[31mAKIA123ABC\u{1b}[0m\u{1b}]0;AKIA123ABC\u{07}tail";
        let out = redact_terminal(input, &p);
        assert_eq!(out, "\u{1b}[31m«key»\u{1b}[0m\u{1b}]0;AKIA123ABC\u{07}tail");
        // The CSI and OSC control sequences are byte-identical to the input.
        assert!(out.contains("\u{1b}[31m") && out.contains("\u{1b}[0m"));
        assert!(out.contains("\u{1b}]0;AKIA123ABC\u{07}"));
    }

    #[test]
    fn redact_terminal_noop_without_rules() {
        let p = crate::security::Policy::new();
        let s = "\u{1b}[1mhello\u{1b}[0m world";
        assert_eq!(redact_terminal(s, &p), s);
    }

    #[test]
    fn build_policy_threads_confirm_tier() {
        let mut config = Config::default();
        config.denied_commands = vec!["^rm\\b".to_string()];
        config.confirm_commands = vec!["\\bforce\\b".to_string()];
        config.allowed_commands = vec!["^git".to_string()];
        let registry = crate::plugin::PluginRegistry::new();
        let p = crate::security::build_policy(&config, &registry);
        assert!(matches!(p.check_command("git status"), crate::security::Verdict::Allow));
        assert!(matches!(p.check_command("git push --force"), crate::security::Verdict::Confirm { .. }));
        assert!(matches!(p.check_command("rm file"), crate::security::Verdict::Deny { .. }));
    }

    // The default command-guard + redactor PLUGINS supply the policy (the guard
    // crate is gone). The rules are registry DATA (builtin/plugins/) the user
    // installs — loaded here from the repo, not embedded. These golden tests fail
    // if a default rule's regex is silently dropped (build_policy skips a bad
    // pattern), so they double as a compile check. All strings are INERT literals.
    fn default_policy() -> crate::security::Policy {
        let mut reg = crate::plugin::PluginRegistry::new();
        for name in ["command-guard", "redactor"] {
            let p = format!("{}/../../builtin/plugins/{name}/plugin.toml", env!("CARGO_MANIFEST_DIR"));
            let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p}: {e}"));
            reg.add_trusted(crate::plugin::Manifest::parse(&text).unwrap());
        }
        crate::security::build_policy(&Config::default(), &reg)
    }

    #[test]
    fn command_guard_plugin_enforces_default_deny_and_confirm() {
        use crate::security::Verdict;
        let p = default_policy();
        assert!(matches!(p.check_command("rm -rf /"), Verdict::Deny { .. }), "catastrophic rm denied");
        assert!(matches!(p.check_command(":(){ :|:& };:"), Verdict::Deny { .. }), "fork bomb denied");
        assert!(matches!(p.check_command("sudo apt install x"), Verdict::Confirm { .. }), "sudo confirmed");
        assert!(matches!(p.check_command("git push --force origin"), Verdict::Confirm { .. }), "force-push confirmed");
        // ordinary commands stay allowed
        assert!(matches!(p.check_command("ls -la"), Verdict::Allow));
        assert!(matches!(p.check_command("git status"), Verdict::Allow));
    }

    #[test]
    fn redactor_plugin_is_the_single_redaction_source() {
        use crate::security::RedactScope::Ai;
        let p = default_policy();
        // Each secret is an INERT literal; the redactor plugin's rules must scrub it.
        assert!(!p.redact("key sk-ant-api03-AbCd1234EfGh5678IjKl", Ai).contains("AbCd1234EfGh5678IjKl"));
        assert!(!p.redact("AKIA1234567890ABCDEF here", Ai).contains("AKIA1234567890ABCDEF"));
        assert!(!p.redact("Authorization: Bearer eyJabc.def.ghi", Ai).contains("eyJabc.def.ghi"));
        assert!(!p.redact("API_KEY=supersecretvalue123", Ai).contains("supersecretvalue123"));
        // ordinary text is untouched
        assert_eq!(p.redact("cargo build --release", Ai), "cargo build --release");
    }
}
