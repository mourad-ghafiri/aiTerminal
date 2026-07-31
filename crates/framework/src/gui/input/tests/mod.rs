use super::*;

fn enc(code: KeyCode, mods: Modifiers) -> Option<Vec<u8>> {
    encode_key(code, mods)
}
fn seq(code: KeyCode, mods: Modifiers) -> String {
    String::from_utf8(enc(code, mods).expect("a sequence")).unwrap()
}

#[test]
fn plain_keys_keep_the_classic_sequences() {
    assert_eq!(seq(KeyCode::Left, Modifiers::empty()), "\x1b[D");
    assert_eq!(seq(KeyCode::Up, Modifiers::empty()), "\x1b[A");
    assert_eq!(seq(KeyCode::Home, Modifiers::empty()), "\x1b[H");
    assert_eq!(seq(KeyCode::Backspace, Modifiers::empty()), "\x7f");
    assert_eq!(seq(KeyCode::Enter, Modifiers::empty()), "\r");
    assert_eq!(enc(KeyCode::A, Modifiers::empty()), None); // letters arrive as TextInput
}

#[test]
fn modified_arrows_use_the_xterm_mod_encoding() {
    // 1 + shift(1) + alt(2) + ctrl(4) + cmd(8)
    assert_eq!(seq(KeyCode::Left, Modifiers::SHIFT), "\x1b[1;2D"); // ⇧← select char
    assert_eq!(seq(KeyCode::Left, Modifiers::ALT), "\x1b[1;3D"); // ⌥← word jump
    assert_eq!(seq(KeyCode::Right, Modifiers::SHIFT | Modifiers::ALT), "\x1b[1;4C"); // ⇧⌥→ select word
    assert_eq!(seq(KeyCode::Left, Modifiers::CONTROL), "\x1b[1;5D"); // ⌃← word jump
    assert_eq!(seq(KeyCode::Left, Modifiers::SUPER), "\x1b[1;9D"); // ⌘← line start
    assert_eq!(seq(KeyCode::Right, Modifiers::SUPER), "\x1b[1;9C"); // ⌘→ line end
    assert_eq!(seq(KeyCode::Right, Modifiers::SHIFT | Modifiers::SUPER), "\x1b[1;10C"); // ⇧⌘→ select to end
    assert_eq!(seq(KeyCode::Up, Modifiers::SUPER), "\x1b[1;9A");
}

#[test]
fn modified_edit_keys_encode_too() {
    assert_eq!(seq(KeyCode::Home, Modifiers::SUPER), "\x1b[1;9H");
    assert_eq!(seq(KeyCode::End, Modifiers::SHIFT), "\x1b[1;2F");
    assert_eq!(seq(KeyCode::Delete, Modifiers::SHIFT), "\x1b[3;2~");
    assert_eq!(seq(KeyCode::PageUp, Modifiers::ALT), "\x1b[5;3~");
    assert_eq!(seq(KeyCode::Tab, Modifiers::SHIFT), "\x1b[Z"); // back-tab
    assert_eq!(seq(KeyCode::Backspace, Modifiers::ALT), "\x1b\x7f"); // ⌥⌫ kill word
    assert_eq!(seq(KeyCode::Backspace, Modifiers::SUPER), "\x1b[127;9u"); // ⌘⌫ kill to line start
    // Shift/Ctrl backspace stay the plain DEL byte — no surprise rebinds.
    assert_eq!(seq(KeyCode::Backspace, Modifiers::SHIFT), "\x7f");
}

#[test]
fn control_letters_still_become_control_bytes() {
    assert_eq!(enc(KeyCode::C, Modifiers::CONTROL), Some(vec![3]));
    assert_eq!(enc(KeyCode::Space, Modifiers::CONTROL), Some(vec![0]));
    assert_eq!(enc(KeyCode::BracketLeft, Modifiers::CONTROL), Some(vec![27]));
    // and a ctrl+arrow is an arrow sequence, not a (nonexistent) ctrl byte
    assert_eq!(seq(KeyCode::Right, Modifiers::CONTROL), "\x1b[1;5C");
}
