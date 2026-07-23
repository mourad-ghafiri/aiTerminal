//! `platform` — Ring-1 OS backends. The ONLY crate that contains FFI, `#[link]`,
//! or syscalls; everything above it depends on the `platform-api` trait seam.
//! macOS is the verified reference backend; Windows and Linux implement the same
//! seam in a later phase.

#[cfg(target_os = "macos")]
mod macos;

use std::io;

use crate::traits::{ImageDecoder, Pty, PtyCommand, TextShaper};

/// The default monospace font family for the host OS.
pub fn default_monospace() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Menlo"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "monospace"
    }
}

/// Spawn a child program (or the user's shell) attached to a new PTY.
#[cfg(target_os = "macos")]
pub fn spawn_pty(cmd: &PtyCommand) -> io::Result<Box<dyn Pty>> {
    Ok(Box::new(macos::pty::spawn(cmd)?))
}

/// The OS text shaper/rasterizer (CoreText on macOS).
#[cfg(target_os = "macos")]
pub fn text_shaper() -> Box<dyn TextShaper> {
    text_shaper_with(default_monospace())
}

/// The OS text shaper using a specific font family (empty → the default).
#[cfg(target_os = "macos")]
pub fn text_shaper_with(family: &str) -> Box<dyn TextShaper> {
    let fam = if family.trim().is_empty() { default_monospace() } else { family };
    Box::new(macos::coretext::MacShaper::new(fam))
}

#[cfg(not(target_os = "macos"))]
pub fn text_shaper_with(_family: &str) -> Box<dyn TextShaper> {
    panic!("platform text shaper not yet implemented for this OS")
}

/// The OS image decoder (CoreGraphics/ImageIO on macOS; PNG/JPEG/GIF/HEIC/…).
#[cfg(target_os = "macos")]
pub fn image_decoder() -> Box<dyn ImageDecoder> {
    Box::new(macos::image::CgImageDecoder)
}

#[cfg(not(target_os = "macos"))]
pub fn image_decoder() -> Box<dyn ImageDecoder> {
    /// A decoder that declines every image until the OS backend lands.
    struct NoDecoder;
    impl ImageDecoder for NoDecoder {
        fn decode(&self, _bytes: &[u8]) -> Option<crate::traits::DecodedImage> {
            None
        }
    }
    Box::new(NoDecoder)
}

/// The process-wide SIGINT flag (handler installed on first call) — the headless
/// CLI polls it to drive cooperative cancellation on Ctrl+C.
#[cfg(target_os = "macos")]
pub fn sigint_flag() -> &'static std::sync::atomic::AtomicBool {
    macos::proc::sigint_flag()
}

#[cfg(not(target_os = "macos"))]
pub fn sigint_flag() -> &'static std::sync::atomic::AtomicBool {
    static NEVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    &NEVER
}

/// Whether `pid` is a live process (job-record reconciliation).
#[cfg(target_os = "macos")]
pub fn pid_alive(pid: u32) -> bool {
    macos::proc::pid_alive(pid)
}

#[cfg(not(target_os = "macos"))]
pub fn pid_alive(_pid: u32) -> bool {
    true // never falsely mark a job dead where we can't check
}

/// Spawn a detached (own-session) background process with its output redirected —
/// survives the launching terminal closing.
#[cfg(target_os = "macos")]
pub fn spawn_detached(program: &std::path::Path, args: &[String], stdout: std::fs::File, stderr: std::fs::File) -> std::io::Result<u32> {
    macos::proc::spawn_detached(program, args, stdout, stderr)
}

#[cfg(not(target_os = "macos"))]
pub fn spawn_detached(program: &std::path::Path, args: &[String], stdout: std::fs::File, stderr: std::fs::File) -> std::io::Result<u32> {
    let child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr))
        .spawn()?;
    Ok(child.id())
}

/// Wake the (possibly blocked) OS event loop from any thread, so a freshly
/// dirtied frame renders now instead of at the next idle tick. No-op before the
/// loop exists, and on OSes without a windowing backend.
#[cfg(target_os = "macos")]
pub fn post_wake_event() {
    macos::window::post_wake_event()
}

#[cfg(not(target_os = "macos"))]
pub fn post_wake_event() {}

/// Boot the OS windowing platform (owns the window, GPU, and event loop).
#[cfg(target_os = "macos")]
pub fn boot() -> Box<dyn crate::traits::Platform> {
    macos::window::boot()
}

#[cfg(not(target_os = "macos"))]
pub fn boot() -> Box<dyn crate::traits::Platform> {
    panic!("windowing platform not yet implemented for this OS")
}

/// Write UTF-8 text to the system clipboard.
pub fn clipboard_write(text: &str) {
    #[cfg(target_os = "macos")]
    macos::clipboard::write(text);
    #[cfg(not(target_os = "macos"))]
    let _ = text;
}

/// Hand a URL (or file/folder) to the OS to open with the user's default handler —
/// the system browser for an http(s) URL, Finder/Preview/etc. for a path. Shells the
/// per-OS opener (`open` on macOS, `xdg-open` on Linux, `start` on Windows). Fire-and-
/// forget; an `Err` means the opener couldn't be spawned.
pub fn open_external(target: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    cmd.arg(target).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Read UTF-8 text from the system clipboard.
pub fn clipboard_read() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos::clipboard::read()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The local timezone's offset from UTC, in seconds (e.g. `+7200` for UTC+2). The OS
/// is the source of truth — resolved once via `date +%z` (the same shell-out anchor
/// the clipboard + curl use) and cached. `0` (UTC) when it can't be determined.
pub fn utc_offset_secs() -> i64 {
    use std::sync::OnceLock;
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        let out = std::process::Command::new("date").arg("+%z").output().ok();
        let z = out.and_then(|o| String::from_utf8(o.stdout).ok()).unwrap_or_default();
        let z = z.trim();
        // `+HHMM` / `-HHMM`
        if z.len() == 5 {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let h: i64 = z[1..3].parse().unwrap_or(0);
            let m: i64 = z[3..5].parse().unwrap_or(0);
            return sign * (h * 3600 + m * 60);
        }
        0
    })
}

/// Fill `buf` with cryptographically-random bytes from the OS CSPRNG. Returns
/// whether it succeeded (the caller falls back to a weaker seed on `false`). The
/// OS source is quarantined here so callers above the platform layer never touch
/// `/dev/urandom` / `BCryptGenRandom` directly. Unix reads `/dev/urandom`; other
/// targets land their own source with the OS backend.
pub fn random_bytes(buf: &mut [u8]) -> bool {
    #[cfg(unix)]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(buf)).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = buf;
        false
    }
}

/// The current user's home directory, resolved per-OS (`$HOME` on unix,
/// `%USERPROFILE%` on Windows). The single home-resolution seam so the dirs above
/// the platform layer are correct on every OS.
pub fn home_dir() -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(std::path::PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(std::path::PathBuf::from)
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// `(total, free)` bytes on the volume containing `path` (`(0, 0)` when the OS
/// backend can't report it).
fn volume_capacity(path: &str) -> (u64, u64) {
    #[cfg(target_os = "macos")]
    {
        macos::fs::capacity(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        (0, 0)
    }
}

/// Mounted volumes / partitions — a file browser's "roots". The root filesystem
/// plus every mount under `/Volumes`, deduped by canonical path, each carrying its
/// capacity. Mount discovery is portable `std::fs`; only free-space is OS-specific.
pub fn volumes() -> Vec<crate::traits::Volume> {
    use crate::traits::Volume;
    let mut out: Vec<Volume> = Vec::new();
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();

    let mut push = |name: String, path: String| {
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| std::path::PathBuf::from(&path));
        if !seen.insert(canon) {
            return;
        }
        let (total, free) = volume_capacity(&path);
        out.push(Volume { name, path, total, free });
    };

    push("Computer".to_string(), "/".to_string());
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        let mut vols: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).collect();
        vols.sort();
        for p in vols {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            push(name, p.to_string_lossy().into_owned());
        }
    }
    out
}

// --- Stubs for not-yet-implemented backends so the workspace type-checks on any
// host (Phase 4 fills these in behind the same signatures). ---

#[cfg(not(target_os = "macos"))]
pub fn spawn_pty(_cmd: &PtyCommand) -> io::Result<Box<dyn Pty>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "platform PTY backend not yet implemented for this OS",
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn text_shaper() -> Box<dyn TextShaper> {
    panic!("platform text shaper not yet implemented for this OS")
}
