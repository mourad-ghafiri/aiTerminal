//! The App's keyboard actions — the concrete action enum a [`Chord`] maps to, the
//! declarative-name resolver (used by config + plugin keybindings), and the
//! default macOS-style binding table. The generic keymap container lives in
//! `crate::keymap`; dispatch lives on `GuiApp`.

use crate::keymap::Keymap;
use corelib::wire::Toml;

/// What a chord does. Anything not bound falls through to the focused PTY.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    GoToTab(u8),
    SplitRight, // vertical divider → new pane to the right
    SplitDown,  // horizontal divider → new pane below
    ClosePane,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    FocusNext,
    ZoomPane,
    Copy,
    Paste,
    ScrollLineUp,
    ScrollLineDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
    /// Open the tab quick-switcher overlay (type a number or name to jump).
    TabSwitcher,
    /// Font zoom for the focused split.
    ZoomInPane,
    ZoomOutPane,
    /// Reset the focused split's zoom to default.
    ResetZoom,
    /// Cycle the tab bar position: top → left → right → top.
    CycleTabBar,
    /// Reload `~/.aiTerminal/config.toml` and reapply preferences live.
    ReloadConfig,
}

impl Action {
    /// Resolve an action from a declarative name (used by plugin keybindings).
    /// Accepts snake_case / kebab-case / spaces, case-insensitively.
    pub fn from_name(name: &str) -> Option<Action> {
        use Action::*;
        let n = name.trim().to_lowercase().replace([' ', '-'], "_");
        // go_to_tab_<1..9> — the human-facing number is 1-based ("tab 1" = the first
        // tab); the dispatch index (`Tabs::goto`) is 0-based, so map N → N-1 here. Keeping
        // this conversion in ONE place is what lets the default keymap be plain data
        // (`go_to_tab_1` … `go_to_tab_9`) and match the runtime exactly.
        if let Some(d) = n.strip_prefix("go_to_tab_") {
            if let Ok(i) = d.parse::<u8>() {
                if (1..=9).contains(&i) {
                    return Some(GoToTab(i - 1));
                }
            }
        }
        Some(match n.as_str() {
            "new_tab" => NewTab,
            "close_tab" => CloseTab,
            "next_tab" => NextTab,
            "prev_tab" | "previous_tab" => PrevTab,
            "split_right" => SplitRight,
            "split_down" => SplitDown,
            "close_pane" => ClosePane,
            "focus_left" => FocusLeft,
            "focus_right" => FocusRight,
            "focus_up" => FocusUp,
            "focus_down" => FocusDown,
            "focus_next" => FocusNext,
            "zoom_pane" => ZoomPane,
            "copy" => Copy,
            "paste" => Paste,
            "scroll_line_up" => ScrollLineUp,
            "scroll_line_down" => ScrollLineDown,
            "scroll_page_up" => ScrollPageUp,
            "scroll_page_down" => ScrollPageDown,
            "scroll_top" => ScrollTop,
            "scroll_bottom" => ScrollBottom,
            "tab_switcher" | "switch_tab" | "command_palette" => TabSwitcher,
            "zoom_in_pane" => ZoomInPane,
            "zoom_out_pane" => ZoomOutPane,
            "reset_zoom" => ResetZoom,
            "cycle_tab_bar" => CycleTabBar,
            "reload_config" => ReloadConfig,
            _ => return None,
        })
    }
}

/// The bundled default keymap, embedded so it is ALWAYS available (independent of
/// finding the `builtin/` bundle at runtime). This single TOML file is the source of
/// truth for the engine's built-in chords; `default_keymap()` parses it.
pub const DEFAULT_KEYMAP_TOML: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../builtin/keymaps/default.toml"));

/// Collect the `[[keybinding]]` `(key, action)` pairs from a parsed keymap document,
/// in file order. Shared by the embedded default keymap and the user keymap files so
/// both parse identically.
pub(crate) fn keybinding_pairs(doc: &Toml) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(kbs) = doc.get("keybinding").and_then(|v| v.as_array()) {
        for k in kbs {
            if let (Some(key), Some(action)) =
                (k.get("key").and_then(|v| v.as_str()), k.get("action").and_then(|v| v.as_str()))
            {
                out.push((key.to_string(), action.to_string()));
            }
        }
    }
    out
}

/// The default macOS-style bindings (Cmd-led, like iTerm/Terminal), parsed from the
/// embedded `builtin/keymaps/default.toml` (data, not hardcoded). The embedded file is
/// valid (built with the binary), so a parse failure yields an empty keymap rather than
/// a panic.
pub fn default_keymap() -> Keymap<Action> {
    let mut k = Keymap::empty();
    let Ok(doc) = Toml::parse(DEFAULT_KEYMAP_TOML) else { return k };
    for (key, action) in keybinding_pairs(&doc) {
        if let Some(a) = Action::from_name(&action) {
            k.bind_str(&key, a);
        }
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;
    use corelib::types::Chord;

    #[test]
    fn action_from_name_resolves_plugin_bindings() {
        assert_eq!(Action::from_name("close_tab"), Some(Action::CloseTab));
        assert_eq!(Action::from_name("Cycle-Tab-Bar"), Some(Action::CycleTabBar)); // case/sep insensitive
        assert_eq!(Action::from_name("go_to_tab_3"), Some(Action::GoToTab(2))); // 1-based name → 0-based index
        assert_eq!(Action::from_name("split down"), Some(Action::SplitDown));
        assert_eq!(Action::from_name("tab_switcher"), Some(Action::TabSwitcher));
        assert_eq!(Action::from_name("command_palette"), Some(Action::TabSwitcher)); // alias
        assert_eq!(Action::from_name("find"), None); // dead action removed
        assert_eq!(Action::from_name("open_app_browser"), None); // the app layer is gone
        assert_eq!(Action::from_name("not_a_real_action"), None);
        // A plugin binding can be merged into a keymap by name.
        let mut km = Keymap::empty();
        assert!(km.bind_str("cmd+shift+x", Action::from_name("reload_config").unwrap()));
        assert_eq!(km.lookup(&Chord::parse("cmd+shift+x").unwrap()), Some(&Action::ReloadConfig));
    }

    #[test]
    fn defaults_have_core_bindings() {
        let k = default_keymap();
        assert_eq!(k.lookup(&Chord::parse("cmd+t").unwrap()), Some(&Action::NewTab));
        assert_eq!(k.lookup(&Chord::parse("cmd+d").unwrap()), Some(&Action::SplitRight));
        assert_eq!(k.lookup(&Chord::parse("cmd+shift+d").unwrap()), Some(&Action::SplitDown));
        assert_eq!(k.lookup(&Chord::parse("cmd+enter").unwrap()), Some(&Action::ZoomPane));
        // Layout-independent tab cycling (no brackets).
        assert_eq!(k.lookup(&Chord::parse("ctrl+tab").unwrap()), Some(&Action::NextTab));
        // Cmd+Shift+←/→ stay UNBOUND — they fall through to the PTY as the xterm
        // select-to-line-edge sequences the lineedit plugin binds.
        assert_eq!(k.lookup(&Chord::parse("cmd+shift+right").unwrap()), None);
        assert_eq!(k.lookup(&Chord::parse("cmd+shift+left").unwrap()), None);
        assert_eq!(k.lookup(&Chord::parse("cmd+9").unwrap()), Some(&Action::GoToTab(8))); // single-digit jumps to 9
        assert_eq!(k.lookup(&Chord::parse("cmd+p").unwrap()), Some(&Action::TabSwitcher));
        assert_eq!(k.lookup(&Chord::parse("cmd+k").unwrap()), Some(&Action::TabSwitcher));
        assert_eq!(k.lookup(&Chord::parse("cmd+j").unwrap()), None); // unbound falls through
        // Shift-family scroll chords (terminal scrollback + app document).
        assert_eq!(k.lookup(&Chord::parse("shift+pageup").unwrap()), Some(&Action::ScrollPageUp));
        assert_eq!(k.lookup(&Chord::parse("shift+up").unwrap()), Some(&Action::ScrollLineUp));
        assert_eq!(k.lookup(&Chord::parse("shift+home").unwrap()), Some(&Action::ScrollTop));
        assert_eq!(k.lookup(&Chord::parse("shift+end").unwrap()), Some(&Action::ScrollBottom));
        assert_eq!(Action::from_name("scroll_top"), Some(Action::ScrollTop));
        assert_eq!(Action::from_name("scroll_bottom"), Some(Action::ScrollBottom));
    }

    #[test]
    fn embedded_default_keymap_is_valid_data() {
        // The bundled default.toml parses, and EVERY action name in it resolves — a typo
        // (or a renamed action) in the data file fails here rather than silently dropping a
        // default binding at runtime.
        let doc = Toml::parse(DEFAULT_KEYMAP_TOML).expect("default.toml parses");
        let pairs = keybinding_pairs(&doc);
        assert!(pairs.len() >= 37, "the default keymap defines the full chord set, got {}", pairs.len());
        for (key, action) in &pairs {
            assert!(Chord::parse(key).is_some(), "default.toml chord {key:?} is parseable");
            assert!(Action::from_name(action).is_some(), "default.toml action {action:?} resolves");
        }
        assert_eq!(doc.get("name").and_then(|v| v.as_str()), Some("Default"));
    }
}
