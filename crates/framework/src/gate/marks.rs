//! Semantic command marks — how a gate knows a remote command actually finished.
//!
//! Sniffing the user's prompt is a footgun (every prompt theme is different) and a
//! quiet-period timer fires in the middle of a slow build. But a gate *spawns* the
//! shell it relays, so it owns that shell's environment — which means it can simply
//! ask the shell to say so:
//!
//! ```text
//! ESC ] 1339 ; S BEL                a command starts here      (preexec)
//! ESC ] 1339 ; E ; <status> BEL     …and it exited <status>    (precmd)
//! ```
//!
//! `1339` continues the private OSC range this terminal already uses (`1337` iTerm
//! cwd, `1338` inline diagrams). [`MarkScanner`] pulls these out of the PTY stream
//! and **removes them**, so the bytes forwarded to the pane never contain an escape
//! the outer terminal would have to guess about.
//!
//! The scanner is resumable across read boundaries (a mark can be split over three
//! separate `read` calls) and bounded: anything that isn't one of our marks is
//! replayed byte-for-byte. The scanner can only ever pass bytes through unchanged or
//! swallow its own marks — it never rewrites someone else's escape sequence.

/// A semantic mark emitted by the gated shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// `preexec` — the shell is about to run a command.
    Start,
    /// `precmd` — the previous command exited with this status.
    End(i32),
}

/// The OSC body that identifies one of our marks, after `ESC ]`.
const PREFIX: &[u8] = b"1339;";
/// A mark payload is `S` or `E;<status>`; anything longer is not ours, so give up
/// early rather than buffering an unbounded OSC string.
const MAX_PAYLOAD: usize = 24;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    /// Saw `ESC`.
    Esc,
    /// Saw `ESC ]` and `n` matching bytes of [`PREFIX`].
    Prefix(usize),
    /// Inside our payload, collecting until `BEL` or `ESC \`.
    Payload,
    /// Saw `ESC` inside our payload — possibly the start of an `ESC \` terminator.
    PayloadEsc,
}

/// A resumable scanner that separates gate marks from ordinary terminal output.
pub struct MarkScanner {
    state: State,
    payload: Vec<u8>,
}

impl Default for MarkScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkScanner {
    pub fn new() -> Self {
        MarkScanner { state: State::Ground, payload: Vec::with_capacity(MAX_PAYLOAD) }
    }

    /// Consume `input`, appending everything that is *not* one of our marks to `out`
    /// and every recognized mark to `marks`.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>, marks: &mut Vec<Mark>) {
        let mut i = 0;
        while i < input.len() {
            // `step` returns false when it bailed out of a partial match: it has already
            // replayed the literal bytes it was holding, so this byte is re-read from
            // Ground rather than being lost.
            if self.step(input[i], out, marks) {
                i += 1;
            }
        }
    }

    /// Process one byte. Returns `false` if the byte must be re-processed after a bail.
    fn step(&mut self, b: u8, out: &mut Vec<u8>, marks: &mut Vec<Mark>) -> bool {
        match self.state {
            State::Ground => {
                if b == 0x1b {
                    self.state = State::Esc;
                } else {
                    out.push(b);
                }
                true
            }
            State::Esc => match b {
                b']' => {
                    self.state = State::Prefix(0);
                    true
                }
                // `ESC ESC`: the first one wasn't the start of anything we handle, but
                // the second one might be — flush one and stay armed.
                0x1b => {
                    out.push(0x1b);
                    true
                }
                _ => {
                    out.push(0x1b);
                    self.state = State::Ground;
                    false
                }
            },
            State::Prefix(n) => {
                if b == PREFIX[n] {
                    self.state = if n + 1 == PREFIX.len() { State::Payload } else { State::Prefix(n + 1) };
                    self.payload.clear();
                    true
                } else {
                    // Some other OSC (a title, a cwd, a diagram) — hand back what we held.
                    out.extend_from_slice(&[0x1b, b']']);
                    out.extend_from_slice(&PREFIX[..n]);
                    self.state = State::Ground;
                    false
                }
            }
            State::Payload => match b {
                0x07 => {
                    self.emit(marks);
                    true
                }
                0x1b => {
                    self.state = State::PayloadEsc;
                    true
                }
                _ if self.payload.len() >= MAX_PAYLOAD => {
                    self.bail(out);
                    false
                }
                _ => {
                    self.payload.push(b);
                    true
                }
            },
            State::PayloadEsc => {
                if b == b'\\' {
                    self.emit(marks);
                    true
                } else {
                    // Not an ST after all; replay the payload AND the escape we swallowed.
                    self.bail(out);
                    out.push(0x1b);
                    false
                }
            }
        }
    }

    /// Terminator reached: turn the buffered payload into a mark. An unrecognized
    /// payload is dropped — `1339` is our namespace, so a future mark this build
    /// doesn't know about must not leak escape bytes into the pane.
    fn emit(&mut self, marks: &mut Vec<Mark>) {
        match self.payload.split_first() {
            Some((b'S', rest)) if rest.is_empty() => marks.push(Mark::Start),
            Some((b'E', rest)) => {
                let status = std::str::from_utf8(rest.strip_prefix(b";").unwrap_or(rest))
                    .ok()
                    .and_then(|s| s.trim().parse::<i32>().ok())
                    .unwrap_or(-1);
                marks.push(Mark::End(status));
            }
            _ => {}
        }
        self.payload.clear();
        self.state = State::Ground;
    }

    /// Give up on a partial match, replaying every byte we were holding verbatim.
    fn bail(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[0x1b, b']']);
        out.extend_from_slice(PREFIX);
        out.extend_from_slice(&self.payload);
        self.payload.clear();
        self.state = State::Ground;
    }
}

#[cfg(test)]
mod tests {
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
}
