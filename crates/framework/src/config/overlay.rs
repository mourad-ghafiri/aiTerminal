//! The project overlay: what a folder's own `.aiTerminal/` and root instructions file
//! contribute to a workspace session — and the rules for letting them.
//!
//! Two principles, both security-load-bearing:
//!
//! 1. **Nothing loads before trust.** A repo's `.aiTerminal/` can declare MCP servers
//!    (code that runs as you) and agents/skills (words that steer a model with your
//!    tools). [`offering`](Workspace::offering) COUNTS what is there without loading
//!    any of it — that is what the trust prompt shows — and only a [`Workspace`]
//!    opened as trusted resolves the project directories at all.
//! 2. **A project may only tighten the guard.** Its `[guard]` rules pass through
//!    [`tighten`]: deny/confirm/read-only/secret rules land, allow/auto rules are
//!    dropped and named. A cloned repo must never be able to loosen the machine's
//!    own policy — the trust prompt covers running the project's code, not rewriting
//!    what the guard refuses.

use std::path::{Path, PathBuf};

use crate::guard::{CommandRule, PathRule, RuleSet};

/// The dot-directory a project overlays from, and the instruction filenames read at
/// its root — ours first, then the open convention other harnesses share.
const DOTDIR: &str = ".aiTerminal";
const OURS: &str = "aiTerminal.md";
const CONVENTION: &str = "AGENTS.md";

/// A folder opened as a workspace: its root, and — when trusted — its overlay dir.
pub struct Workspace {
    pub root: PathBuf,
    /// `<root>/.aiTerminal`, present only when the folder is trusted AND the dir exists.
    local: Option<PathBuf>,
}

/// What a project WOULD inject, counted without loading a byte of it.
#[derive(Debug, Default, PartialEq)]
pub struct Offering {
    pub agents: usize,
    pub skills: usize,
    pub prompts: usize,
    pub flows: usize,
    pub mcp: usize,
    /// The instructions file present at the root, by name.
    pub instructions: Option<&'static str>,
    pub config: bool,
}

impl Offering {
    /// Whether the project offers anything at all beyond being a folder.
    pub fn is_empty(&self) -> bool {
        self.agents + self.skills + self.prompts + self.flows + self.mcp == 0 && self.instructions.is_none() && !self.config
    }
}

fn count(dir: &Path, ext: &str) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some(ext))
                .count()
        })
        .unwrap_or(0)
}

impl Workspace {
    /// Open `root` as a workspace. `trusted` decides whether the project's own
    /// `.aiTerminal/` takes part; untrusted means global config only.
    pub fn open(root: &Path, trusted: bool) -> Workspace {
        let dot = root.join(DOTDIR);
        Workspace { root: root.to_path_buf(), local: (trusted && dot.is_dir()).then_some(dot) }
    }

    /// Whether the project overlay is live.
    pub fn overlaid(&self) -> bool {
        self.local.is_some()
    }

    /// What this root's project would inject — for the trust prompt. Pure counting.
    pub fn offering(root: &Path) -> Offering {
        let dot = root.join(DOTDIR);
        Offering {
            agents: count(&dot.join("agents"), "md"),
            skills: count(&dot.join("skills"), "md"),
            prompts: count(&dot.join("prompts"), "md"),
            flows: count(&dot.join("flows"), "toml"),
            mcp: count(&dot.join("mcp"), "toml"),
            instructions: match (root.join(OURS).is_file(), root.join(CONVENTION).is_file()) {
                (true, _) => Some(OURS),
                (false, true) => Some(CONVENTION),
                _ => None,
            },
            config: dot.join("config.toml").is_file(),
        }
    }

    /// A digest of everything trust covers that can EXECUTE or reconfigure: the mcp
    /// declarations and the project config. Instructions and agent prose steer a
    /// model that is already guarded; these two run code and change settings, so a
    /// change to them (a `git pull` away) re-opens the question.
    pub fn fingerprint(root: &Path) -> String {
        let dot = root.join(DOTDIR);
        let mut bytes: Vec<u8> = Vec::new();
        let mut mcp: Vec<PathBuf> = std::fs::read_dir(dot.join("mcp"))
            .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml")).collect())
            .unwrap_or_default();
        mcp.sort();
        for p in mcp {
            bytes.extend_from_slice(p.file_name().and_then(|n| n.to_str()).unwrap_or_default().as_bytes());
            bytes.extend_from_slice(&std::fs::read(&p).unwrap_or_default());
        }
        bytes.extend_from_slice(&std::fs::read(dot.join("config.toml")).unwrap_or_default());
        corelib::codec::sha256_hex(&bytes)
    }

    fn dirs(&self, kind: &str, global: PathBuf) -> Vec<PathBuf> {
        match &self.local {
            Some(dot) if dot.join(kind).is_dir() => vec![dot.join(kind), global],
            _ => vec![global],
        }
    }

    pub fn agents_dirs(&self) -> Vec<PathBuf> {
        self.dirs("agents", crate::config::Config::agents_dir())
    }
    pub fn skills_dirs(&self) -> Vec<PathBuf> {
        self.dirs("skills", crate::config::Config::skills_dir())
    }
    pub fn prompts_dirs(&self) -> Vec<PathBuf> {
        self.dirs("prompts", crate::config::Config::prompts_dir())
    }
    pub fn flows_dirs(&self) -> Vec<PathBuf> {
        self.dirs("flows", crate::config::Config::flows_dir())
    }
    pub fn mcp_dirs(&self) -> Vec<PathBuf> {
        self.dirs("mcp", crate::config::Config::mcp_dir())
    }

    /// The instruction sections for a workspace turn: the global `aiTerminal.md`,
    /// then the project's own file — `aiTerminal.md`, else the `AGENTS.md`
    /// convention shared with other harnesses. Project last: nearer wins by
    /// position, the way every overlay here works. The project file is read from
    /// the ROOT (committed, visible), trusted or not — it is prose for a guarded
    /// model, the same standing as the prompt itself.
    pub fn instructions(&self, global: &str) -> String {
        let mut out = String::new();
        if !global.trim().is_empty() {
            out.push_str(&format!("## Global instructions (aiTerminal.md)\n{}\n\n", global.trim()));
        }
        if let Some((name, text)) = self.project_instructions() {
            out.push_str(&format!("## This project's instructions ({name})\n{}\n\n", text.trim()));
        }
        out
    }

    /// The project's instruction file, by precedence, when it has one.
    pub fn project_instructions(&self) -> Option<(&'static str, String)> {
        for name in [OURS, CONVENTION] {
            if let Ok(text) = std::fs::read_to_string(self.root.join(name)) {
                if !text.trim().is_empty() {
                    return Some((name, text));
                }
            }
        }
        None
    }

    /// The effective config: the machine's own, with the project's `config.toml`
    /// applied on top — `[ai]` (a declared pool REPLACES), bounds, and the rest of
    /// the ordinary sections. The `[guard]` section is STRIPPED here and routed
    /// through [`project_rules`](Self::project_rules) instead, because `apply_toml`
    /// appends guard rules verbatim and a project's allow rule must never reach the
    /// compiler.
    pub fn config(&self, base: &crate::config::Config) -> crate::config::Config {
        let mut cfg = base.clone();
        let Some(text) = self.local.as_ref().and_then(|dot| std::fs::read_to_string(dot.join("config.toml")).ok()) else {
            return cfg;
        };
        if let Ok(corelib::wire::Toml::Table(mut pairs)) = corelib::wire::Toml::parse(&text) {
            pairs.retain(|(k, _)| k != "guard");
            cfg.apply_toml(&corelib::wire::Toml::Table(pairs).to_string());
        }
        cfg
    }

    /// The project's guard contribution, tightened. `None` when there is nothing.
    pub fn project_rules(&self) -> Option<RuleSet> {
        let text = std::fs::read_to_string(self.local.as_ref()?.join("config.toml")).ok()?;
        let doc = corelib::wire::Toml::parse(&text).ok()?;
        let rules = RuleSet::parse(doc.get("guard")?);
        let (kept, dropped) = tighten(rules);
        for why in dropped {
            eprintln!("aiTerminal: project guard rule dropped \u{2014} {why}");
        }
        (!kept.is_empty()).then_some(kept)
    }
}

/// Keep what tightens, drop what loosens, and name every drop.
///
/// Deny, confirm and read-only make the machine refuse MORE; a secret rule makes it
/// hide more. Allow and auto are the loosening tiers — from a repo they would be a
/// way to whitelist `curl | sh` by cloning — so they never reach the compiler.
pub fn tighten(rules: RuleSet) -> (RuleSet, Vec<String>) {
    let mut dropped = Vec::new();
    let commands = rules
        .commands
        .into_iter()
        .filter(|c| match c.rule {
            CommandRule::Deny | CommandRule::Confirm => true,
            CommandRule::Allow | CommandRule::Auto => {
                dropped.push(format!("command {:?} \u{2014} a project may not allow-list commands", c.pattern));
                false
            }
        })
        .collect();
    let paths = rules
        .paths
        .into_iter()
        .filter(|p| match p.rule {
            PathRule::Deny | PathRule::ReadOnly => true,
            PathRule::Allow => {
                dropped.push(format!("path {:?} \u{2014} a project may not allow-list paths", p.pattern));
                false
            }
        })
        .collect();
    (RuleSet { commands, paths, secrets: rules.secrets }, dropped)
}

#[cfg(test)]
mod tests;
