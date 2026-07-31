use super::*;

fn k(name: &str) -> Vec<u8> {
    key_bytes(name, false).unwrap_or_else(|| panic!("unknown key {name}"))
}

#[test]
fn arrows_follow_the_mode_the_program_selected() {
    // The reason a host that only ever sends CSI cannot move the selection in a
    // program that asked for application cursor keys.
    assert_eq!(key_bytes("up", false).unwrap(), b"\x1b[A");
    assert_eq!(key_bytes("up", true).unwrap(), b"\x1bOA");
    assert_eq!(key_bytes("down", true).unwrap(), b"\x1bOB");
    assert_eq!(key_bytes("right", true).unwrap(), b"\x1bOC");
    assert_eq!(key_bytes("left", true).unwrap(), b"\x1bOD");
    assert_eq!(key_bytes("home", true).unwrap(), b"\x1bOH");
    assert_eq!(key_bytes("end", true).unwrap(), b"\x1bOF");
}

#[test]
fn the_everyday_named_keys_map_correctly() {
    assert_eq!(k("enter"), b"\r");
    assert_eq!(k("tab"), b"\t");
    assert_eq!(k("shift-tab"), b"\x1b[Z");
    assert_eq!(k("esc"), b"\x1b");
    assert_eq!(k("backspace"), b"\x7f");
    assert_eq!(k("pgdn"), b"\x1b[6~");
    assert_eq!(k("del"), b"\x1b[3~");
}

#[test]
fn ctrl_is_a_rule_not_a_table() {
    // Every ctrl-letter works, so menu navigation (ctrl-n/p) and word delete
    // (ctrl-w) need no enumeration.
    assert_eq!(k("ctrl-c"), &[0x03]);
    assert_eq!(k("ctrl-d"), &[0x04]);
    assert_eq!(k("ctrl-w"), &[0x17]);
    assert_eq!(k("ctrl-n"), &[0x0e]);
    assert_eq!(k("ctrl-p"), &[0x10]);
    assert_eq!(k("^r"), &[0x12]);
    assert_eq!(k("ctrl-space"), &[0x00]);
}

#[test]
fn alt_prefixes_with_escape() {
    assert_eq!(k("alt-b"), b"\x1bb");
    assert_eq!(k("meta-f"), b"\x1bf");
    assert_eq!(k("alt-."), b"\x1b.");
}

#[test]
fn function_keys_use_the_standard_xterm_forms() {
    assert_eq!(k("f1"), b"\x1bOP");
    assert_eq!(k("f4"), b"\x1bOS");
    assert_eq!(k("f5"), b"\x1b[15~");
    assert_eq!(k("f10"), b"\x1b[21~");
    assert_eq!(k("f12"), b"\x1b[24~");
    assert_eq!(key_bytes("f13", false), None);
}

#[test]
fn a_single_character_keeps_its_case() {
    // `G` is "go to the bottom" in vim and every pager; lowercasing it silently
    // does something else entirely.
    assert_eq!(k("q"), b"q");
    assert_eq!(k("G"), b"G");
    assert_eq!(k("Y"), b"Y");
    assert_eq!(k("3"), b"3");
}

#[test]
fn an_unrecognized_name_is_refused_rather_than_typed() {
    for bad in ["", "  ", "delete-everything", "ctrl-shift-meta-x", "ctrl-", "alt-", "rm -rf /"] {
        assert_eq!(key_bytes(bad, false), None, "{bad:?} must not become input");
    }
}

#[test]
fn a_multi_line_prompt_arrives_as_one_paste() {
    // Without the wrapper an input box that submits on Enter would run the first
    // line and treat the rest as follow-up messages — the single biggest reason
    // sending a real prompt from a phone fails.
    let prompt = "refactor the parser\nkeep the tests green";
    let out = typed_text(prompt, true);
    let s = String::from_utf8(out).unwrap();
    assert!(s.starts_with("\x1b[200~") && s.ends_with("\x1b[201~"));
    assert_eq!(s.matches('\r').count(), 1, "the newline is content, not a submit");

    // A program that never asked for it gets the plain bytes.
    assert_eq!(typed_text(prompt, false), b"refactor the parser\rkeep the tests green");
}

#[test]
fn a_single_keystroke_is_never_wrapped_in_a_paste() {
    // `/keys y` answering a prompt must arrive as one byte. vim inserts a
    // Normal-mode paste as literal text; a raw single-byte reader sees the escape.
    assert_eq!(typed_text("y", true), b"y");
    assert_eq!(typed_text(":wq", true), b":wq");
    assert_eq!(typed_text("a long single line with spaces", true), b"a long single line with spaces");
}

#[test]
fn newlines_are_normalized_to_carriage_returns() {
    assert_eq!(typed_text("a\r\nb\nc", false), b"a\rb\rc");
}

#[test]
fn the_submitting_return_sits_outside_the_bracket() {
    // Inside, it is pasted content; a program that tells paste from typing would
    // insert a newline instead of accepting the prompt.
    let out = String::from_utf8(typed_line("first\nsecond", true)).unwrap();
    assert_eq!(out, "\x1b[200~first\rsecond\x1b[201~\r");
    // A single line needs no bracket at all, so it is just the text and a Return.
    assert_eq!(typed_line("hello", true), b"hello\r");
    assert_eq!(typed_line("hello", false), b"hello\r");
}
