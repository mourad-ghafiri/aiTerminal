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
mod tests {
    use super::*;

    #[test]
    fn output_is_capped_and_the_child_still_finishes() {
        // 10 MB of output against a 4 KiB cap: memory stays at the cap, the child
        // runs to completion (the pipe keeps draining), nothing times out.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "yes | head -c 10000000"]);
        let t = Instant::now();
        let r = run_bounded(cmd, Duration::from_secs(30), 4096).unwrap();
        assert!(r.truncated);
        assert_eq!(r.stdout.len(), 4096, "exactly the cap is kept");
        assert!(!r.timed_out);
        assert!(matches!(r.status, Some(s) if s.success()));
        assert!(t.elapsed() < Duration::from_secs(10), "took {:?}", t.elapsed());
    }

    #[test]
    fn the_deadline_kills_the_child() {
        // A sleeping child must be killed AND reaped at the deadline — the whole
        // call returns promptly with `timed_out` (the zombie-leak regression).
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let t = Instant::now();
        let r = run_bounded(cmd, Duration::from_millis(200), 4096).unwrap();
        assert!(r.timed_out);
        assert!(r.status.is_none());
        assert!(t.elapsed() < Duration::from_secs(2), "took {:?}", t.elapsed());
    }

    #[test]
    fn read_tail_keeps_exactly_the_end() {
        let data: Vec<u8> = (0..10_000_000u32).map(|i| (i % 64) as u8 + 0x20).collect();
        let tail = read_tail(std::io::Cursor::new(data.clone()), 4000);
        assert_eq!(tail.len(), 4000);
        assert_eq!(tail.as_bytes(), &data[data.len() - 4000..]);
        // Shorter-than-keep input comes back whole.
        assert_eq!(read_tail(std::io::Cursor::new(b"abc".to_vec()), 4000), "abc");
    }
}
