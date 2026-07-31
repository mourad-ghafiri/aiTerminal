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
mod tests;
