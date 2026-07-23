//! The status-line evaluation engine — the pure functions that resolve a plugin's
//! `[[var]]`/segment sources into rendered values, plus terminal-context probing.
//! Split out of `plugin/mod.rs` so the manifest/registry data model stays separate
//! from the evaluation logic.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;

pub(super) fn eval_source(src: &VarSource, ctx: &Context, vars: &Vars, deadline: Duration, exec_cache: &mut std::collections::HashMap<String, String>) -> String {
    match src {
        VarSource::Literal(s) => s.clone(),
        VarSource::Env(name) => std::env::var(name).unwrap_or_default(),
        VarSource::From(id) => vars.get(id).to_string(),
        VarSource::File(rel) => {
            let path = if Path::new(rel).is_absolute() { PathBuf::from(rel) } else { ctx.cwd.join(rel) };
            std::fs::read_to_string(path).unwrap_or_default()
        }
        VarSource::Exec(cmd) => {
            if let Some(cached) = exec_cache.get(cmd) {
                return cached.clone();
            }
            let out = run_bounded("/bin/sh", &["-c", cmd], Some(&ctx.cwd), deadline).unwrap_or_default();
            exec_cache.insert(cmd.clone(), out.clone());
            out
        }
    }
}

pub(super) fn apply_transforms(mut v: String, tr: &Transforms) -> String {
    if tr.trim {
        v = v.trim().to_string();
    }
    if let Some(p) = &tr.strip_prefix {
        // strip on the first line (handles e.g. .git/HEAD)
        let line = v.lines().next().unwrap_or("").trim();
        v = line.strip_prefix(p.as_str()).unwrap_or(line).to_string();
    }
    if tr.basename {
        v = Path::new(&v).file_name().and_then(|s| s.to_str()).unwrap_or(&v).to_string();
    }
    if let Some(idx) = tr.field {
        v = v.split_whitespace().nth(idx).unwrap_or("").to_string();
    }
    if let Some(map) = &tr.map_nonempty {
        v = if v.trim().is_empty() { String::new() } else { map.clone() };
    }
    if !v.is_empty() {
        if let Some(p) = &tr.prefix {
            v = format!("{p}{v}");
        }
        if let Some(s) = &tr.suffix {
            v.push_str(s);
        }
    }
    if v.is_empty() {
        if let Some(d) = &tr.default {
            v = d.clone();
        }
    }
    v
}

/// Built-in context variables (generic; not tool-specific).
pub(super) fn builtin_context_vars(ctx: &Context, deadline: Duration) -> Vars {
    let mut v = Vars::default();
    v.set("cwd.full", ctx.cwd.display().to_string());
    v.set("cwd.short", shorten_path(&ctx.cwd, &ctx.home));
    v.set(
        "dir.name",
        ctx.cwd.file_name().and_then(|s| s.to_str()).unwrap_or("/").to_string(),
    );
    v.set("home", ctx.home.display().to_string());
    v.set("user", std::env::var("USER").unwrap_or_else(|_| "user".into()));
    v.set("os", os_name());

    // Prefer the host the shell reported via OSC 7 (e.g. the REMOTE host during SSH); else
    // the local hostname, resolved once per session (it never changes) — not every tick.
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let local_host = || {
        HOST.get_or_init(|| {
            run_bounded("hostname", &["-s"], None, deadline)
                .or_else(|| run_bounded("hostname", &[], None, deadline))
                .map(|h| h.trim().split('.').next().unwrap_or("").to_string())
                .unwrap_or_default()
        })
        .clone()
    };
    let host = match &ctx.host {
        Some(h) if !h.is_empty() => h.split('.').next().unwrap_or(h).to_string(),
        _ => local_host(),
    };
    if !host.is_empty() {
        v.set("host", host.as_str());
        v.set("host.short", host.as_str());
    }
    if let Some(d) = run_bounded("date", &["+%H:%M %H:%M:%S %Y-%m-%d"], None, deadline) {
        let mut it = d.split_whitespace();
        if let Some(hm) = it.next() {
            v.set("time.hm", hm);
        }
        if let Some(hms) = it.next() {
            v.set("time.hms", hms);
        }
        if let Some(ymd) = it.next() {
            v.set("date.ymd", ymd);
        }
    }
    v
}

fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        "Unknown"
    }
}

/// Run a command in `cwd`, capturing stdout, killed if it exceeds `deadline`.
fn run_bounded(cmd: &str, args: &[&str], cwd: Option<&Path>, deadline: Duration) -> Option<String> {
    let mut c = Command::new(cmd);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let mut child = c.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    match rx.recv_timeout(deadline) {
        Ok(s) => {
            let _ = child.wait();
            Some(s)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

fn shorten_path(cwd: &Path, home: &Path) -> String {
    let s = if let Ok(rest) = cwd.strip_prefix(home) {
        if rest.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", rest.display())
        }
    } else {
        cwd.display().to_string()
    };
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() > 3 {
        format!("\u{2026}/{}", parts[parts.len() - 2..].join("/"))
    } else {
        s
    }
}

pub(super) fn utf8_len(lead: u8) -> usize {
    if lead < 0x80 {
        1
    } else if lead >> 5 == 0b110 {
        2
    } else if lead >> 4 == 0b1110 {
        3
    } else if lead >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// Build the plugin-evaluation [`Context`] for `cwd` (the data sources read here).
pub fn probe_context(cwd: &Path, columns: u16) -> Context {
    Context { cwd: cwd.to_path_buf(), home: home_dir(), columns, host: None }
}

/// Like [`probe_context`], but with an explicit host to report (e.g. the remote host
/// from OSC 7 during SSH). `None` keeps the local hostname.
pub fn probe_context_host(cwd: &Path, columns: u16, host: Option<String>) -> Context {
    Context { cwd: cwd.to_path_buf(), home: home_dir(), columns, host }
}

fn home_dir() -> PathBuf {
    platform::os::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// The live working directory of process `pid`, via `lsof` (no fragile repr(C)
/// FFI). Deadline-bounded so it can never stall the status worker.
pub fn process_cwd(pid: i32, deadline: Duration) -> Option<PathBuf> {
    let mut child = Command::new("lsof")
        .args(["-a", "-d", "cwd", "-Fn", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    let out = match rx.recv_timeout(deadline) {
        Ok(s) => {
            let _ = child.wait();
            s
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    out.lines().find_map(|l| l.strip_prefix('n').map(PathBuf::from))
}
