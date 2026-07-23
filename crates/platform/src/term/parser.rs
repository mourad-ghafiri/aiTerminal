//! A compact ANSI/VT escape-sequence parser based on the Paul Williams VT500
//! state machine. It is decoupled from the screen model via the [`Perform`]
//! trait so it can be unit-tested in isolation, and it decodes UTF-8 in the
//! ground state so printed text is `char`-level.

/// Hard cap on the OSC accumulation buffer — a malicious/unterminated `ESC ] …` can't grow
/// an unbounded `Vec` (excess bytes are ignored until the terminator).
const MAX_OSC: usize = 1 << 20; // 1 MiB

/// The screen model implements this; the parser calls it as it recognizes input.
pub trait Perform {
    /// A printable character.
    fn print(&mut self, c: char);
    /// A C0/C1 control byte (BS, HT, LF, CR, …).
    fn execute(&mut self, byte: u8);
    /// A CSI sequence: `ESC [ params intermediates final`.
    fn csi(&mut self, params: &[u16], intermediates: &[u8], private: Option<u8>, action: u8);
    /// A non-CSI escape: `ESC intermediates final`.
    fn esc(&mut self, intermediates: &[u8], action: u8);
    /// An OSC string already split on `;` into fields.
    fn osc(&mut self, fields: &[&[u8]]);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    OscString,
}

const MAX_PARAMS: usize = 16;
const MAX_INTERMEDIATES: usize = 2;

pub struct Parser {
    state: State,
    params: [u16; MAX_PARAMS],
    n_params: usize,
    cur_param: u32,
    has_param: bool,
    intermediates: [u8; MAX_INTERMEDIATES],
    n_intermediates: usize,
    private: Option<u8>,
    osc: Vec<u8>,
    utf8: Utf8Decoder,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Parser {
            state: State::Ground,
            params: [0; MAX_PARAMS],
            n_params: 0,
            cur_param: 0,
            has_param: false,
            intermediates: [0; MAX_INTERMEDIATES],
            n_intermediates: 0,
            private: None,
            osc: Vec::new(),
            utf8: Utf8Decoder::new(),
        }
    }

    pub fn feed<P: Perform>(&mut self, bytes: &[u8], p: &mut P) {
        for &b in bytes {
            self.advance(b, p);
        }
    }

    fn clear(&mut self) {
        self.n_params = 0;
        self.cur_param = 0;
        self.has_param = false;
        self.n_intermediates = 0;
        self.private = None;
    }

    fn push_param(&mut self) {
        if self.n_params < MAX_PARAMS {
            self.params[self.n_params] = self.cur_param.min(u16::MAX as u32) as u16;
            self.n_params += 1;
        }
        self.cur_param = 0;
        self.has_param = false;
    }

    fn finish_params(&mut self) {
        // a trailing or sole value (even default 0) becomes a param
        if self.has_param || self.n_params == 0 {
            self.push_param();
        }
    }

    fn advance<P: Perform>(&mut self, b: u8, p: &mut P) {
        // ESC, CAN, SUB and C0 controls (except in OSC) can interrupt anywhere.
        match self.state {
            State::Ground => self.ground(b, p),
            State::Escape => self.escape(b, p),
            State::EscapeIntermediate => self.escape_intermediate(b, p),
            State::CsiEntry => self.csi_entry(b, p),
            State::CsiParam => self.csi_param(b, p),
            State::CsiIntermediate => self.csi_intermediate(b, p),
            State::CsiIgnore => self.csi_ignore(b),
            State::OscString => self.osc_string(b, p),
        }
    }

    fn ground<P: Perform>(&mut self, b: u8, p: &mut P) {
        match b {
            0x1b => self.state = State::Escape,
            0x00..=0x1f => p.execute(b),
            _ => {
                if let Some(c) = self.utf8.feed(b) {
                    p.print(c);
                }
            }
        }
    }

    fn escape<P: Perform>(&mut self, b: u8, p: &mut P) {
        self.clear();
        match b {
            b'[' => self.state = State::CsiEntry,
            b']' => {
                self.osc.clear();
                self.state = State::OscString;
            }
            0x20..=0x2f => {
                self.intermediates[0] = b;
                self.n_intermediates = 1;
                self.state = State::EscapeIntermediate;
            }
            0x30..=0x7e => {
                p.esc(&[], b);
                self.state = State::Ground;
            }
            0x1b => {} // stay, new ESC
            0x00..=0x1f => p.execute(b),
            _ => self.state = State::Ground,
        }
    }

    fn escape_intermediate<P: Perform>(&mut self, b: u8, p: &mut P) {
        match b {
            0x20..=0x2f => {
                if self.n_intermediates < MAX_INTERMEDIATES {
                    self.intermediates[self.n_intermediates] = b;
                    self.n_intermediates += 1;
                }
            }
            0x30..=0x7e => {
                let inter = self.intermediates[..self.n_intermediates].to_vec();
                p.esc(&inter, b);
                self.state = State::Ground;
            }
            _ => self.state = State::Ground,
        }
    }

    fn csi_entry<P: Perform>(&mut self, b: u8, p: &mut P) {
        match b {
            b'0'..=b'9' => {
                self.cur_param = (b - b'0') as u32;
                self.has_param = true;
                self.state = State::CsiParam;
            }
            b';' => {
                self.push_param();
                self.state = State::CsiParam;
            }
            0x3c..=0x3f => {
                self.private = Some(b);
                self.state = State::CsiParam;
            }
            0x20..=0x2f => {
                self.intermediates[0] = b;
                self.n_intermediates = 1;
                self.state = State::CsiIntermediate;
            }
            0x40..=0x7e => self.dispatch_csi(b, p),
            0x00..=0x1f => p.execute(b),
            _ => self.state = State::CsiIgnore,
        }
    }

    fn csi_param<P: Perform>(&mut self, b: u8, p: &mut P) {
        match b {
            b'0'..=b'9' => {
                self.cur_param = self.cur_param.saturating_mul(10) + (b - b'0') as u32;
                self.has_param = true;
            }
            b';' => self.push_param(),
            0x20..=0x2f => {
                if self.has_param {
                    self.push_param();
                }
                self.intermediates[0] = b;
                self.n_intermediates = 1;
                self.state = State::CsiIntermediate;
            }
            0x40..=0x7e => self.dispatch_csi(b, p),
            0x00..=0x1f => p.execute(b),
            _ => self.state = State::CsiIgnore,
        }
    }

    fn csi_intermediate<P: Perform>(&mut self, b: u8, p: &mut P) {
        match b {
            0x20..=0x2f => {
                if self.n_intermediates < MAX_INTERMEDIATES {
                    self.intermediates[self.n_intermediates] = b;
                    self.n_intermediates += 1;
                }
            }
            0x40..=0x7e => self.dispatch_csi(b, p),
            _ => self.state = State::CsiIgnore,
        }
    }

    fn csi_ignore(&mut self, b: u8) {
        if (0x40..=0x7e).contains(&b) {
            self.state = State::Ground;
        }
    }

    fn dispatch_csi<P: Perform>(&mut self, action: u8, p: &mut P) {
        self.finish_params();
        let inter = self.intermediates[..self.n_intermediates].to_vec();
        p.csi(&self.params[..self.n_params], &inter, self.private, action);
        self.state = State::Ground;
    }

    fn osc_string<P: Perform>(&mut self, b: u8, p: &mut P) {
        match b {
            0x07 => {
                // BEL terminator
                self.dispatch_osc(p);
                self.state = State::Ground;
            }
            0x1b => {
                // possible ST: next byte should be '\'. Peek via a tiny substate
                // by reusing Escape — but simplest: treat ESC as terminator and
                // re-enter escape so the following '\' is consumed harmlessly.
                self.dispatch_osc(p);
                self.state = State::Escape;
            }
            0x00..=0x06 | 0x08..=0x1f => { /* ignore control inside OSC */ }
            // Cap the OSC buffer so a malicious/unterminated sequence can't grow an
            // unbounded Vec (OOM); excess bytes are ignored until the terminator.
            _ if self.osc.len() < MAX_OSC => self.osc.push(b),
            _ => {}
        }
    }

    fn dispatch_osc<P: Perform>(&mut self, p: &mut P) {
        let fields: Vec<&[u8]> = self.osc.split(|&c| c == b';').collect();
        p.osc(&fields);
        self.osc.clear();
    }
}

/// Minimal incremental UTF-8 decoder. Emits U+FFFD on malformed input.
struct Utf8Decoder {
    buf: [u8; 4],
    needed: usize,
    have: usize,
}

impl Utf8Decoder {
    fn new() -> Self {
        Utf8Decoder { buf: [0; 4], needed: 0, have: 0 }
    }

    fn feed(&mut self, b: u8) -> Option<char> {
        if self.needed == 0 {
            // leading byte
            if b < 0x80 {
                return Some(b as char);
            } else if b >> 5 == 0b110 {
                self.needed = 2;
            } else if b >> 4 == 0b1110 {
                self.needed = 3;
            } else if b >> 3 == 0b11110 {
                self.needed = 4;
            } else {
                return Some('\u{FFFD}'); // stray continuation / invalid lead
            }
            self.buf[0] = b;
            self.have = 1;
            None
        } else {
            if b >> 6 != 0b10 {
                // invalid continuation: reset and reinterpret this byte fresh
                self.needed = 0;
                self.have = 0;
                return self.feed(b).or(Some('\u{FFFD}'));
            }
            self.buf[self.have] = b;
            self.have += 1;
            if self.have == self.needed {
                let n = self.needed;
                self.needed = 0;
                self.have = 0;
                match core::str::from_utf8(&self.buf[..n]) {
                    Ok(s) => s.chars().next(),
                    Err(_) => Some('\u{FFFD}'),
                }
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
}
