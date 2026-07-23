//! Process utilities for the headless CLI — a SIGINT flag, pid liveness, and
//! session-detached spawning. The one place (besides the PTY) that talks to the
//! process-control syscalls; everything is exposed through `platform::os`.

use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};

extern "C" {
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn signal(sig: c_int, handler: usize) -> usize;
    fn setsid() -> c_int;
}

const SIGINT: c_int = 2;

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

/// Whether `pid` is a live process (`kill(pid, 0)` succeeds). Used to reconcile
/// job records whose owner crashed or was killed.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe { kill(pid as c_int, 0) == 0 }
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
mod tests {
    #[test]
    fn pid_liveness_reflects_reality() {
        // Our own pid is alive; pid 0 is never "a job"; a far-out pid is (almost
        // surely) dead — the reconciliation predicate the job list relies on.
        assert!(super::pid_alive(std::process::id()));
        assert!(!super::pid_alive(0));
        assert!(!super::pid_alive(3_999_999));
    }

    #[test]
    fn sigint_flag_installs_once_and_starts_clear() {
        let f = super::sigint_flag();
        assert!(!f.load(std::sync::atomic::Ordering::Relaxed));
        let _ = super::sigint_flag(); // idempotent
    }
}
