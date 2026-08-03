//! The capability backends — the implementation functions behind the pure native
//! object families (`os`/`fs`/`sec`/`clock`/`store`/`sys`/`net`/`web`), plus their
//! SSRF/filesystem guards and helpers. Split out of `caps/mod.rs` so the registry +
//! permission map stay separate from the per-family logic, and split again by family
//! so each file is about one thing. A child module: it reads the parent's
//! `CapCtx`/`arg`/`obj`.

pub(crate) mod files;
pub(crate) mod misc;
pub(crate) mod nav;
pub(crate) mod paths;
pub(crate) mod ssrf;
pub(crate) mod sysrun;
pub(crate) mod web;

// ── what `caps` calls ───────────────────────────────────────────────────────
//
// One family function per native object, plus the helpers the sibling modules and
// the test suite reach for. Which file each lives in is this module's business.

/// The family entry points the registry in `caps::mod` dispatches to.
pub(super) use crate::caps::backends::{
    files::fs, misc::{clock, guard, os, store}, ssrf::net, sysrun::sys, web::web,
};

/// Path handling and guards, shared with `caps::files` and `caps::git`.
pub(super) use crate::caps::backends::nav::{expand_tilde, first_heading};
pub(super) use crate::caps::backends::paths::fs_path;
pub(crate) use crate::caps::backends::paths::fs_path_rel;

/// The SSRF rules `caps::git` and `caps::http` apply to their own fetches.
pub(super) use crate::caps::backends::ssrf::{ssrf_pin, ssrf_resolve, url_host_port};

/// Pure helpers with their own unit tests, and nothing else naming them.
#[cfg(test)]
pub(super) use crate::caps::backends::{
    files::glob_match,
    ssrf::is_blocked_ip,
    web::{html_to_markdown, parse_ddg_results, percent_decode, percent_encode},
};
