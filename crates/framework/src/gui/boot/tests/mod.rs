use super::*;

#[test]
fn build_keymap_composes_defaults_plugins_and_config() {
    // Hermetic: a temp $HOME so no user keymap files leak in.
    let (_h, _home) = crate::test_home::lock_home("boot-keymap");
    let mut config = Config::default();
    // A config [[keybinding]] overrides a default chord (config wins last).
    config.keybindings = vec![("cmd+t".into(), "close_tab".into()), ("ctrl+alt+z".into(), "zoom_pane".into())];
    let registry = crate::plugin::load_registry(&config);
    let km = build_keymap(&config, &registry);
    use corelib::types::Chord;
    assert_eq!(km.lookup(&Chord::parse("cmd+t").unwrap()), Some(&Action::CloseTab), "config override wins over the default new_tab");
    assert_eq!(km.lookup(&Chord::parse("ctrl+alt+z").unwrap()), Some(&Action::ZoomPane), "a new config chord binds");
    assert_eq!(km.lookup(&Chord::parse("cmd+d").unwrap()), Some(&Action::SplitRight), "untouched defaults survive");
}
