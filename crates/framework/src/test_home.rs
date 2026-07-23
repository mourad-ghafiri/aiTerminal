//! Test-only support: serialize the process-global `$HOME` across the tests that depend
//! on it (HOME-rooted config + `~` workspace resolution) and **restore it on drop** — so
//! these tests never race each other or leak a temp `$HOME` into an unrelated test. All
//! HOME-touching tests in the crate must go through [`lock_home`].

use std::sync::{Mutex, MutexGuard};

static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Holds the `$HOME` lock for a test's duration and restores the previous value on drop.
pub(crate) struct HomeGuard {
    prev: Option<std::ffi::OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Lock `$HOME` to a fresh temp dir for the test's duration (restored on drop). Keep the
/// returned guard alive for the whole test; the dir is a clean `tt-home-<tag>-<pid>`.
pub(crate) fn lock_home(tag: &str) -> (HomeGuard, std::path::PathBuf) {
    let lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("tt-home-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let prev = std::env::var_os("HOME");
    std::env::set_var("HOME", &dir);
    (HomeGuard { prev, _lock: lock }, dir)
}
