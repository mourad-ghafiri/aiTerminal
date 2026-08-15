//! The interactive GUI front-end — a light terminal window: tabs + splits of PTY
//! panes, the tab quick-switcher, the plugin status bar, and per-profile workspace
//! persistence. All AI lives behind the `@ai` / `@<agent>` shell integration (the
//! `aiTerminal ai` CLI), so the window itself stays a pure terminal.

// The GUI front-end is split across sibling submodules; they reach this module's
// items through `use super::*`, so the shared imports, types, constants, and
// helper fns are `pub(crate)`.
pub(crate) mod action;
mod boot;
mod chat;
mod confirm;
mod gate;
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
pub use confirm::render_confirm_proof;
use confirm::{draw_confirm, CloseIntent, Confirm};
pub use chat::{render_chat_proof, render_home_proof};
pub use gate::render_gate_proof;
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
        guard: Arc<crate::guard::Guard>,
        integ: crate::shell::ShellSpawn,
        scrollback: usize,
        cwd: Option<&str>,
        restore: Option<&str>,
        dims: Option<(u16, u16)>,
    ) -> std::io::Result<Session> {
        // Start the grid at the pane's SAVED size when restoring a workspace, so the
        // replayed content reproduces its exact physical rows (the VT parser wraps a line
        // the same way only at the same width — spawning at a fixed 80 col re-wrapped every
        // wider line, scrambling the restore). A fresh pane uses the classic 80×24 until
        // the first layout resizes it to the real rect.
        let (cols, rows) = dims.map(|(c, r)| (c.max(1), r.max(1))).unwrap_or((80, 24));
        // An interactive login shell: argv[0]=`-<name>`, cwd=$HOME (or an explicit `cwd`
        // when restoring a saved workspace), TERM exported — so the window works correctly
        // even when launched from the desktop (Dock). Shell integration (aliases / file
        // colors / prompt) rides in via env+args.
        let cmd = PtyCommand {
            program: shell.to_string(),
            args: integ.args,
            cols,
            rows,
            login: integ.login,
            env: integ.env,
            cwd: cwd.map(str::to_string),
        };
        let pty: Arc<dyn Pty> = Arc::from(platform::os::spawn_pty(&cmd)?);
        let term = Arc::new(Mutex::new(Term::with_scrollback(cols, rows, scrollback)));
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
            let redact = guard.masks_display();
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
                                redact.then(|| redact_terminal(&String::from_utf8_lossy(&buf[..n]), &guard));
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
        Ok(Session { pty, term, cols, rows, selection: None, shell_name, exited })
    }

    /// Whether the shell process has ended (so the host can reap this pane).
    fn exited(&self) -> bool {
        self.exited.load(SeqCst)
    }

    /// The current grid size (cols, rows) — persisted per pane so a restore rebuilds
    /// the terminal at exactly this width and the saved content never re-wraps.
    fn grid_size(&self) -> (u16, u16) {
        (self.cols, self.rows)
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
        // icon prefix this reads e.g. "3 - 🖥 the-terminal [zsh]".
        let shell = self.shell_name.trim_start_matches('-');
        match self.cwd().and_then(|(_host, path)| folder_label(&path)) {
            Some(folder) => format!("{folder} [{shell}]"),
            None => format!("[{shell}]"),
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

/// What a pane holds: a terminal session, or a workspace sitting — the
/// conversation lives in the split tree beside the shells, so tabs, splits,
/// focus, zoom and close all treat it as just another pane.
pub(crate) enum PaneContent {
    Terminal(Session),
    /// A workspace sitting, shown IN PLACE of the pane's shell: the session is
    /// parked, not killed — its PTY keeps feeding its own term underneath, and
    /// ⌘J (or the sitting ending) restores it exactly as it was. `parked` is
    /// `None` for a pane restored from disk without a shell behind it.
    Workspace { chat: Box<chat::ChatSurface>, parked: Option<Session> },
}

/// A pane: a font-zoom level plus what it holds.
pub(crate) struct Pane {
    zoom: f32,
    content: PaneContent,
}

impl Pane {
    fn terminal(s: Session, zoom: f32) -> Pane {
        Pane { zoom, content: PaneContent::Terminal(s) }
    }
    fn workspace(chat: chat::ChatSurface, zoom: f32) -> Pane {
        Pane { zoom, content: PaneContent::Workspace { chat: Box::new(chat), parked: None } }
    }
    /// Show a workspace IN THIS PANE: the current shell parks behind it.
    fn wrap_workspace(&mut self, chat: chat::ChatSurface) {
        let old = std::mem::replace(&mut self.content, PaneContent::Workspace { chat: Box::new(chat), parked: None });
        match old {
            PaneContent::Terminal(session) => {
                if let PaneContent::Workspace { parked, .. } = &mut self.content {
                    *parked = Some(session);
                }
            }
            // Never called on a workspace pane — keep what was there.
            other @ PaneContent::Workspace { .. } => self.content = other,
        }
    }
    /// Bring the parked shell back, exactly as it was. `false` when there is
    /// nothing parked — the caller closes the pane instead.
    fn unwrap_terminal(&mut self) -> bool {
        if let PaneContent::Workspace { parked: parked @ Some(_), .. } = &mut self.content {
            let session = parked.take().expect("matched Some");
            self.content = PaneContent::Terminal(session);
            return true;
        }
        false
    }
    /// The terminal session, when this pane SHOWS one (a parked shell is
    /// invisible to the terminal machinery until it returns).
    pub(crate) fn session(&self) -> Option<&Session> {
        match &self.content {
            PaneContent::Terminal(s) => Some(s),
            PaneContent::Workspace { .. } => None,
        }
    }
    pub(crate) fn session_mut(&mut self) -> Option<&mut Session> {
        match &mut self.content {
            PaneContent::Terminal(s) => Some(s),
            PaneContent::Workspace { .. } => None,
        }
    }
    /// The shell parked behind a workspace — persistence saves it whole.
    pub(crate) fn parked(&self) -> Option<&Session> {
        match &self.content {
            PaneContent::Workspace { parked, .. } => parked.as_ref(),
            PaneContent::Terminal(_) => None,
        }
    }
    /// The workspace surface, when this pane is one.
    pub(crate) fn chat(&self) -> Option<&chat::ChatSurface> {
        match &self.content {
            PaneContent::Workspace { chat, .. } => Some(chat),
            PaneContent::Terminal(_) => None,
        }
    }
    pub(crate) fn chat_mut(&mut self) -> Option<&mut chat::ChatSurface> {
        match &mut self.content {
            PaneContent::Workspace { chat, .. } => Some(chat),
            PaneContent::Terminal(_) => None,
        }
    }
    /// The pane NAME only (no icon): a program's own title, else `<folder> [<shell>]`;
    /// a workspace names its folder. The renderer composes `index - icon name`.
    fn title(&self) -> String {
        match &self.content {
            PaneContent::Terminal(s) => s.title(),
            PaneContent::Workspace { chat, .. } => {
                let name = chat.root().file_name().and_then(|s| s.to_str()).unwrap_or("workspace");
                format!("\u{2726} {name}")
            }
        }
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
    /// Last unix-time the job supervisor ran (throttle) — see `follow_jobs`.
    last_jobs_check: u64,
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
    guard: Arc<crate::guard::Guard>,
    /// The tab quick-switcher overlay (Cmd+P / Cmd+K), if open.
    switcher: TabSwitcher,
    /// The close confirmation, if open. Above the switcher in every sense: it takes
    /// input first, draws last, and opening it dismisses the switcher.
    confirm: Confirm,
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
pub(crate) fn redact_terminal(text: &str, guard: &crate::guard::Guard) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut run = String::new();
    let mut i = 0;
    let flush = |run: &mut String, out: &mut String| {
        if !run.is_empty() {
            out.push_str(&guard.mask(&run));
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
mod tests;
