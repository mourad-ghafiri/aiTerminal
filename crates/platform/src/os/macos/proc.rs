//! Process utilities for the headless CLI — a SIGINT flag, pid liveness, and
//! session-detached spawning. The one place (besides the PTY) that talks to the
//! process-control syscalls; everything is exposed through `platform::os`.

use std::os::raw::{c_int, c_ulong};
use std::sync::atomic::{AtomicBool, Ordering};

extern "C" {
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn signal(sig: c_int, handler: usize) -> usize;
    fn sigaction(sig: c_int, act: *const SigAction, old: *mut SigAction) -> c_int;
    fn setsid() -> c_int;
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn tcgetattr(fd: c_int, termios: *mut Termios) -> c_int;
    fn tcsetattr(fd: c_int, actions: c_int, termios: *const Termios) -> c_int;
    fn cfmakeraw(termios: *mut Termios);
}

/// macOS `struct sigaction` (`<sys/signal.h>`): the handler union (a pointer), a `sigset_t` mask
/// (`__uint32_t` on macOS), and the flags word. Used to install `SIGWINCH` **without** `SA_RESTART`
/// so a resize interrupts a blocking `read` (whereas `signal()` sets `SA_RESTART` and it wouldn't).
#[repr(C)]
struct SigAction {
    sa_handler: usize,
    sa_mask: u32,
    sa_flags: c_int,
}

const SIGINT: c_int = 2;
/// `SIGPIPE` — raised when a write lands on a pipe whose reader has gone.
const SIGPIPE: c_int = 13;
const SIGTERM: c_int = 15;
/// `SIGWINCH` — the kernel raises it on a controlling-terminal resize.
const SIGWINCH: c_int = 28;
/// `TIOCGWINSZ` on macOS/BSD — `_IOR('t', 104, struct winsize)`.
const TIOCGWINSZ: c_ulong = 0x4008_7468;
/// `tcsetattr` action: apply after the output buffer drains and flush pending input.
const TCSAFLUSH: c_int = 2;

/// macOS/BSD `struct termios` (`<termios.h>`): four `tcflag_t` (unsigned long) flag words,
/// a control-char array, and the two speed fields. Laid out exactly so `tcgetattr`/`tcsetattr`
/// read and write it correctly.
#[repr(C)]
#[derive(Clone, Copy)]
struct Termios {
    c_iflag: c_ulong,
    c_oflag: c_ulong,
    c_cflag: c_ulong,
    c_lflag: c_ulong,
    c_cc: [u8; 20], // NCCS = 20 on macOS
    c_ispeed: c_ulong,
    c_ospeed: c_ulong,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

/// The controlling terminal's `(cols, rows)` via `TIOCGWINSZ` on stderr — the CLI's true
/// pane size (independent of whether the shell exported `$COLUMNS`). `None` off a tty.
pub fn terminal_size() -> Option<(u16, u16)> {
    let mut ws = Winsize::default();
    // SAFETY: `ws` is a valid, correctly-sized winsize buffer; fd 2 is stderr.
    let rc = unsafe { ioctl(2, TIOCGWINSZ, &mut ws as *mut Winsize) };
    if rc == 0 && ws.ws_col > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

static SIGINT_HIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_sig: c_int) {
    // Async-signal-safe: a single relaxed store.
    SIGINT_HIT.store(true, Ordering::Relaxed);
}

/// Install (once) a SIGINT handler that only sets a flag, and return the flag.
/// The caller polls it and drives its own cooperative cancellation (the engine's
/// `CancelToken`), so Ctrl+C becomes a clean stop instead of a hard kill.
pub fn sigint_flag() -> &'static AtomicBool {
    use std::sync::Once;
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| unsafe {
        signal(SIGINT, on_sigint as *const () as usize);
    });
    &SIGINT_HIT
}

/// Restore the default disposition for `SIGPIPE`, so a closed pipe ends the process
/// quietly instead of panicking.
///
/// The Rust runtime sets `SIGPIPE` to `SIG_IGN` before `main`. That is right for a
/// server — a dead client should not kill it — and wrong for a command: writes to the
/// gone pipe return `EPIPE`, and `println!` panics on a write error. So
/// `aiTerminal ai flow | head -2` can die with a backtrace where every other Unix
/// tool exits silently, and it does it *intermittently*, only when the reader happens
/// to leave between two writes.
///
/// `SIG_DFL` is `0`, and this is the one call the standard fix needs.
pub fn restore_sigpipe() {
    use std::sync::Once;
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| unsafe {
        signal(SIGPIPE, 0);
    });
}

/// Whether `pid` is a live process (`kill(pid, 0)` succeeds). Used to reconcile
/// job records whose owner crashed or was killed.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe { kill(pid as c_int, 0) == 0 }
}

/// Send SIGTERM to `pid` (cancel a detached/scheduled job). Returns true on delivery.
pub fn terminate(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe { kill(pid as c_int, SIGTERM) == 0 }
}

/// Puts the controlling terminal (fd 0) into raw mode until dropped — canonical/echo/signal
/// processing off, so a full-screen app reads keystrokes one at a time and owns the screen.
/// [`raw_mode`] returns one; dropping it restores the terminal exactly as it was (on normal
/// exit, an early return, OR a panic unwind), so the shell is never left in a broken state.
pub struct RawGuard {
    fd: c_int,
    saved: Termios,
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        // SAFETY: `saved` is the exact termios we captured from this fd in `raw_mode`.
        unsafe {
            tcsetattr(self.fd, TCSAFLUSH, &self.saved);
        }
    }
}

/// Enter raw mode on stdin (fd 0). Returns a restore guard, or `None` if stdin isn't a tty
/// (so callers can print a hint instead of scrambling a pipe). `VMIN=1/VTIME=0` → each `read`
/// blocks until at least one byte, which pairs with EINTR-on-signal for resize handling.
pub fn raw_mode() -> Option<RawGuard> {
    let fd: c_int = 0;
    let mut saved = Termios {
        c_iflag: 0,
        c_oflag: 0,
        c_cflag: 0,
        c_lflag: 0,
        c_cc: [0; 20],
        c_ispeed: 0,
        c_ospeed: 0,
    };
    // SAFETY: `saved` is a valid termios buffer; fd 0 is stdin. A non-tty fd fails cleanly.
    unsafe {
        if tcgetattr(fd, &mut saved) != 0 {
            return None;
        }
        let mut raw = saved;
        cfmakeraw(&mut raw); // also sets VMIN=1/VTIME=0; we re-assert them for clarity
        raw.c_cc[16] = 1; // VMIN  = 1 (block until ≥1 byte)
        raw.c_cc[17] = 0; // VTIME = 0 (no inter-byte timer)
        if tcsetattr(fd, TCSAFLUSH, &raw) != 0 {
            return None;
        }
    }
    Some(RawGuard { fd, saved })
}

static SIGWINCH_HIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigwinch(_sig: c_int) {
    // Async-signal-safe: a single relaxed store.
    SIGWINCH_HIT.store(true, Ordering::Relaxed);
}

/// Install (once) a `SIGWINCH` handler that sets a flag, and return the flag. A full-screen
/// app's blocking `read` returns `EINTR` when the signal fires; the app then re-queries
/// [`terminal_size`] and repaints. The caller clears the flag after handling it.
///
/// Installed via `sigaction` with `sa_flags = 0` — crucially **not** `SA_RESTART`. `signal()` on
/// macOS/BSD sets `SA_RESTART`, which auto-restarts the interrupted `read` so the app would never
/// learn about the resize until the next keypress; clearing it makes `read` return `EINTR`.
pub fn sigwinch_flag() -> &'static AtomicBool {
    use std::sync::Once;
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let act = SigAction { sa_handler: on_sigwinch as *const () as usize, sa_mask: 0, sa_flags: 0 };
        // SAFETY: `act` is a valid sigaction for the process-lifetime handler `on_sigwinch`.
        unsafe {
            sigaction(SIGWINCH, &act, std::ptr::null_mut());
        }
    });
    &SIGWINCH_HIT
}

/// Spawn `program args…` in its OWN SESSION (`setsid` in the child before exec),
/// stdin null and stdout/stderr redirected to the given files — a background job
/// that survives the launching terminal closing (no SIGHUP from its group).
pub fn spawn_detached(
    program: &std::path::Path,
    args: &[String],
    stdout: std::fs::File,
    stderr: std::fs::File,
) -> std::io::Result<u32> {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr));
    unsafe {
        cmd.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
    Ok(cmd.spawn()?.id())
}

#[cfg(test)]
mod tests;
