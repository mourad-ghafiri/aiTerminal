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
