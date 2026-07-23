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
use super::Pane;

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
    let mut kvs = vec![
        ("kind".into(), Toml::Str("terminal".into())),
        ("zoom".into(), Toml::Float(p.zoom as f64)),
    ];
    if let Some((_, path)) = p.session.cwd() {
        kvs.push(("cwd".into(), Toml::Str(path)));
    }
    // The selection band is transient UI, not content — scrub it, or a live
    // shift-selection at save time is restored as an un-dismissable highlight.
    let content = p.session.content_ansi(CONTENT_SAVE_LINES, Some(sel_band)).join("\n");
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
    let mut pane = factory.terminal_pane_at(cwd.as_deref(), content).ok()?;
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
mod tests {
    use super::*;

    #[test]
    fn terminal_pane_snapshot_round_trips_through_toml() {
        // A terminal-pane snapshot table round-trips kind/zoom/cwd through TOML text — the
        // exact path workspace.toml takes (the full Pane is rebuilt by the live factory).
        let t = Toml::Table(vec![
            ("kind".into(), Toml::Str("terminal".into())),
            ("zoom".into(), Toml::Float(1.25)),
            ("cwd".into(), Toml::Str("/work/My Project".into())),
        ]);
        let back = Toml::parse(&t.to_string()).unwrap();
        assert_eq!(back.get("kind").and_then(|v| v.as_str()), Some("terminal"));
        assert_eq!(back.get("zoom").and_then(|v| v.as_num()), Some(1.25));
        assert_eq!(back.get("cwd").and_then(|v| v.as_str()), Some("/work/My Project"));
    }

    #[test]
    fn multi_tab_split_layout_round_trips() {
        // Two tabs — one a single pane, one a split of two terminals in different folders —
        // keep their layout, cwds, and the active-tab/focus through the TOML text form.
        let mk = |cwd: &str| {
            Toml::Table(vec![
                ("kind".into(), Toml::Str("terminal".into())),
                ("cwd".into(), Toml::Str(cwd.into())),
            ])
        };
        let doc = Toml::Table(vec![
            ("active".into(), Toml::Int(1)),
            ("tab".into(), Toml::Array(vec![
                Toml::Table(vec![("focus".into(), Toml::Int(0)), ("root".into(), Toml::Table(vec![("leaf".into(), mk("/home"))]))]),
                Toml::Table(vec![
                    ("focus".into(), Toml::Int(1)),
                    ("root".into(), Toml::Table(vec![("split".into(), Toml::Table(vec![
                        ("dir".into(), Toml::Str("row".into())),
                        ("kids".into(), Toml::Array(vec![
                            Toml::Table(vec![("leaf".into(), mk("/tmp"))]),
                            Toml::Table(vec![("leaf".into(), mk("/work/a,b"))]),
                        ])),
                    ]))])),
                ]),
            ])),
        ]);
        let back = Toml::parse(&doc.to_string()).unwrap();
        assert_eq!(back.get("active").and_then(|v| v.as_num()), Some(1.0));
        let tabs = back.get("tab").and_then(|v| if let Toml::Array(a) = v { Some(a) } else { None }).unwrap();
        assert_eq!(tabs.len(), 2, "both tabs survive");
        let split_kids = tabs[1].get("root").and_then(|r| r.get("split")).and_then(|s| s.get("kids"))
            .and_then(|v| if let Toml::Array(a) = v { Some(a) } else { None }).unwrap();
        let cwd_of = |t: &Toml| t.get("leaf").and_then(|l| l.get("cwd")).and_then(|a| a.as_str()).map(str::to_string);
        assert_eq!(cwd_of(&split_kids[0]).as_deref(), Some("/tmp"));
        assert_eq!(cwd_of(&split_kids[1]).as_deref(), Some("/work/a,b"));
    }

    #[test]
    fn window_and_tab_bar_round_trip_through_workspace_toml() {
        // The exact-state promise: the saved doc carries the window's logical size
        // and the tab-bar orientation, and the readers resolve them back.
        let (_h, _home) = crate::test_home::lock_home("ws-window");
        crate::profile::ensure_default();
        let path = crate::profile::workspace_path("default").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "active = 0\ntab_bar = \"left\"\n[window]\nw = 1280.0\nh = 800.0\n[[tab]]\nfocus = 0\n[tab.root.leaf]\nkind = \"terminal\"\n",
        )
        .unwrap();
        assert_eq!(saved_window("default"), Some((1280.0, 800.0)));
        assert_eq!(saved_tab_bar("default").as_deref(), Some("left"));
        // Garbage sizes are rejected (never a 1×1 window).
        std::fs::write(&path, "[window]\nw = 10.0\nh = 5.0\n").unwrap();
        assert_eq!(saved_window("default"), None);
        // No file → no overrides.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(saved_window("default"), None);
        assert_eq!(saved_tab_bar("default"), None);
    }

    #[test]
    fn pane_content_round_trips_through_workspace_toml() {
        // The restore promise: a pane's STYLED buffer (ANSI escapes, multi-line,
        // quotes, non-ASCII) survives the TOML text form byte-for-byte.
        let content = "\u{276F} \x1b[32mcargo test\x1b[0m\nrunning \x1b[1m5\x1b[0m tests\ntest a::b {ok} \"quoted\" … مرحبا\n\x1b[38;5;42mdone\x1b[0m";
        let t = Toml::Table(vec![
            ("kind".into(), Toml::Str("terminal".into())),
            ("cwd".into(), Toml::Str("/w".into())),
            ("content".into(), Toml::Str(content.into())),
        ]);
        let back = Toml::parse(&t.to_string()).unwrap();
        assert_eq!(back.get("content").and_then(|v| v.as_str()), Some(content), "content is lossless through workspace.toml");
    }

    #[test]
    fn expand_tilde_resolves_home() {
        let home = platform::os::home_dir().unwrap();
        assert_eq!(expand_tilde("~/proj"), home.join("proj").to_string_lossy());
        assert_eq!(expand_tilde("/abs/path"), "/abs/path");
    }
}
