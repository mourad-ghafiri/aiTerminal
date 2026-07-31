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

#[test]
fn termios_matches_the_macos_abi_layout() {
    // The `#[repr(C)]` struct must match `<termios.h>` so tcgetattr/tcsetattr are correct:
    // 4 unsigned-long flag words + a 20-byte cc array + 2 unsigned-long speeds, 8-aligned.
    assert_eq!(std::mem::size_of::<super::Termios>(), 72);
    assert_eq!(std::mem::align_of::<super::Termios>(), 8);
}

#[test]
fn sigwinch_flag_installs_once_and_starts_clear() {
    let f = super::sigwinch_flag();
    assert!(!f.load(std::sync::atomic::Ordering::Relaxed));
    let _ = super::sigwinch_flag(); // idempotent
}

#[test]
fn sigaction_matches_the_macos_abi_layout() {
    // handler pointer (8) + sigset_t mask (4) + flags (4), 8-aligned → 16 bytes. Wrong here
    // and `sigaction` would scribble past the flags word or misread SA_RESTART.
    assert_eq!(std::mem::size_of::<super::SigAction>(), 16);
    assert_eq!(std::mem::align_of::<super::SigAction>(), 8);
}
