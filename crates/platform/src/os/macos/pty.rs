//! macOS PTY backend: `posix_openpt` + `fork` + `execve` (all libSystem, no
//! third-party crate). The child runs the requested program (or the user's login
//! shell) attached to the slave side as its controlling terminal.
//!
//! Launched from the desktop (a GUI launch via LaunchServices) there is no inherited
//! interactive shell environment,
//! so for a login shell we (a) set argv[0] to `-<name>` so the profile is sourced
//! (fixing `PATH`), (b) `chdir($HOME)` instead of the inherited `/`, and (c) build
//! an envp that always carries `TERM`/`COLORTERM`/`TERM_PROGRAM`. The program is
//! resolved to an absolute path (so `execve` works without a `PATH` search).

use std::ffi::{CStr, CString};
use std::io;
use std::os::raw::{c_char, c_int, c_long, c_ulong, c_void};

use crate::traits::{Pty, PtyCommand};

// --- libSystem FFI (declared by us; linked from libSystem automatically) ---
extern "C" {
    fn posix_openpt(flags: c_int) -> c_int;
    fn grantpt(fd: c_int) -> c_int;
    fn unlockpt(fd: c_int) -> c_int;
    fn ptsname(fd: c_int) -> *mut c_char;
    fn fork() -> c_int;
    fn setsid() -> c_int;
    fn open(path: *const c_char, flags: c_int) -> c_int;
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn dup2(old: c_int, new: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn chdir(path: *const c_char) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, n: usize) -> isize;
    fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
    fn execve(path: *const c_char, argv: *const *const c_char, envp: *const *const c_char) -> c_int;
    fn _exit(code: c_int) -> !;
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn getuid() -> u32;
    fn getpwuid(uid: u32) -> *const Passwd;
}

/// macOS `struct passwd` (LP64 layout) — we read `pw_shell` + `pw_dir` only.
#[repr(C)]
struct Passwd {
    pw_name: *const c_char,
    pw_passwd: *const c_char,
    pw_uid: u32,
    pw_gid: u32,
    pw_change: c_long,
    pw_class: *const c_char,
    pw_gecos: *const c_char,
    pw_dir: *const c_char,
    pw_shell: *const c_char,
    pw_expire: c_long,
}

// macOS constants (BSD).
const O_RDWR: c_int = 0x0002;
const O_NOCTTY: c_int = 0x20000;
const TIOCSWINSZ: c_ulong = 0x8008_7467; // _IOW('t', 103, struct winsize)
const TIOCSCTTY: c_ulong = 0x2000_7461; // _IO('t', 97)
const SIGHUP: c_int = 1;
const EIO: i32 = 5;

#[repr(C)]
#[derive(Clone, Copy)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

/// `pw_shell` + `pw_dir` from the password database for the current uid.
fn passwd_shell_and_home() -> (Option<String>, Option<String>) {
    // SAFETY: `getpwuid` returns a pointer into a static buffer; we copy out
    // immediately and never retain it.
    unsafe {
        let pw = getpwuid(getuid());
        if pw.is_null() {
            return (None, None);
        }
        let s = |p: *const c_char| {
            if p.is_null() {
                None
            } else {
                Some(CStr::from_ptr(p).to_string_lossy().into_owned()).filter(|x| !x.is_empty())
            }
        };
        (s((*pw).pw_shell), s((*pw).pw_dir))
    }
}

/// Is `p` an existing, executable regular file?
fn is_executable(p: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0)).unwrap_or(false)
}

/// Resolve a program name to an absolute, executable path (a tiny `which`): an
/// explicit path is used as-is; a bare name is searched on `$PATH`.
fn which(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    if name.contains('/') {
        return is_executable(name).then(|| name.to_string());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        let cand = format!("{}/{}", dir.trim_end_matches('/'), name);
        if is_executable(&cand) {
            return Some(cand);
        }
    }
    None
}

/// Resolve the shell to spawn: configured → `$SHELL` → password-db shell →
/// `/bin/zsh` → `/bin/bash` → `/bin/sh`. Always returns an executable path.
fn resolve_shell(configured: &str) -> String {
    let (pw_shell, _) = passwd_shell_and_home();
    let candidates = [
        configured.to_string(),
        std::env::var("SHELL").unwrap_or_default(),
        pw_shell.unwrap_or_default(),
    ];
    for c in candidates.iter().filter(|c| !c.is_empty()) {
        if let Some(abs) = which(c) {
            return abs;
        }
    }
    for c in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
        if is_executable(c) {
            return c.to_string();
        }
    }
    "/bin/sh".to_string()
}

/// `argv[0]` for a login shell: the basename prefixed with `-` (so the shell
/// sources the user's login profile and sets up `PATH`).
fn login_argv0(program: &str) -> String {
    let base = program.rsplit('/').next().unwrap_or(program);
    format!("-{base}")
}

/// `$HOME`, falling back to the password-db home dir (GUI/desktop launches set `HOME`).
fn home_dir() -> Option<String> {
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    passwd_shell_and_home().1
}

/// The child's environment: the parent's, minus the terminal vars we own, plus
/// `TERM`/`COLORTERM`/`TERM_PROGRAM[_VERSION]` (correct even with no inherited
/// shell env), plus the host's `overrides` (shell integration: `ZDOTDIR`,
/// `LS_COLORS`, …). Precedence: inherited env < backend `TERM` group < `overrides`
/// — a key in `overrides` replaces any inherited/backend value. Built in the parent
/// (pre-fork); the child only `execve`s it.
fn build_envp(overrides: &[(String, String)]) -> Vec<CString> {
    use std::os::unix::ffi::OsStrExt;
    const OWNED: [&str; 4] = ["TERM", "COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"];
    let overridden = |k: &[u8]| overrides.iter().any(|(ok, _)| ok.as_bytes() == k);
    let mut out: Vec<CString> = Vec::new();
    for (k, v) in std::env::vars_os() {
        let kb = k.as_bytes();
        if OWNED.iter().any(|o| o.as_bytes() == kb) || overridden(kb) {
            continue;
        }
        let mut buf = Vec::with_capacity(kb.len() + 1 + v.len());
        buf.extend_from_slice(kb);
        buf.push(b'=');
        buf.extend_from_slice(v.as_bytes());
        if let Ok(c) = CString::new(buf) {
            out.push(c);
        }
    }
    for (k, v) in [
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("TERM_PROGRAM", corelib::brand::NAME),
        ("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION")),
    ] {
        if !overridden(k.as_bytes()) {
            if let Ok(c) = CString::new(format!("{k}={v}")) {
                out.push(c);
            }
        }
    }
    for (k, v) in overrides {
        if let Ok(c) = CString::new(format!("{k}={v}")) {
            out.push(c);
        }
    }
    out
}

/// A live PTY master connected to a child process.
pub struct MacPty {
    master: c_int,
    pid: c_int,
}

// Only Copy integer handles (an fd + pid); safe to move to the reader thread,
// and safe to share (&) since reads happen on one thread and writes on another —
// concurrent read+write on a PTY master fd is sound at the OS level.
unsafe impl Send for MacPty {}
unsafe impl Sync for MacPty {}

pub fn spawn(cmd: &PtyCommand) -> io::Result<MacPty> {
    // Resolve program + argv + env + cwd BEFORE fork (the child path allocates
    // nothing). A login shell, or any empty/bare program, resolves through the
    // shell cascade; an explicit command path is resolved with a `PATH` search so
    // `execve` (which does none) still finds it.
    let nul = |_| io::Error::new(io::ErrorKind::InvalidInput, "value has NUL");
    let program = if cmd.login || cmd.program.is_empty() {
        resolve_shell(&cmd.program)
    } else {
        which(&cmd.program).unwrap_or_else(|| cmd.program.clone())
    };
    let prog_c = CString::new(program.as_bytes()).map_err(nul)?;

    // argv[0]: "-<name>" for a login shell, else the program path.
    let argv0 = if cmd.login { login_argv0(&program) } else { program.clone() };
    let mut arg_cstrings: Vec<CString> = Vec::with_capacity(cmd.args.len() + 1);
    arg_cstrings.push(CString::new(argv0.as_bytes()).map_err(nul)?);
    for a in &cmd.args {
        arg_cstrings.push(CString::new(a.as_bytes()).map_err(nul)?);
    }
    let mut argv: Vec<*const c_char> = arg_cstrings.iter().map(|c| c.as_ptr()).collect();
    argv.push(std::ptr::null());

    // envp (parent env + our TERM/COLORTERM/TERM_PROGRAM).
    let env_cstrings = build_envp(&cmd.env);
    let mut envp: Vec<*const c_char> = env_cstrings.iter().map(|c| c.as_ptr()).collect();
    envp.push(std::ptr::null());

    // Where to start the shell: an explicit cwd (workspace restore) wins; otherwise a
    // login shell starts at $HOME (a GUI launch would otherwise inherit "/").
    let chdir_c = match cmd.cwd.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => CString::new(p).ok(),
        None if cmd.login => home_dir().and_then(|h| CString::new(h).ok()),
        None => None,
    };

    let ws = Winsize { ws_row: cmd.rows.max(1), ws_col: cmd.cols.max(1), ws_xpixel: 0, ws_ypixel: 0 };

    // SAFETY: standard libSystem PTY/fork/exec dance; the child branch calls only
    // async-signal-safe functions on pre-built pointers and never returns to Rust.
    unsafe {
        let master = posix_openpt(O_RDWR | O_NOCTTY);
        if master < 0 {
            return Err(io::Error::last_os_error());
        }
        if grantpt(master) != 0 || unlockpt(master) != 0 {
            let e = io::Error::last_os_error();
            close(master);
            return Err(e);
        }
        let sname = ptsname(master);
        if sname.is_null() {
            close(master);
            return Err(io::Error::new(io::ErrorKind::Other, "ptsname failed"));
        }
        // Copy the slave path before fork (ptsname returns a static buffer).
        let slave_path = {
            let mut len = 0usize;
            while *sname.add(len) != 0 {
                len += 1;
            }
            let bytes = std::slice::from_raw_parts(sname as *const u8, len + 1);
            bytes.to_vec()
        };

        let pid = fork();
        if pid < 0 {
            let e = io::Error::last_os_error();
            close(master);
            return Err(e);
        }
        if pid == 0 {
            // --- child: async-signal-safe only, no allocation, never returns ---
            setsid();
            let slave = open(slave_path.as_ptr() as *const c_char, O_RDWR);
            if slave < 0 {
                _exit(127);
            }
            ioctl(slave, TIOCSCTTY, 0 as c_int);
            ioctl(slave, TIOCSWINSZ, &ws as *const Winsize);
            dup2(slave, 0);
            dup2(slave, 1);
            dup2(slave, 2);
            if slave > 2 {
                close(slave);
            }
            close(master);
            if let Some(dir) = &chdir_c {
                chdir(dir.as_ptr()); // best-effort; ignore failure
            }
            execve(prog_c.as_ptr(), argv.as_ptr(), envp.as_ptr());
            _exit(127); // exec failed
        }

        Ok(MacPty { master, pid })
    }
}

impl Pty for MacPty {
    fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let ws = Winsize { ws_row: rows.max(1), ws_col: cols.max(1), ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: valid master fd + winsize pointer.
        let rc = unsafe { ioctl(self.master, TIOCSWINSZ, &ws as *const Winsize) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn write(&self, bytes: &[u8]) -> io::Result<usize> {
        // SAFETY: valid fd; buffer described by ptr+len.
        let n = unsafe { write(self.master, bytes.as_ptr() as *const c_void, bytes.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }

    fn pid(&self) -> Option<i32> {
        Some(self.pid)
    }

    fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: valid fd; buffer described by ptr+len.
        let n = unsafe { read(self.master, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            // After the child exits and closes the slave, macOS returns EIO on the
            // master rather than 0; treat that as EOF.
            if err.raw_os_error() == Some(EIO) {
                return Ok(0);
            }
            return Err(err);
        }
        Ok(n as usize)
    }
}

impl Drop for MacPty {
    fn drop(&mut self) {
        // SAFETY: best-effort teardown of our own fd + child.
        unsafe {
            if self.pid > 0 {
                kill(self.pid, SIGHUP);
            }
            close(self.master);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_argv0_prefixes_basename() {
        assert_eq!(login_argv0("/bin/zsh"), "-zsh");
        assert_eq!(login_argv0("zsh"), "-zsh");
        assert_eq!(login_argv0("/usr/local/bin/fish"), "-fish");
    }

    #[test]
    fn which_resolves_absolute_and_path() {
        assert_eq!(which("/bin/sh").as_deref(), Some("/bin/sh"));
        assert!(which("sh").is_some(), "sh should be on PATH");
        assert!(which("definitely-not-a-real-binary-xyz123").is_none());
        assert!(which("").is_none());
    }

    #[test]
    fn resolve_shell_always_returns_an_executable() {
        let s = resolve_shell("");
        assert!(s.starts_with('/') && is_executable(&s), "default shell {s:?} not executable");
        // an explicit, valid shell wins
        assert_eq!(resolve_shell("/bin/sh"), "/bin/sh");
        // a bogus configured shell still yields a working fallback
        assert!(is_executable(&resolve_shell("/no/such/shell-xyz")));
    }

    #[test]
    fn echo_through_pty_round_trips() {
        let cmd = PtyCommand {
            program: "/bin/echo".into(),
            args: vec!["pty-ok".into()],
            cols: 80,
            rows: 24,
            login: false,
            ..Default::default()
        };
        let pty = spawn(&cmd).expect("spawn echo");
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            match pty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) => panic!("read error: {e}"),
            }
            if out.len() > 4096 {
                break;
            }
        }
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("pty-ok"), "pty output was {s:?}");
    }

    #[test]
    fn shell_dash_c_runs_and_writes_input() {
        let cmd = PtyCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "printf 'A%sB' hello".into()],
            cols: 80,
            rows: 24,
            login: false,
            ..Default::default()
        };
        let pty = spawn(&cmd).expect("spawn sh");
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        while let Ok(n) = pty.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            if out.len() > 4096 {
                break;
            }
        }
        assert!(String::from_utf8_lossy(&out).contains("AhelloB"));
    }

    #[test]
    fn term_is_exported_to_the_child() {
        // Even with no inherited shell env (the GUI-launch case), the child must see
        // our TERM — proves build_envp + execve carry it through.
        let cmd = PtyCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "printf 'T=%s' \"$TERM\"".into()],
            cols: 80,
            rows: 24,
            login: false,
            ..Default::default()
        };
        let pty = spawn(&cmd).expect("spawn sh");
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        while let Ok(n) = pty.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            if out.len() > 4096 {
                break;
            }
        }
        assert!(
            String::from_utf8_lossy(&out).contains("T=xterm-256color"),
            "child TERM was {:?}",
            String::from_utf8_lossy(&out)
        );
    }
}
