use super::*;

/// Run a whole input through a fresh scanner.
fn scan(input: &[u8]) -> (Vec<u8>, Vec<Mark>) {
    let (mut out, mut marks) = (Vec::new(), Vec::new());
    MarkScanner::new().feed(input, &mut out, &mut marks);
    (out, marks)
}

#[test]
fn extracts_start_and_end_and_removes_them_from_the_stream() {
    let (out, marks) = scan(b"\x1b]1339;S\x07ls -la\r\ntotal 4\r\n\x1b]1339;E;0\x07");
    assert_eq!(out, b"ls -la\r\ntotal 4\r\n");
    assert_eq!(marks, vec![Mark::Start, Mark::End(0)]);
}

#[test]
fn a_nonzero_exit_status_survives() {
    let (_, marks) = scan(b"\x1b]1339;E;127\x07");
    assert_eq!(marks, vec![Mark::End(127)]);
}

#[test]
fn accepts_the_st_terminator_as_well_as_bel() {
    let (out, marks) = scan(b"a\x1b]1339;S\x1b\\b");
    assert_eq!(out, b"ab");
    assert_eq!(marks, vec![Mark::Start]);
}

#[test]
fn a_mark_split_across_reads_is_still_recognized() {
    // The realistic failure: a 4 KiB PTY read lands mid-escape.
    let full = b"x\x1b]1339;E;3\x07y";
    for split in 1..full.len() {
        let (mut out, mut marks) = (Vec::new(), Vec::new());
        let mut s = MarkScanner::new();
        s.feed(&full[..split], &mut out, &mut marks);
        s.feed(&full[split..], &mut out, &mut marks);
        assert_eq!(out, b"xy", "split at {split}");
        assert_eq!(marks, vec![Mark::End(3)], "split at {split}");
    }
}

#[test]
fn one_byte_at_a_time_behaves_identically() {
    let full = b"\x1b]1339;S\x07hi\x1b]1339;E;0\x07";
    let (mut out, mut marks) = (Vec::new(), Vec::new());
    let mut s = MarkScanner::new();
    for b in full {
        s.feed(&[*b], &mut out, &mut marks);
    }
    assert_eq!(out, b"hi");
    assert_eq!(marks, vec![Mark::Start, Mark::End(0)]);
}

#[test]
fn other_escape_sequences_pass_through_byte_for_byte() {
    // Titles, cwd reports, inline diagrams, colors, and a bare ESC-ESC. The scanner
    // sits in the middle of every byte the shell prints; anything it does not own
    // must come out the far side unchanged.
    for seq in [
        &b"\x1b]0;my title\x07"[..],
        &b"\x1b]7;file:///tmp\x07"[..],
        &b"\x1b]1338;4;Zm9v\x07"[..],
        &b"\x1b]133;A\x07"[..],
        &b"\x1b[38;2;1;2;3mcolored\x1b[0m"[..],
        &b"\x1b\x1b[A"[..],
        &b"\x1b]1\x07"[..],
    ] {
        let (out, marks) = scan(seq);
        assert_eq!(out, seq, "{:?} was altered", String::from_utf8_lossy(seq));
        assert!(marks.is_empty());
    }
}

#[test]
fn literal_mark_text_without_a_real_escape_is_not_a_mark() {
    // The shell ECHOES what it is told to run. If a chat sends the mark as text,
    // the echo must not be mistaken for the real thing and desync the capture.
    let text = br"echo '\033]1339;S\007' and ESC]1339;E;0";
    let (out, marks) = scan(text);
    assert_eq!(out, text);
    assert!(marks.is_empty());
}

#[test]
fn an_unterminated_payload_is_replayed_and_never_grows() {
    // A truncated or hostile OSC must not buffer without bound.
    let long = [&b"\x1b]1339;"[..], &b"A".repeat(4096)].concat();
    let (out, marks) = scan(&long);
    assert!(marks.is_empty());
    assert_eq!(out, long, "held bytes are replayed verbatim");
}

#[test]
fn an_unknown_payload_in_our_namespace_is_swallowed_not_leaked() {
    let (out, marks) = scan(b"a\x1b]1339;Z;9\x07b");
    assert_eq!(out, b"ab", "no stray escape reaches the pane");
    assert!(marks.is_empty());
}
