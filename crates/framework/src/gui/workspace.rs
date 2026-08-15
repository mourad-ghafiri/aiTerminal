//! Workspace persistence: serialize the active profile's full terminal tab/split
//! layout to `profiles/<id>/workspace.toml` and restore it on launch / profile switch.
//!
//! The split topology, focus, and zoom are handled generically by
//! [`Tabs::snapshot`](super::panes::Tabs::snapshot) / `restore`; this module supplies the
//! Pane↔TOML closures: a terminal stores its zoom + cwd and is relaunched in that folder
//! (a live shell can't be resurrected — the tmux-resurrect model). Everything is TOML;
//! there is no JSON on disk.

use corelib::wire::Toml;

use super::setup::PaneFactory;
use super::panes::Tabs;
use super::{Pane, Session};

/// The active profile's `(emoji, name)` for the status-bar chip (falls back to a neutral
/// glyph + the id when metadata is missing).
pub(in crate::gui) fn profile_chip() -> (String, String) {
    let active = crate::profile::active();
    let s = |k: &str| active.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (emoji, name) = (s("emoji"), s("name"));
    if name.is_empty() {
        ("\u{1F464}".into(), crate::profile::active_id()) // 👤
    } else {
        (if emoji.is_empty() { "\u{1F464}".into() } else { emoji }, name)
    }
}

/// Expand a leading `~` in a saved cwd to the home dir (OSC-7 paths are usually
/// absolute, but be safe).
fn expand_tilde(path: &str) -> String {
    if path == "~" || path.starts_with("~/") {
        if let Some(home) = platform::os::home_dir() {
            return home.join(path.trim_start_matches('~').trim_start_matches('/')).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

/// How many lines of each pane's content persist (scrollback tail + screen) —
/// enough to pick up where you left off without ballooning workspace.toml.
const CONTENT_SAVE_LINES: usize = 1000;

/// One pane → TOML (`{kind, zoom, cwd, content}`): a terminal stores its working
/// directory (it relaunches there — a live process can't be resurrected) AND its
/// buffer content WITH styling (ANSI escapes), so the reopened pane silently
/// shows exactly the session you left, colors included.
fn snapshot_pane(p: &Pane, sel_band: (u8, u8, u8)) -> Toml {
    // LAUNCH IS ALWAYS TERMINAL MODE: a workspace pane persists as the SHELL it
    // parked (a live sitting can't be resurrected; its chat log is on disk) —
    // and with none parked, as a plain terminal at the workspace's root, so the
    // folder survives even though the conversation doesn't.
    if let Some(chat) = p.chat() {
        let mut kvs = match p.parked() {
            Some(shell) => {
                let Toml::Table(kvs) = snapshot_terminal(shell, sel_band) else { unreachable!("snapshot_terminal returns a table") };
                kvs
            }
            None => vec![
                ("kind".into(), Toml::Str("terminal".into())),
                ("cwd".into(), Toml::Str(chat.root().display().to_string())),
            ],
        };
        kvs.insert(1, ("zoom".into(), Toml::Float(p.zoom as f64)));
        return Toml::Table(kvs);
    }
    let Some(session) = p.session() else { return Toml::Table(vec![("kind".into(), Toml::Str("terminal".into()))]) };
    let Toml::Table(mut kvs) = snapshot_terminal(session, sel_band) else { unreachable!("snapshot_terminal returns a table") };
    kvs.insert(1, ("zoom".into(), Toml::Float(p.zoom as f64)));
    Toml::Table(kvs)
}

/// ONE terminal → TOML — the shape a plain terminal pane saves, and the shape a
/// workspace pane nests for its parked shell.
fn snapshot_terminal(session: &Session, sel_band: (u8, u8, u8)) -> Toml {
    // The grid size at save time — the restore rebuilds the terminal at exactly these
    // dims so the replayed content reproduces its physical rows (same width → same
    // wrapping); without it, a wider saved line re-wraps and the restore looks scrambled.
    let (cols, rows) = session.grid_size();
    let mut kvs = vec![
        ("kind".into(), Toml::Str("terminal".into())),
        ("cols".into(), Toml::Int(cols as i64)),
        ("rows".into(), Toml::Int(rows as i64)),
    ];
    if let Some((_, path)) = session.cwd() {
        kvs.push(("cwd".into(), Toml::Str(path)));
    }
    // The selection band is transient UI, not content — scrub it, or a live
    // shift-selection at save time is restored as an un-dismissable highlight.
    let content = session.content_ansi(CONTENT_SAVE_LINES, Some(sel_band)).join("\n");
    if !content.trim().is_empty() {
        kvs.push(("content".into(), Toml::Str(content)));
    }
    Toml::Table(kvs)
}

/// TOML → one pane, rebuilt through the factory: a terminal relaunches in its saved
/// cwd with the saved buffer content replayed above the fresh prompt.
fn restore_pane(factory: &PaneFactory, t: &Toml) -> Option<Pane> {
    if t.get("kind").and_then(|v| v.as_str()) != Some("terminal") {
        return None;
    }
    let cwd = t.get("cwd").and_then(|v| v.as_str()).map(expand_tilde);
    let content = t.get("content").and_then(|v| v.as_str());
    // Rebuild at the saved grid size so the content replays without re-wrapping (the
    // first layout then resizes the pane to its real rect). Both must be present and
    // sane, else fall back to the default 80×24.
    let dims = t
        .get("cols")
        .and_then(|v| v.as_num())
        .zip(t.get("rows").and_then(|v| v.as_num()))
        // A sane range (both finite, ≥1, and within a real grid): a corrupt `cols = 70000`
        // would otherwise wrap through `as u16` to a wrong (tiny) size. The first layout
        // resizes to the true rect anyway; this just needs to be non-degenerate.
        .filter(|(c, r)| c.is_finite() && r.is_finite() && *c >= 1.0 && *r >= 1.0 && *c <= 10_000.0 && *r <= 10_000.0)
        .map(|(c, r)| (c as u16, r as u16));
    let mut pane = factory.terminal_pane_at(cwd.as_deref(), content, dims).ok()?;
    if let Some(z) = t.get("zoom").and_then(|v| v.as_num()) {
        pane.zoom = z as f32;
    }
    Some(pane)
}

/// Persist a workspace under profile `id`'s `workspace.toml`: the full tab/split
/// tree plus the window's logical size and the tab-bar orientation — so reopening
/// the profile (or the terminal) restores the exact same state. A no-op when the
/// profile dir can't be resolved.
pub(in crate::gui) fn save_as(
    tabs: &Tabs<Pane>,
    id: &str,
    window: Option<(f32, f32)>,
    tab_bar: &str,
    sel_band: (u8, u8, u8),
) {
    let Some(path) = crate::profile::workspace_path(id) else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut doc = tabs.snapshot(&|p| snapshot_pane(p, sel_band));
    if let Toml::Table(pairs) = &mut doc {
        pairs.push(("tab_bar".into(), Toml::Str(tab_bar.to_string())));
        if let Some((w, h)) = window {
            pairs.push((
                "window".into(),
                Toml::Table(vec![("w".into(), Toml::Float(w as f64)), ("h".into(), Toml::Float(h as f64))]),
            ));
        }
    }
    let _ = std::fs::write(path, doc.to_string());
}

/// The saved workspace document for profile `id`, if any.
fn load_doc(id: &str) -> Option<Toml> {
    let path = crate::profile::workspace_path(id)?;
    let text = std::fs::read_to_string(path).ok()?;
    Toml::parse(&text).ok()
}

/// The active profile's saved logical window size (points), for the boot-time
/// `WindowConfig` — so the window reopens exactly as it was left.
pub(in crate::gui) fn saved_window(id: &str) -> Option<(f32, f32)> {
    let doc = load_doc(id)?;
    let win = doc.get("window")?;
    let w = win.get("w").and_then(|v| v.as_num())? as f32;
    let h = win.get("h").and_then(|v| v.as_num())? as f32;
    (w >= 200.0 && h >= 150.0).then_some((w, h))
}

/// The profile's saved tab-bar orientation name (`top`/`bottom`/`left`/`right`).
pub(in crate::gui) fn saved_tab_bar(id: &str) -> Option<String> {
    load_doc(id)?.get("tab_bar").and_then(|v| v.as_str()).map(str::to_string)
}

/// The tabs to open at launch: restore the active profile's saved `workspace.toml` when it
/// exists and rebuilds, else a single fresh shell. The active profile is the latest-opened —
/// so a single default profile just opens a terminal, while a profile with saved work comes
/// back exactly as it was left.
pub(in crate::gui) fn startup_tabs(factory: &PaneFactory) -> Tabs<Pane> {
    let id = crate::profile::active_id();
    crate::profile::touch(&id);
    if let Some(path) = crate::profile::workspace_path(&id) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(doc) = Toml::parse(&text) {
                let mut g = |t: &Toml| restore_pane(factory, t);
                if let Some(tabs) = Tabs::restore(&doc, &mut g) {
                    return tabs;
                }
            }
        }
    }
    Tabs::new(factory.initial_pane())
}

#[cfg(test)]
mod tests;
