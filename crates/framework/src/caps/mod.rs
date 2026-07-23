//! The native object standard library — the tool families an AI agent calls
//! (`fs` / `sys` / `web` / `memory` / `data` / `todo` / …). Each returns a
//! **tainted** JSON value. `sys.run` always re-enters the command guard; `net`/
//! `web` apply an SSRF allow-rule + the `[ai] network` switch; `store`/`data`
//! are confined to the caller's sandboxed data dir.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use corelib::wire::Json;

mod backends;
mod clip;
mod codec;
mod data;
mod diag;
mod files;
mod git;
mod http;
mod memory;
mod queue;
mod task;
mod time;
mod todo;
use backends::{clock, fs, os, sec, store, sys, web};
pub mod host;
mod net;
mod object;

pub use host::{Host, NullHost};
pub use object::{MethodSpec, NativeObject, ObjectRegistry};

use std::sync::OnceLock;

/// Execution context for a capability call.
#[derive(Clone)]
pub struct CapCtx {
    pub policy: Arc<crate::security::Policy>,
    /// The caller's sandboxed data dir, or `None` (then `store`/`data` are
    /// unavailable).
    pub app_data: Option<PathBuf>,
    pub remote_enabled: bool,
    /// The calling origin (e.g. `terminal://ai/`), for attribution.
    pub origin: String,
    /// The sandbox root — the directory the run was invoked from. `fs` WRITES are
    /// confined to it (outside / `None` = denied); read-only `fs` browsing is
    /// unaffected.
    pub sandbox: Option<PathBuf>,
}

/// The standard-library registry (pure families). Built once per process; each
/// object is a stateless value type, so the registry is cheap + `Send + Sync`.
pub fn standard_registry() -> ObjectRegistry {
    ObjectRegistry::new(vec![
        Box::new(Os),
        Box::new(Fs),
        Box::new(files::FilesObj),
        Box::new(diag::DiagObj),
        Box::new(Sec),
        Box::new(Clock),
        Box::new(Store),
        Box::new(Sys),
        Box::new(Net),
        Box::new(WebObj),
        Box::new(memory::MemoryObj),
        Box::new(codec::CodecObj),
        Box::new(time::TimeObj),
        Box::new(data::DataObj),
        Box::new(queue::QueueObj),
        Box::new(todo::TodoObj),
        Box::new(task::TaskObj),
        Box::new(http::HttpObj),
        Box::new(clip::ClipObj),
    ])
}

fn registry() -> &'static ObjectRegistry {
    static R: OnceLock<ObjectRegistry> = OnceLock::new();
    R.get_or_init(standard_registry)
}

/// Run a capability method (the agent tool path + tests). All families are pure
/// over [`CapCtx`]; a [`NullHost`] satisfies the seam.
pub fn run(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let mut null = NullHost;
    registry().run(method, args, ctx, &mut null)
}

/// A human description of a method (shown in the agent tool catalog).
pub fn describe(method: &str) -> &'static str {
    registry().describe(method)
}

pub(super) fn arg<'a>(args: &'a [(String, String)], i: usize, name: &str) -> Option<&'a str> {
    args.iter()
        .find(|(k, _)| k == name)
        .or_else(|| args.iter().find(|(k, _)| k == &i.to_string()))
        .map(|(_, v)| v.as_str())
}

// ===== the standard objects (each delegates to its family fn below) =========

struct Os;
impl NativeObject for Os {
    fn family(&self) -> &'static str { "os" }
    fn methods(&self) -> &'static [MethodSpec] {
        &[MethodSpec { method: "os.open", describe: "Open a link in the OS browser" }]
    }
    fn invoke(&self, method: &str, args: &[(String, String)], _ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        os(method, args)
    }
}

struct Fs;
impl NativeObject for Fs {
    fn family(&self) -> &'static str { "fs" }
    fn methods(&self) -> &'static [MethodSpec] {
        &[
            MethodSpec { method: "fs.home", describe: "Read the home directory path" },
            MethodSpec { method: "fs.roots", describe: "List mounted volumes / partitions" },
            MethodSpec { method: "fs.list", describe: "List a directory's entries" },
            MethodSpec { method: "fs.stat", describe: "Read a path's metadata" },
            MethodSpec { method: "fs.measure", describe: "Measure a folder's recursive size + file/dir counts" },
            MethodSpec { method: "fs.read", describe: "Read a text file's contents" },
            MethodSpec { method: "fs.open", describe: "Open a file with its default app" },
            MethodSpec { method: "fs.write", describe: "Write a text file (sandbox-confined: the invocation directory)" },
            MethodSpec { method: "fs.mkdir", describe: "Create a directory (sandbox-confined: the invocation directory)" },
            MethodSpec { method: "fs.edit", describe: "Replace text in a file (sandbox-confined: the invocation directory)" },
            MethodSpec { method: "fs.delete", describe: "Delete a file (sandbox-confined: the invocation directory)" },
            MethodSpec { method: "fs.append", describe: "Append to a file (sandbox-confined: the invocation directory)" },
            MethodSpec { method: "fs.copy", describe: "Copy a file (sandbox-confined: the invocation directory)" },
            MethodSpec { method: "fs.move", describe: "Move/rename a file (sandbox-confined: the invocation directory)" },
            MethodSpec { method: "fs.glob", describe: "Find files by glob pattern" },
            MethodSpec { method: "fs.search", describe: "Search the workspace for a literal or regex (grep); returns path:line matches" },
        ]
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        fs(method, args, ctx)
    }
}

struct Sec;
impl NativeObject for Sec {
    fn family(&self) -> &'static str { "sec" }
    fn methods(&self) -> &'static [MethodSpec] {
        &[
            MethodSpec { method: "sec.check_command", describe: "Check a command against the guard" },
            MethodSpec { method: "sec.redact", describe: "Redact secrets from text" },
        ]
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        sec(method, args, ctx)
    }
}

struct Clock;
impl NativeObject for Clock {
    fn family(&self) -> &'static str { "clock" }
    fn methods(&self) -> &'static [MethodSpec] {
        &[MethodSpec { method: "clock.now", describe: "Read the current time" }]
    }
    fn invoke(&self, method: &str, _args: &[(String, String)], _ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        clock(method)
    }
}

struct Store;
impl NativeObject for Store {
    fn family(&self) -> &'static str { "store" }
    fn methods(&self) -> &'static [MethodSpec] {
        &[
            MethodSpec { method: "store.get", describe: "Read this app's storage" },
            MethodSpec { method: "store.set", describe: "Write to this app's storage" },
            MethodSpec { method: "store.delete", describe: "Write to this app's storage" },
            MethodSpec { method: "store.list", describe: "List this app's storage keys" },
        ]
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        store(method, args, ctx)
    }
}

struct Sys;
impl NativeObject for Sys {
    fn family(&self) -> &'static str { "sys" }
    fn methods(&self) -> &'static [MethodSpec] {
        &[MethodSpec { method: "sys.run", describe: "Run a shell command (re-checked by the command guard)" }]
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        sys(method, args, ctx)
    }
}

struct Net;
impl NativeObject for Net {
    fn family(&self) -> &'static str { "net" }
    fn methods(&self) -> &'static [MethodSpec] {
        &[MethodSpec { method: "net.get", describe: "Fetch a URL over the network" }]
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        backends::net(method, args, ctx)
    }
}

/// The `web` family — fetch a page as **markdown** for an AI tool / the harness. A
/// thin reader over `nav_fetch` (`md://`/`mds://`) and `net::https_get` (`https://`,
/// reduced to markdown), so an agent reads docs without raw HTML. Same SSRF +
/// `remote_enabled` guards as `net`.
struct WebObj;
impl NativeObject for WebObj {
    fn family(&self) -> &'static str { "web" }
    fn methods(&self) -> &'static [MethodSpec] {
        &[MethodSpec { method: "web.read", describe: "Fetch a web page as markdown" }]
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        web(method, args, ctx)
    }
}

// ----- helpers ---------------------------------------------------------------

pub(super) fn obj(pairs: &[(&str, Json)]) -> Json {
    Json::Obj(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

#[cfg(test)]
mod tests;
