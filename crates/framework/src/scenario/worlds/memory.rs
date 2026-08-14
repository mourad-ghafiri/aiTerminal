//! Memory — what an agent remembers, where it remembers it, and what it forgets.
//!
//! The store is Markdown files and nothing else: no database, no index to rebuild, one
//! file per memory that you can open in an editor. Its unit tests prove the pieces. What
//! had no test at all was the **journey** — that a fact learned while working in one
//! project is recalled there and is invisible in another, that recall ranks by relevance
//! rather than by whatever was written last, and that forgetting is actually forgetting.
//!
//! Two routes reach the same store, and both run here through their real constructors:
//! the [`MemoryService`](crate::ai::MemoryService) the harness calls, and the `memory.*`
//! capability family an agent calls. They have to agree, because an agent's memory is
//! only worth anything if it is the same memory the run before it wrote to.
//!
//! `$HOME` is locked to a scratch directory for the world's lifetime, so the *global*
//! tier is real — `for_folder` genuinely reads folder-first-then-global — without any of
//! it touching the machine it runs on.

use corelib::wire::{Json, Toml};

use super::super::world::{self, World};
use crate::ai::MemoryService;

pub struct MemoryWorld {
    _home: crate::test_home::HomeGuard,
    root: std::path::PathBuf,
    /// Which folder store is in hand — empty is the global one.
    folder: String,
    /// The most recent `recall`/`search`, in rank order.
    recalled: Vec<String>,
    /// What `consolidate` reported: merged, then pruned.
    consolidated: (usize, usize),
    /// The last error from the capability family, for the refusals.
    refused: Option<String>,
}

pub fn build(_setup: &Toml) -> Result<Box<dyn World>, String> {
    let (home, root) = crate::test_home::lock_home("scenario-memory");
    crate::config::Config::ensure_default();
    Ok(Box::new(MemoryWorld {
        _home: home,
        root,
        folder: String::new(),
        recalled: Vec::new(),
        consolidated: (0, 0),
        refused: None,
    }))
}

impl World for MemoryWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // ── where the agent is working ─────────────────────────────────────────
        if let Some(name) = world::text(step, "in_folder") {
            self.folder = name;
            return Ok(());
        }
        if world::flag(step, "globally") == Some(true) {
            self.folder.clear();
            return Ok(());
        }

        // ── what it remembers ──────────────────────────────────────────────────
        if let Some(body) = world::text(step, "remember") {
            let kind = world::text(step, "kind").unwrap_or_else(|| "note".into());
            let tags = world::list(step, "tags").unwrap_or_default();
            self.service().add(&kind, tags, &body).map_err(|e| e.to_string())?;
            return Ok(());
        }
        if let Some(phrase) = world::text(step, "forget") {
            let id = self.id_of(&phrase)?;
            return match self.service().forget(&id) {
                true => Ok(()),
                false => Err(format!("there was no memory '{id}' to forget")),
            };
        }
        if let Some(phrase) = world::text(step, "revise") {
            let body = world::text(step, "body").ok_or("`revise` needs a `body`")?;
            let id = self.id_of(&phrase)?;
            return match self.service().update(&id, Some(&body), None, None) {
                Some(e) if e.id == id => Ok(()),
                Some(e) => Err(format!("revising changed the id from '{id}' to '{}'", e.id)),
                None => Err(format!("there was no memory '{id}' to revise")),
            };
        }
        if let Some(pair) = world::list(step, "link") {
            let [from, to] = pair.as_slice() else { return Err("`link` takes exactly two memories".into()) };
            let (from, to) = (self.id_of(from)?, self.id_of(to)?);
            return match self.service().link(&from, &to) {
                true => Ok(()),
                false => Err(format!("'{from}' and '{to}' were not linked")),
            };
        }
        if world::flag(step, "consolidate") == Some(true) {
            self.consolidated = self.service().consolidate();
            return Ok(());
        }

        // ── what it recalls ────────────────────────────────────────────────────
        if let Some(about) = world::text(step, "recall") {
            let k = world::int(step, "top").unwrap_or(5).max(1) as usize;
            self.recalled = self.service().recall(&about, k).into_iter().map(|e| e.body).collect();
            return Ok(());
        }
        if let Some(query) = world::text(step, "search") {
            let k = world::int(step, "top").unwrap_or(5).max(1) as usize;
            self.recalled = self.service().search(&query, k).into_iter().map(|(e, _)| e.body).collect();
            return Ok(());
        }

        // ── the route an agent actually takes ──────────────────────────────────
        if let Some(method) = world::text(step, "call") {
            let args: Vec<(String, String)> = world::list(step, "args")
                .unwrap_or_default()
                .iter()
                .filter_map(|a| a.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
                .collect();
            self.refused = match crate::caps::run(&method, &args, &self.ctx()) {
                Ok(value) => {
                    self.recalled = bodies(&value);
                    None
                }
                Err(e) => Some(e),
            };
            return Ok(());
        }

        // ── assertions ─────────────────────────────────────────────────────────
        if let Some(want) = world::list(step, "expect_recalled") {
            return world::expect_contains(&self.recalled.join("\n"), &want, "what was recalled");
        }
        if let Some(bad) = world::list(step, "expect_not_recalled") {
            return world::expect_missing(&self.recalled.join("\n"), &bad, "what was recalled");
        }
        if let Some(want) = world::text(step, "expect_first_recalled") {
            let got = self.recalled.first().cloned().unwrap_or_default();
            return world::expect_contains(&got, &[want], "the top-ranked memory");
        }
        if let Some(want) = world::int(step, "expect_count") {
            let got = self.service().list().len() as i64;
            if got != want {
                return Err(format!("the store holds {got} memories, expected {want}"));
            }
            return Ok(());
        }
        if let Some(want) = world::int(step, "expect_merged") {
            if self.consolidated.0 as i64 != want {
                return Err(format!("consolidate merged {}, expected {want}", self.consolidated.0));
            }
            return Ok(());
        }
        if let Some(want) = world::list(step, "expect_refused") {
            let got = self.refused.clone().ok_or("that call succeeded, and was expected to be refused")?;
            return world::expect_contains(&got, &want, "why the call was refused");
        }
        if world::flag(step, "expect_accepted") == Some(true) {
            return match &self.refused {
                None => Ok(()),
                Some(e) => Err(format!("the call was refused: {e}")),
            };
        }
        Err(world::unknown_verb(step))
    }
}

impl MemoryWorld {
    /// The folder store in hand, or `None` when the agent is working nowhere in
    /// particular and only the global store applies.
    fn folder_dir(&self) -> Option<std::path::PathBuf> {
        if self.folder.is_empty() {
            return None;
        }
        let dir = self.root.join("projects").join(&self.folder).join("memory");
        let _ = std::fs::create_dir_all(&dir);
        Some(dir)
    }

    /// The id of the memory a phrase names. Ids carry a timestamp, so a scenario cannot
    /// write one down — and it should not have to: a person thinks of a memory by what
    /// it says, which is exactly what the retriever is for.
    fn id_of(&self, phrase: &str) -> Result<String, String> {
        self.service()
            .search(phrase, 1)
            .first()
            .map(|(e, _)| e.id.clone())
            .ok_or_else(|| format!("no memory matches {phrase:?}"))
    }

    /// The very constructors the product uses, so a scenario cannot pass by taking a
    /// route the agent never takes.
    fn service(&self) -> MemoryService {
        match self.folder_dir() {
            Some(dir) => MemoryService::for_folder(dir),
            None => MemoryService::open(),
        }
    }

    fn ctx(&self) -> crate::caps::CapCtx {
        crate::caps::CapCtx {
            guard: std::sync::Arc::new(crate::guard::Guard::default()),
            app_data: None,
            remote_enabled: false,
            origin: "scenario://memory/".into(),
            sandbox: None,
            memory_dir: self.folder_dir(), approver: std::sync::Arc::new(crate::guard::NobodyToAsk),
        }
    }
}

/// The `body` of every memory in a capability result, whatever shape it came back in.
fn bodies(value: &Json) -> Vec<String> {
    let one = |v: &Json| v.get("body").and_then(|b| b.as_str()).map(str::to_string);
    match value {
        Json::Arr(items) => items.iter().filter_map(one).collect(),
        other => one(other).into_iter().collect(),
    }
}
