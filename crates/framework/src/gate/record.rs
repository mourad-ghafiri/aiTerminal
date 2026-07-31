//! The on-disk trace of a live gate — how `@gate status` and `@gate stop` work from
//! another pane.
//!
//! There is no IPC between CLI invocations in this codebase, so cross-process control
//! follows the pattern the background-job runner already uses: the running process
//! polls its own record, and another process asks it to stop by **writing** to that
//! record.
//!
//! Stopping by file rather than by signal is not a stylistic choice. A gate puts the
//! terminal into raw mode and restores it from a `Drop` guard; `SIGTERM`'s default
//! action terminates the process without unwinding, so signalling a gate would leave
//! the user's pane with no echo and no cursor. The record poll costs one small read
//! every half second and is always safe.

use std::path::PathBuf;

use corelib::wire::Toml;

use crate::config::Config;

/// A record's lifecycle.
pub const RUNNING: &str = "running";
pub const STOPPING: &str = "stopping";

/// One live (or recently live) gate.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub id: String,
    pub channel: String,
    pub status: String,
    pub pid: u32,
    pub started: u64,
    /// The paired chat, once there is one.
    pub peer: String,
}

impl Info {
    /// A gate whose process is gone left a stale file behind (a crash, a kill -9).
    pub fn alive(&self) -> bool {
        self.status != STOPPING && platform::os::pid_alive(self.pid)
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The file for `id`. The id is charset-checked so it can never escape the directory.
fn path_for(id: &str) -> Option<PathBuf> {
    let ok = !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then(|| Config::gates_dir().join(format!("{id}.toml")))
}

fn write(path: &PathBuf, info: &Info) {
    let doc = Toml::Table(vec![
        ("channel".into(), Toml::Str(info.channel.clone())),
        ("status".into(), Toml::Str(info.status.clone())),
        ("pid".into(), Toml::Int(info.pid as i64)),
        ("started".into(), Toml::Int(info.started as i64)),
        ("peer".into(), Toml::Str(info.peer.clone())),
    ]);
    let _ = std::fs::create_dir_all(Config::gates_dir());
    let _ = std::fs::write(path, doc.to_string());
    // The record names the chat authorized to drive this machine — not world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

pub fn read(id: &str) -> Option<Info> {
    let text = std::fs::read_to_string(path_for(id)?).ok()?;
    let doc = Toml::parse(&text).ok()?;
    let s = |k: &str| doc.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let n = |k: &str| doc.get(k).and_then(|v| v.as_int()).unwrap_or(0) as u64;
    Some(Info {
        id: id.to_string(),
        channel: s("channel"),
        status: s("status"),
        pid: n("pid") as u32,
        started: n("started"),
        peer: s("peer"),
    })
}

/// Every record on disk, newest first, with dead ones pruned as we go.
pub fn list() -> Vec<Info> {
    let Ok(entries) = std::fs::read_dir(Config::gates_dir()) else { return Vec::new() };
    let mut out: Vec<Info> = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let Some(id) = p.file_stem().and_then(|s| s.to_str()) else { continue };
        let Some(info) = read(id) else { continue };
        // A record whose process died (crash, `kill -9`) is noise; clear it so
        // `@gate status` only ever shows the truth.
        if !platform::os::pid_alive(info.pid) {
            let _ = std::fs::remove_file(&p);
            continue;
        }
        out.push(info);
    }
    out.sort_by(|a, b| b.started.cmp(&a.started));
    out
}

/// Ask a running gate to shut down. Returns the record it flagged.
pub fn request_stop(id: &str) -> Option<Info> {
    let mut info = read(id)?;
    let path = path_for(id)?;
    info.status = STOPPING.into();
    write(&path, &info);
    Some(info)
}

/// A live gate's record, removed when the gate ends however it ends.
pub struct GateRecord {
    info: Info,
    path: PathBuf,
}

impl GateRecord {
    /// Claim a record for this process. The id sorts by start time and is unique per
    /// process, matching the job runner's scheme.
    pub fn create(channel: &str) -> std::io::Result<GateRecord> {
        let pid = std::process::id();
        let info = Info {
            id: format!("{}-{pid}", now()),
            channel: channel.to_string(),
            status: RUNNING.into(),
            pid,
            started: now(),
            peer: String::new(),
        };
        let path = path_for(&info.id).ok_or_else(|| std::io::Error::other("bad gate id"))?;
        write(&path, &info);
        Ok(GateRecord { info, path })
    }

    pub fn id(&self) -> &str {
        &self.info.id
    }

    /// Record who paired, so `@gate status` in another pane can show it.
    pub fn set_peer(&mut self, peer: &str) {
        self.info.peer = peer.to_string();
        write(&self.path, &self.info);
    }

    /// Has another pane asked us to stop? Polled by the driver.
    pub fn stop_requested(&self) -> bool {
        read(&self.info.id).map(|i| i.status == STOPPING).unwrap_or(true) // a deleted record also means stop
    }
}

impl Drop for GateRecord {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests;
