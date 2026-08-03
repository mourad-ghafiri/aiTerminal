//! What may be read, and what may be changed.
//!
//! Rules are regexes over the **absolute** path, so `\.env$` and `/clients/` both read the
//! way you would expect. Every judgement tests the path as given *and* its canonical form,
//! because a symlink is a second name for a file and a rule is about the file.

use std::path::{Path, PathBuf};

use super::regex::Regex;
use super::rules::PathRule;
use super::Decision;

/// The path tiers, compiled.
#[derive(Clone, Default)]
pub(crate) struct Paths {
    deny: Vec<Regex>,
    read_only: Vec<Regex>,
    allow: Vec<Regex>,
}

impl Paths {
    pub(crate) fn add(&mut self, rule: PathRule, re: Regex) {
        match rule {
            PathRule::Deny => self.deny.push(re),
            PathRule::ReadOnly => self.read_only.push(re),
            PathRule::Allow => self.allow.push(re),
        }
    }

    /// The patterns of a tier, for the briefing.
    pub(crate) fn patterns(&self, rule: PathRule) -> Vec<&str> {
        let tier = match rule {
            PathRule::Deny => &self.deny,
            PathRule::ReadOnly => &self.read_only,
            PathRule::Allow => &self.allow,
        };
        tier.iter().map(|r| r.as_str()).collect()
    }

    /// May this path be read, listed or stat-ed?
    pub(crate) fn judge_read(&self, p: &Path) -> Decision {
        match self.refuses_read(p) {
            Some(why) => Decision::Deny { reason: format!("{} {why}", show(p)) },
            None => Decision::Allow,
        }
    }

    /// Why a read is refused, with the path left out — so a caller that has already named
    /// the path (a command quoting the token it wrote) does not say it twice.
    pub(crate) fn refuses_read(&self, p: &Path) -> Option<String> {
        let names = names_of(p);
        if let Some(hit) = first(&self.deny, &names) {
            return Some(format!("matches an off-limits path  /{hit}/"));
        }
        match self.allow.is_empty() || first(&self.allow, &names).is_some() {
            true => None,
            false => Some("is outside the allowed paths".to_string()),
        }
    }

    /// May this path be created, modified, moved or deleted? Everything a read is refused
    /// for, plus the read-only tier.
    pub(crate) fn judge_write(&self, p: &Path) -> Decision {
        if let d @ Decision::Deny { .. } = self.judge_read(p) {
            return d;
        }
        let names = names_of(p);
        match first(&self.read_only, &names) {
            Some(hit) => Decision::Deny { reason: format!("{} is read-only here  /{hit}/", show(p)) },
            None => Decision::Allow,
        }
    }
}

/// Both names a rule is tested against: the path as given, and — when it differs — the
/// canonical one. A rule is about a file, and a symlink is a second name for it.
fn names_of(p: &Path) -> Vec<String> {
    let given = p.to_string_lossy().into_owned();
    match p.canonicalize() {
        Ok(real) if real != *p => {
            let real = real.to_string_lossy().into_owned();
            match real == given {
                true => vec![given],
                false => vec![given, real],
            }
        }
        _ => vec![given],
    }
}

fn first<'a>(tier: &'a [Regex], names: &[String]) -> Option<&'a str> {
    names.iter().find_map(|n| tier.iter().find(|r| r.is_match(n)).map(|r| r.as_str()))
}

fn show(p: &Path) -> String {
    format!("{:?}", p.display().to_string())
}

/// The rules that are always in force, whatever the config says and whichever plugins are
/// disabled: the keys and credentials that would compromise this machine, this terminal's
/// own config (it holds the API key), and the gate records that say who may drive it
/// remotely.
///
/// Everything else the product refuses ships as editable plugin data, because a policy you
/// can read and change is the whole idea. These are the exception, and the reason is
/// narrow: a guard you can switch off by disabling a plugin is not a guard.
pub(crate) fn floor(home: Option<&Path>) -> Vec<(String, PathRule)> {
    let mut out: Vec<(String, PathRule)> = Vec::new();
    if let Some(home) = home {
        let h = escape(&home.to_string_lossy());
        for dir in [r"\.ssh", r"\.aws", r"\.gnupg", r"\.config/gh", r"\.aiTerminal/gates"] {
            out.push((format!("^{h}/{dir}(/|$)"), PathRule::Deny));
        }
        out.push((format!(r"^{h}/\.aiTerminal/config\.toml$"), PathRule::Deny));
    }
    out.push((r"(^|/)(id_rsa|id_dsa|id_ecdsa|id_ed25519|\.netrc)$".to_string(), PathRule::Deny));
    out.push((r"(?i)\.(pem|key|p12|pfx)$".to_string(), PathRule::Deny));
    out
}

/// A literal string as a regex that matches only itself — for splicing a real home
/// directory (which can contain `.`, `+`, spaces) into a floor pattern.
fn escape(literal: &str) -> String {
    let mut out = String::with_capacity(literal.len());
    for c in literal.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The directory a relative path resolves against, and the home `~` expands to. Captured
/// once when the guard is built, so every judgement afterwards is a pure function of its
/// input — and a test can point both somewhere harmless.
#[derive(Clone, Debug, Default)]
pub struct Base {
    pub home: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
}

impl Base {
    /// This machine, as it is right now.
    pub fn here() -> Base {
        Base { home: platform::os::home_dir(), cwd: std::env::current_dir().ok() }
    }
}
