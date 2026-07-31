use super::*;

#[test]
fn a_recording_sink_mirrors_exactly_what_it_writes() {
    // The invariant the whole screenshot feature rests on.
    let mut s = RecordingSink::with_mirror(20, 4);
    s.emit(b"hello");
    s.emit(b"\r\nworld");
    assert_eq!(s.text(), "hello\r\nworld");
    let term = s.term.as_ref().unwrap();
    let row0: String = term.row(0).iter().map(|c| c.ch).collect();
    let row1: String = term.row(1).iter().map(|c| c.ch).collect();
    assert_eq!(row0.trim_end(), "hello");
    assert_eq!(row1.trim_end(), "world");
}

#[test]
fn the_mirror_tracks_the_alternate_screen() {
    // How `/shot` and the dispatch guard both know a full-screen program is up.
    let mut s = RecordingSink::with_mirror(20, 4);
    assert!(!s.term.as_ref().unwrap().in_alt_screen());
    s.emit(b"\x1b[?1049h");
    assert!(s.term.as_ref().unwrap().in_alt_screen());
    s.emit(b"\x1b[?1049l");
    assert!(!s.term.as_ref().unwrap().in_alt_screen());
}
