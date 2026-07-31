use super::*;

#[derive(Default)]
struct Rec {
    prints: String,
    execs: Vec<u8>,
    csis: Vec<(Vec<u16>, Option<u8>, u8)>,
    escs: Vec<u8>,
    oscs: Vec<Vec<String>>,
}
impl Perform for Rec {
    fn print(&mut self, c: char) {
        self.prints.push(c);
    }
    fn execute(&mut self, b: u8) {
        self.execs.push(b);
    }
    fn csi(&mut self, params: &[u16], _inter: &[u8], private: Option<u8>, action: u8) {
        self.csis.push((params.to_vec(), private, action));
    }
    fn esc(&mut self, _inter: &[u8], action: u8) {
        self.escs.push(action);
    }
    fn osc(&mut self, fields: &[&[u8]]) {
        self.oscs
            .push(fields.iter().map(|f| String::from_utf8_lossy(f).into_owned()).collect());
    }
}

fn run(input: &[u8]) -> Rec {
    let mut p = Parser::new();
    let mut r = Rec::default();
    p.feed(input, &mut r);
    r
}

#[test]
fn plain_text_prints() {
    assert_eq!(run(b"hello").prints, "hello");
}

#[test]
fn utf8_multibyte_decodes() {
    assert_eq!(run("héllo→世".as_bytes()).prints, "héllo→世");
}

#[test]
fn controls_execute() {
    let r = run(b"a\r\nb");
    assert_eq!(r.prints, "ab");
    assert_eq!(r.execs, vec![b'\r', b'\n']);
}

#[test]
fn csi_cursor_position() {
    let r = run(b"\x1b[10;20H");
    assert_eq!(r.csis, vec![(vec![10, 20], None, b'H')]);
}

#[test]
fn csi_default_param() {
    let r = run(b"\x1b[H");
    assert_eq!(r.csis, vec![(vec![0], None, b'H')]);
}

#[test]
fn csi_private_mode() {
    let r = run(b"\x1b[?25l");
    assert_eq!(r.csis, vec![(vec![25], Some(b'?'), b'l')]);
}

#[test]
fn sgr_multiple_params() {
    let r = run(b"\x1b[1;38;5;200m");
    assert_eq!(r.csis, vec![(vec![1, 38, 5, 200], None, b'm')]);
}

#[test]
fn osc_title_bel_terminated() {
    let r = run(b"\x1b]0;my title\x07");
    assert_eq!(r.oscs, vec![vec!["0".to_string(), "my title".to_string()]]);
}

#[test]
fn osc_st_terminated() {
    let r = run(b"\x1b]2;hi\x1b\\rest");
    assert_eq!(r.oscs, vec![vec!["2".to_string(), "hi".to_string()]]);
    assert_eq!(r.prints, "rest");
}

#[test]
fn esc_designate_charset_ignored_cleanly() {
    // ESC ( B  (select ASCII) then text
    let r = run(b"\x1b(Bok");
    assert_eq!(r.prints, "ok");
    assert_eq!(r.escs, vec![b'B']);
}

#[test]
fn control_interrupt_resets_a_partial_utf8_sequence() {
    // A lead byte, then an ESC sequence, then a continuation byte: the stale partial
    // must NOT absorb the continuation into a corrupt glyph. It resets to a replacement
    // char, and the (now-standalone) continuation byte is its own replacement.
    let r = run(&[0xE4, 0x1b, b'[', b'm', 0xBD, b'A']);
    assert!(r.prints.ends_with('A'), "the trailing ASCII prints cleanly: {:?}", r.prints);
    assert!(r.prints.contains('\u{FFFD}'), "the broken bytes became replacement chars");
    assert_eq!(r.csis.len(), 1, "the ESC[m in the middle still dispatched");
    // A CLEAN multibyte char split only by nothing still decodes (no false reset).
    let ok = run("é".as_bytes());
    assert_eq!(ok.prints, "é");
}

#[test]
fn huge_numeric_param_does_not_overflow() {
    // `ESC[9999999999m` overflowed u32 (`saturating_mul` then a plain add) → debug panic.
    // It must parse, saturate to u16::MAX, and dispatch cleanly.
    let r = run(b"\x1b[9999999999m");
    assert_eq!(r.csis.len(), 1);
    assert_eq!(r.csis[0].2, b'm');
    assert_eq!(r.csis[0].0, vec![u16::MAX]);
}
