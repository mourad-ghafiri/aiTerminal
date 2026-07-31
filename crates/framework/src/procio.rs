//! Bounded subprocess I/O — the one way framework code runs a child whose output
//! it captures. Two invariants, enforced here so no call site can forget them:
//!
//!   * **Output is capped.** A child that prints gigabytes (`cat huge.log`,
//!     `yes`, a verbose test run) costs at most `cap` bytes of memory per pipe;
//!     the rest is drained and dropped so the child never blocks on a full pipe.
//!   * **The deadline kills.** A hung child is `kill()`ed and `wait()`ed at the
//!     deadline — a timeout that leaves the process running is a leak, not a
//!     timeout.
#![forbid(unsafe_code)]

use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// The outcome of a [`run_bounded`] call.
#[derive(Debug, Default)]
pub struct Bounded {
    /// `None` when the child was killed at the deadline.
    pub status: Option<ExitStatus>,
    pub stdout: String,
    pub stderr: String,
    /// Whether either stream was cut at the cap.
    pub truncated: bool,
    /// Whether the deadline fired (the child was killed + reaped).
    pub timed_out: bool,
}

/// Run `cmd` with piped, capped stdio and a hard deadline. `cap` bounds EACH of
/// stdout/stderr. stdin is null.
pub fn run_bounded(mut cmd: Command, deadline: Duration, cap: usize) -> std::io::Result<Bounded> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let so = child.stdout.take();
    let se = child.stderr.take();
    let out_h = std::thread::spawn(move || so.map(|h| capped_read(h, cap)).unwrap_or_default());
    let err_h = std::thread::spawn(move || se.map(|h| capped_read(h, cap)).unwrap_or_default());
    let started = Instant::now();
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(st)) => break (Some(st), false),
            Err(_) => break (None, false),
            Ok(None) => {
                if started.elapsed() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break (None, true);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    };
    // The drain threads end at pipe EOF, which the exit (or kill) guarantees.
    let (stdout, t_out) = out_h.join().unwrap_or_default();
    let (stderr, t_err) = err_h.join().unwrap_or_default();
    Ok(Bounded { status, stdout, stderr, truncated: t_out || t_err, timed_out })
}

/// Read up to `cap` bytes from `r`, then keep DRAINING (and discarding) to EOF so
/// the writer never blocks on a full pipe. Returns `(text, truncated)`.
fn capped_read(mut r: impl Read, cap: usize) -> (String, bool) {
    let mut buf = Vec::new();
    let _ = (&mut r).take(cap as u64 + 1).read_to_end(&mut buf);
    let truncated = buf.len() > cap;
    if truncated {
        buf.truncate(cap);
        let mut sink = [0u8; 8192];
        while matches!(r.read(&mut sink), Ok(n) if n > 0) {}
    }
    (String::from_utf8_lossy(&buf).into_owned(), truncated)
}

/// Stream `r` to EOF keeping only the LAST `keep` bytes — constant memory no
/// matter how much the source produces (failures print last; the tail is what
/// verifiers need).
pub fn read_tail(mut r: impl Read, keep: usize) -> String {
    let mut ring: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                ring.extend_from_slice(&buf[..n]);
                if ring.len() > keep * 2 {
                    let cut = ring.len() - keep;
                    ring.drain(..cut);
                }
            }
        }
    }
    if ring.len() > keep {
        let cut = ring.len() - keep;
        ring.drain(..cut);
    }
    String::from_utf8_lossy(&ring).into_owned()
}

#[cfg(test)]
mod tests;
