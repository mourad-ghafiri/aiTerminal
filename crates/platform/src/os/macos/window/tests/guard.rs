use corelib::types::KeyCode;

#[test]
fn a_key_resolves_by_the_character_it_types_not_where_it_sits() {
    // Why ⌘Q works on any layout. `keycode_from_event` asks the OS what character
    // the key produces in the ACTIVE layout (`charactersIgnoringModifiers`) and
    // maps that — so the keycap marked Q is `KeyCode::Q` on AZERTY, QWERTZ and
    // QWERTY alike, even though it sits in three different places.
    assert_eq!(crate::os::macos::window::char_to_keycode('q'), Some(KeyCode::Q));
    assert_eq!(crate::os::macos::window::char_to_keycode('a'), Some(KeyCode::A));
    assert_eq!(crate::os::macos::window::char_to_keycode('z'), Some(KeyCode::Z));
    assert_eq!(crate::os::macos::window::char_to_keycode('w'), Some(KeyCode::W));

    // The hardware table underneath is US-QWERTY and is ONLY a fallback for keys
    // that type no character. It disagrees with AZERTY on exactly the keys AZERTY
    // moves — which is why it must never be consulted first.
    assert_eq!(crate::os::macos::window::keycode_from_hw(12), KeyCode::Q, "US scancode 12 is Q");
    assert_eq!(crate::os::macos::window::keycode_from_hw(0), KeyCode::A, "US scancode 0 is A");
    // On a French AZERTY the key at scancode 0 types 'q'; the character path gives
    // Q, the scancode path would give A. The character path is the one that runs.
    assert_ne!(crate::os::macos::window::char_to_keycode('q').unwrap(), crate::os::macos::window::keycode_from_hw(0));

    // NOTE: the `charactersIgnoringModifiers` call itself is ObjC and only runs on
    // a real keypress, so no unit test can cover that hop. It is verified by
    // pressing ⌘Q on a non-US layout.
}

#[test]
fn guarded_catches_panic_and_continues() {
    // A panic in one "frame" must NOT propagate — the loop keeps running, so a
    // subsequent frame still executes (the app survives a bad frame).
    let mut after = false;
    crate::os::macos::window::guarded(|| panic!("simulated render panic"));
    crate::os::macos::window::guarded(|| after = true);
    assert!(after, "the event loop must continue after a caught panic");
}
