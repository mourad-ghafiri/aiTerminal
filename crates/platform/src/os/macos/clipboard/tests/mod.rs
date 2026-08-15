use super::*;

#[test]
fn round_trips_through_the_system_pasteboard() {
    // Save whatever the USER has on the pasteboard first and restore it after —
    // a test must never clobber real clipboard contents.
    let saved = read();
    let val = "aiTerminal-clipboard-test-世界-🚀";
    write(val);
    let got = read();
    if let Some(prev) = saved {
        write(&prev);
    }
    assert_eq!(got.as_deref(), Some(val));
}

#[test]
fn a_text_clipboard_is_not_an_image() {
    // Save and restore the person's real pasteboard, like every clipboard test.
    let saved = read();
    write("aiTerminal-clipboard-image-probe");
    assert!(read_image().is_none(), "text on the pasteboard must not decode as an image");
    if let Some(s) = saved {
        write(&s);
    }
}
