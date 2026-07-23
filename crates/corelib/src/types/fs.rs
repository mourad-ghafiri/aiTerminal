//! Filesystem volume metadata — the pure-data half of the
//! `platform::os::volumes()` seam (a file browser's "roots").

/// A mounted volume / partition: its display name, mount path, and capacity in
/// bytes (`total`/`free` are `0` when the OS backend can't report them).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Volume {
    pub name: String,
    pub path: String,
    pub total: u64,
    pub free: u64,
}
