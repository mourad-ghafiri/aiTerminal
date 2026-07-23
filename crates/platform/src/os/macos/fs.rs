//! Volume capacity via `statvfs(2)` — the one filesystem FFI we need (std has no
//! free-space API). Mount enumeration itself is pure `std::fs` and lives in
//! `os::volumes`; this module only turns a path into `(total, free)` bytes.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};

// macOS `struct statvfs` (`<sys/statvfs.h>`): the `unsigned long` fields are 64-bit
// on LP64; the block/inode counts are `fsblkcnt_t`/`fsfilcnt_t` = `unsigned int`
// (32-bit) on Darwin. Layout is `#[repr(C)]`-exact (the six u32 counts land at
// offset 16..40, leaving the trailing u64s 8-aligned with no inserted padding).
#[repr(C)]
struct Statvfs {
    f_bsize: u64,
    f_frsize: u64,
    f_blocks: u32,
    f_bfree: u32,
    f_bavail: u32,
    f_files: u32,
    f_ffree: u32,
    f_favail: u32,
    f_fsid: u64,
    f_flag: u64,
    f_namemax: u64,
}

extern "C" {
    fn statvfs(path: *const c_char, buf: *mut Statvfs) -> c_int;
}

/// `(total, free)` bytes for the volume containing `path`, or `(0, 0)` if the
/// path is unmappable or the syscall fails. `free` is space available to a
/// non-root user (`f_bavail`).
pub fn capacity(path: &str) -> (u64, u64) {
    let Ok(c) = CString::new(path) else { return (0, 0) };
    let mut buf: Statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `buf` is a correctly-sized, properly-aligned `statvfs`, and `c` is a
    // valid NUL-terminated C string that outlives the call.
    let rc = unsafe { statvfs(c.as_ptr(), &mut buf) };
    if rc != 0 {
        return (0, 0);
    }
    let frsize = buf.f_frsize;
    let total = frsize.saturating_mul(buf.f_blocks as u64);
    let free = frsize.saturating_mul(buf.f_bavail as u64);
    (total, free)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_volume_has_capacity() {
        let (total, free) = capacity("/");
        assert!(total > 0, "root volume reports a total size");
        assert!(free <= total);
    }

    #[test]
    fn bad_path_is_zero() {
        assert_eq!(capacity("/no/such/path/zzz"), (0, 0));
    }
}
