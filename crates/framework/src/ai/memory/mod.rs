//! The harness **memory** — structured, retrieval-based recall the AI uses to ground
//! itself, stored as plain `<id>.md` files (frontmatter header + body). No database,
//! no embeddings, no external crates: a from-scratch BM25 ranker ([`retrieve`]) makes
//! it **model-agnostic** and offline. Memory is layered like agents/skills — a
//! stored as plain Markdown files in the global `ai/memory/` store.
//! (`~/.aiTerminal/.terminal/memory/`) on a same-id collision.
//!
//! Design: a file-backed [`MemoryStore`] (load/save/remove), a [`Retriever`] strategy
//! ([`Bm25Retriever`]), and a [`MemoryService`] facade (`add`/`search`/`recall`/`get`/
//! `update`/`forget`/`consolidate`/`stats`). The service is **stateless over disk**
//! (each call loads what it needs, like `ai::session`) with one optimization: a
//! process-wide corpus cache keyed by a cheap mtime/size stamp over the read dirs,
//! so per-turn auto-recall stats the store instead of re-reading + re-parsing every
//! file. Any write moves the stamp, so the cache can never serve stale entries.

mod retrieve;
pub use retrieve::{tokenize, Bm25Retriever, Retriever};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use corelib::wire::{Frontmatter, Json, Toml};

/// The memory kinds (the `kind` frontmatter field). Free-form text is normalized to
/// the closest of these, defaulting to `fact`.
pub const KINDS: &[&str] = &["fact", "preference", "decision", "task", "reference"];

/// Token overlap at which two memories are the same memory. ONE constant, used by
/// both `add` (refuse to write the duplicate) and `consolidate` (merge the ones
/// already written), so the two can never disagree about what a duplicate is.
const DEDUP_SIMILARITY: f32 = 0.8;

/// One memory: a typed, tagged note with a salience + recency record for ranking.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryEntry {
    pub id: String,
    pub kind: String,
    pub tags: Vec<String>,
    /// Importance weight (boosts ranking). Starts at 1.0; reinforced on recall.
    pub salience: f32,
    pub created: u64,
    /// Last write / reinforcement — the recency clock the ranker decays from.
    pub updated: u64,
    pub recalls: u32,
    /// Ids of related memories. Recall follows these one hop, so retrieving a
    /// decision brings the note it depends on — the reason it was made rarely shares
    /// enough words with the question to rank on its own.
    pub links: Vec<String>,
    pub body: String,
}

impl MemoryEntry {
    pub fn new(id: String, kind: String, tags: Vec<String>, body: String, now: u64) -> Self {
        MemoryEntry { id, kind, tags, salience: 1.0, created: now, updated: now, recalls: 0, links: Vec::new(), body }
    }

    /// The text the retriever indexes: body + tags + kind (so a tag match also scores).
    pub fn searchable(&self) -> String {
        format!("{} {} {}", self.body, self.tags.join(" "), self.kind)
    }

    /// First non-empty line, truncated — for list rows.
    pub fn preview(&self) -> String {
        let line = self.body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        if line.chars().count() > 80 {
            format!("{}\u{2026}", line.chars().take(79).collect::<String>())
        } else {
            line.to_string()
        }
    }

    /// Serialize as a frontmatter `.md` document.
    fn to_markdown(&self) -> String {
        let quoted = |v: &Vec<String>| v.iter().map(|t| format!("\"{}\"", t.replace('"', "'"))).collect::<Vec<_>>().join(", ");
        format!(
            "---\nkind = \"{}\"\ntags = [{}]\nsalience = {}\ncreated = {}\nupdated = {}\nrecalls = {}\nlinks = [{}]\n---\n{}\n",
            self.kind, quoted(&self.tags), self.salience, self.created, self.updated, self.recalls, quoted(&self.links), self.body.trim()
        )
    }

    /// Parse from a frontmatter `.md` document (the file stem is the id).
    fn parse(id: &str, text: &str) -> MemoryEntry {
        let fm = Frontmatter::parse(text);
        let h = &fm.header;
        let strings = |key: &str| -> Vec<String> {
            h.get(key)
                .and_then(Toml::as_array)
                .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let tags = strings("tags");
        // `links = [...]` is authoritative; `[[id]]` in the body is the human way to
        // write the same thing, so both are read and merged. Someone editing a memory
        // by hand should not have to keep two lists in step.
        let mut links = strings("links");
        for id in body_links(&fm.body) {
            if !links.contains(&id) {
                links.push(id);
            }
        }
        MemoryEntry {
            id: id.to_string(),
            kind: normalize_kind(h.get("kind").and_then(Toml::as_str).unwrap_or("fact")).to_string(),
            tags,
            salience: h.get("salience").and_then(Toml::as_num).map(|n| n as f32).unwrap_or(1.0),
            created: h.get("created").and_then(Toml::as_int).unwrap_or(0) as u64,
            updated: h.get("updated").and_then(Toml::as_int).unwrap_or(0) as u64,
            recalls: h.get("recalls").and_then(Toml::as_int).unwrap_or(0) as u32,
            links,
            body: fm.body.trim().to_string(),
        }
    }

    /// `{id, kind, tags, salience, created, updated, recalls, preview}` for app State.
    pub fn to_json(&self) -> Json {
        Json::Obj(vec![
            ("id".into(), Json::Str(self.id.clone())),
            ("kind".into(), Json::Str(self.kind.clone())),
            ("tags".into(), Json::Arr(self.tags.iter().map(|t| Json::Str(t.clone())).collect())),
            ("salience".into(), Json::Num(self.salience as f64)),
            ("created".into(), Json::Num(self.created as f64)),
            ("updated".into(), Json::Num(self.updated as f64)),
            ("recalls".into(), Json::Num(self.recalls as f64)),
            ("links".into(), Json::Arr(self.links.iter().map(|l| Json::Str(l.clone())).collect())),
            ("preview".into(), Json::Str(self.preview())),
            ("body".into(), Json::Str(self.body.clone())),
        ])
    }
}

/// One corpus-cache row: the read dirs it covers, their stamp, and the entries.
type CorpusCacheEntry = (Vec<PathBuf>, u64, std::sync::Arc<Vec<(PathBuf, MemoryEntry)>>);

#[cfg(test)]
thread_local! {
    /// Counts real disk passes (per thread) so tests can assert the cache works.
    static DISK_LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A cheap change stamp over the memory dirs: a wrapping sum of every `.md`
/// file's mtime + size + a count. Any write/delete moves it; computing it costs
/// stats, not reads (the `config_stamp` pattern from the GUI's config poll).
fn dirs_stamp(dirs: &[PathBuf]) -> u64 {
    let mut stamp: u64 = 0;
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            if let Ok(md) = e.metadata() {
                let mtime = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                stamp = stamp.wrapping_add(mtime).wrapping_add(md.len()).wrapping_add(1);
            }
        }
    }
    stamp
}

/// File I/O over one memory directory (pure over a `dir`, like `ai::session`).
pub struct MemoryStore;

impl MemoryStore {
    /// Load every `<id>.md` in `dir` (ignores unreadable files).
    pub fn load_dir(dir: &Path) -> Vec<MemoryEntry> {
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut out = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let Some(id) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            if let Ok(text) = std::fs::read_to_string(&p) {
                out.push(MemoryEntry::parse(id, &text));
            }
        }
        out
    }

    pub fn save(dir: &Path, e: &MemoryEntry) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(format!("{}.md", sanitize(&e.id))), e.to_markdown())
    }

    pub fn remove(dir: &Path, id: &str) -> bool {
        std::fs::remove_file(dir.join(format!("{}.md", sanitize(id)))).is_ok()
    }
}

/// The memory facade the capability + CLI call. Holds the write dir + the read
/// order (first dir wins on an id collision), plus the ranker.
pub struct MemoryService {
    write_dir: PathBuf,
    read_dirs: Vec<PathBuf>,
    retriever: Bm25Retriever,
}

impl MemoryService {
    /// Construct over explicit dirs — pure, for tests.
    pub fn with_dirs(write_dir: PathBuf, read_dirs: Vec<PathBuf>) -> Self {
        MemoryService { write_dir, read_dirs, retriever: Bm25Retriever::default() }
    }

    /// Open the global memory store (`~/.aiTerminal/ai/memory/`).
    pub fn open() -> Self {
        let global = crate::config::Config::memory_dir();
        Self::with_dirs(global.clone(), vec![global])
    }

    /// Open a FOLDER-scoped store: writes land in the folder's session memory, and
    /// recall reads the folder store FIRST, then the global store (first-wins shadowing).
    /// So an agent working in a project remembers project facts locally while still
    /// recalling everything durable from the global store.
    pub fn for_folder(folder_mem: PathBuf) -> Self {
        let global = crate::config::Config::memory_dir();
        Self::with_dirs(folder_mem.clone(), vec![folder_mem, global])
    }

    /// Every memory across the read dirs (first dir wins on a same-id collision).
    /// Served from the process-wide corpus cache while the dirs' stamp is unchanged.
    fn load_all(&self) -> std::sync::Arc<Vec<(PathBuf, MemoryEntry)>> {
        static CACHE: std::sync::Mutex<Vec<CorpusCacheEntry>> = std::sync::Mutex::new(Vec::new());
        let stamp = dirs_stamp(&self.read_dirs);
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, st, entries)) = cache.iter().find(|(dirs, ..)| dirs == &self.read_dirs) {
            if *st == stamp {
                return entries.clone();
            }
        }
        let fresh = std::sync::Arc::new(self.load_all_disk());
        cache.retain(|(dirs, ..)| dirs != &self.read_dirs);
        cache.push((self.read_dirs.clone(), stamp, fresh.clone()));
        fresh
    }

    /// The uncached disk pass behind [`load_all`](Self::load_all).
    fn load_all_disk(&self) -> Vec<(PathBuf, MemoryEntry)> {
        #[cfg(test)]
        DISK_LOADS.with(|c| c.set(c.get() + 1));
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for dir in &self.read_dirs {
            for e in MemoryStore::load_dir(dir) {
                if seen.insert(e.id.clone()) {
                    out.push((dir.clone(), e));
                }
            }
        }
        out
    }

    /// All memories, newest-updated first.
    pub fn list(&self) -> Vec<MemoryEntry> {
        let mut v: Vec<MemoryEntry> = self.load_all().iter().map(|(_, e)| e.clone()).collect();
        v.sort_by(|a, b| b.updated.cmp(&a.updated));
        v
    }

    /// Add a memory — or, when the store already holds one saying the same thing,
    /// **reinforce that one instead**.
    ///
    /// An agent re-learns the same fact constantly: it reads a config, saves what it
    /// found, and does it again next run. Writing a second near-identical file made
    /// both rank and both get recalled, so the model paid twice to be told one thing,
    /// and the corpus grew without gaining anything. `consolidate` cleaned it up
    /// eventually — but only if someone ran it.
    ///
    /// Reinforcing is also more truthful about what happened: learning a fact twice
    /// is evidence the fact matters, which is exactly what salience records.
    pub fn add(&self, kind: &str, tags: Vec<String>, body: &str) -> std::io::Result<MemoryEntry> {
        if let Some((dir, mut existing)) = self.near_duplicate(body) {
            for t in tags {
                if !existing.tags.contains(&t) {
                    existing.tags.push(t);
                }
            }
            existing.salience = (existing.salience + 0.3).min(5.0);
            existing.updated = now_unix();
            MemoryStore::save(&dir, &existing)?;
            return Ok(existing);
        }
        let mut e = MemoryEntry::new(make_id(body), normalize_kind(kind).to_string(), tags, body.to_string(), now_unix());
        e.links = body_links(body);
        MemoryStore::save(&self.write_dir, &e)?;
        Ok(e)
    }

    /// The stored memory that already says what `body` says, if there is one. The same
    /// token-overlap measure `consolidate` merges on, so "would be merged later" and
    /// "is not written now" can never disagree.
    fn near_duplicate(&self, body: &str) -> Option<(PathBuf, MemoryEntry)> {
        self.load_all().iter().find(|(_, e)| jaccard(&e.body, body) >= DEDUP_SIMILARITY).cloned()
    }

    /// Link `from` to `to` (both directions — a relation nobody can follow backwards
    /// is half a relation). Returns whether both ids exist.
    pub fn link(&self, from: &str, to: &str) -> bool {
        if from == to {
            return false;
        }
        let (Some((from_dir, mut a)), Some((to_dir, mut b))) = (self.find(from), self.find(to)) else {
            return false;
        };
        if !a.links.contains(&b.id) {
            a.links.push(b.id.clone());
            let _ = MemoryStore::save(&from_dir, &a);
        }
        if !b.links.contains(&a.id) {
            b.links.push(a.id.clone());
            let _ = MemoryStore::save(&to_dir, &b);
        }
        true
    }

    /// Rank memories against `query`, returning the top `k` `(entry, score)` — READ-ONLY.
    pub fn search(&self, query: &str, k: usize) -> Vec<(MemoryEntry, f32)> {
        let cached = self.load_all();
        let all: Vec<MemoryEntry> = cached.iter().map(|(_, e)| e.clone()).collect();
        let ranked = self.retriever.rank(query, &all, now_unix());
        ranked.into_iter().take(k).map(|(i, s)| (all[i].clone(), s)).collect()
    }

    /// The top `k` memories relevant to `context`, filtered to strong matches, plus
    /// one hop along their links — for auto-recall injection. READ-ONLY (never churns
    /// disk).
    ///
    /// The hop is the point of links: a decision ranks because it shares words with
    /// the question, while the reason it was made usually does not. Following the
    /// relation retrieves what lexical matching structurally cannot.
    pub fn recall(&self, context: &str, k: usize) -> Vec<MemoryEntry> {
        let hits = self.search(context, k.max(1) * 2);
        let Some((_, top)) = hits.first() else { return Vec::new() };
        let floor = (top * 0.35).max(0.15);
        let direct: Vec<MemoryEntry> = hits.into_iter().filter(|(_, s)| *s >= floor).take(k).map(|(e, _)| e).collect();

        // One hop only. Two would pull in most of a well-linked store, which is the
        // opposite of what a budgeted context needs.
        let have: HashSet<String> = direct.iter().map(|e| e.id.clone()).collect();
        let wanted: Vec<String> = direct.iter().flat_map(|e| e.links.iter().cloned()).filter(|id| !have.contains(id)).collect();
        if wanted.is_empty() {
            return direct;
        }
        let all = self.load_all();
        let mut out = direct;
        for id in wanted {
            if let Some((_, e)) = all.iter().find(|(_, e)| e.id == id) {
                out.push(e.clone());
            }
        }
        out
    }

    fn find(&self, id: &str) -> Option<(PathBuf, MemoryEntry)> {
        self.load_all().iter().find(|(_, e)| e.id == id).cloned()
    }

    /// Fetch a memory by id and REINFORCE it (recalls+1, salience bump, updated=now).
    pub fn get(&self, id: &str) -> Option<MemoryEntry> {
        let (dir, mut e) = self.find(id)?;
        e.recalls += 1;
        e.salience = (e.salience + 0.2).min(5.0);
        e.updated = now_unix();
        let _ = MemoryStore::save(&dir, &e);
        Some(e)
    }

    /// Edit a memory in place (any of body/tags/kind). `updated` bumps.
    pub fn update(&self, id: &str, body: Option<&str>, tags: Option<Vec<String>>, kind: Option<&str>) -> Option<MemoryEntry> {
        let (dir, mut e) = self.find(id)?;
        if let Some(b) = body {
            if !b.trim().is_empty() {
                e.body = b.trim().to_string();
            }
        }
        if let Some(t) = tags {
            e.tags = t;
        }
        if let Some(k) = kind {
            e.kind = normalize_kind(k).to_string();
        }
        e.updated = now_unix();
        let _ = MemoryStore::save(&dir, &e);
        Some(e)
    }

    /// Delete a memory. Returns whether it existed.
    pub fn forget(&self, id: &str) -> bool {
        match self.find(id) {
            Some((dir, e)) => MemoryStore::remove(&dir, &e.id),
            None => false,
        }
    }

    /// Tidy the store: merge near-duplicates (high token overlap — keep the
    /// higher-salience one) and prune stale, low-salience, never-recalled notes.
    /// Returns `(merged, pruned)`.
    pub fn consolidate(&self) -> (usize, usize) {
        let now = now_unix();
        let mut all: Vec<(PathBuf, MemoryEntry)> = self.load_all().as_ref().clone();
        let (mut merged, mut pruned) = (0, 0);
        // Merge: O(n²) over a project-scale corpus is fine.
        let mut drop: HashSet<usize> = HashSet::new();
        for i in 0..all.len() {
            if drop.contains(&i) {
                continue;
            }
            for j in (i + 1)..all.len() {
                if drop.contains(&j) {
                    continue;
                }
                if jaccard(&all[i].1.searchable(), &all[j].1.searchable()) >= DEDUP_SIMILARITY {
                    // keep the higher-salience entry, forget the other
                    let (keep, lose) = if all[i].1.salience >= all[j].1.salience { (i, j) } else { (j, i) };
                    MemoryStore::remove(&all[lose].0, &all[lose].1.id);
                    drop.insert(lose);
                    merged += 1;
                    if lose == i {
                        break;
                    }
                    let _ = keep;
                }
            }
        }
        // Prune: stale (>30d), weak (<0.5 salience), never recalled.
        for (k, (dir, e)) in all.iter().enumerate() {
            if drop.contains(&k) {
                continue;
            }
            let age_days = now.saturating_sub(e.updated) / 86_400;
            if e.recalls == 0 && e.salience < 0.5 && age_days > 30 {
                MemoryStore::remove(dir, &e.id);
                pruned += 1;
            }
        }
        all.clear();
        (merged, pruned)
    }

    /// `{count, by_kind:{...}, total_recalls}` for the inspector.
    pub fn stats(&self) -> Json {
        let all = self.list();
        let mut by_kind: HashMap<String, u32> = HashMap::new();
        let mut recalls = 0u64;
        for e in &all {
            *by_kind.entry(e.kind.clone()).or_insert(0) += 1;
            recalls += e.recalls as u64;
        }
        Json::Obj(vec![
            ("count".into(), Json::Num(all.len() as f64)),
            ("total_recalls".into(), Json::Num(recalls as f64)),
            ("by_kind".into(), Json::Obj(by_kind.into_iter().map(|(k, v)| (k, Json::Num(v as f64))).collect())),
        ])
    }
}

/// The `[[id]]` references written in a memory's body — the human way to link one
/// note to another, the same notation a wiki uses.
fn body_links(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(open) = rest.find("[[") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("]]") else { break };
        let id = sanitize(after[..close].trim());
        if !id.is_empty() && !out.contains(&id) {
            out.push(id);
        }
        rest = &after[close + 2..];
    }
    out
}

/// Normalize a free-form kind to one of [`KINDS`] (default `fact`).
fn normalize_kind(k: &str) -> &'static str {
    let k = k.trim().to_lowercase();
    KINDS.iter().copied().find(|kind| *kind == k).unwrap_or("fact")
}

/// Token-set Jaccard similarity of two texts (for dedup in `consolidate`).
fn jaccard(a: &str, b: &str) -> f32 {
    let ta: HashSet<String> = tokenize(a).into_iter().collect();
    let tb: HashSet<String> = tokenize(b).into_iter().collect();
    if ta.is_empty() && tb.is_empty() {
        return 1.0;
    }
    let inter = ta.intersection(&tb).count() as f32;
    let union = ta.union(&tb).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// `<unix-millis>-<slug>` — a stable, filesystem-safe id derived from the body, so a
/// memory is human-identifiable; the millisecond stamp keeps distinct adds unique.
fn make_id(body: &str) -> String {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let slug: String = tokenize(body).into_iter().take(5).collect::<Vec<_>>().join("-").chars().take(40).collect();
    if slug.is_empty() {
        format!("{stamp}-mem")
    } else {
        format!("{stamp}-{slug}")
    }
}

/// Keep an id filesystem-safe (no path traversal): ASCII alnum, `-`, `_` only.
fn sanitize(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect()
}

/// Current unix time (seconds), via std — no external crate.
fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc() -> (MemoryService, PathBuf) {
        let dir = std::env::temp_dir().join(format!("aiterm-mem-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        (MemoryService::with_dirs(dir.clone(), vec![dir.clone()]), dir)
    }

    #[test]
    fn add_round_trips_through_frontmatter() {
        let (s, dir) = svc();
        let e = s.add("fact", vec!["api".into()], "API base is /v2; auth via X-Token").unwrap();
        let loaded = s.get(&e.id).unwrap();
        assert_eq!(loaded.body, "API base is /v2; auth via X-Token");
        assert_eq!(loaded.kind, "fact");
        assert_eq!(loaded.tags, vec!["api".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_finds_relevant_and_recall_filters_noise() {
        let (s, dir) = svc();
        s.add("fact", vec![], "Deploy runs on push to main").unwrap();
        s.add("fact", vec![], "Prod region is us-east-1").unwrap();
        s.add("fact", vec![], "The office plant needs watering on Fridays").unwrap();
        let hits = s.search("how to deploy", 5);
        assert_eq!(hits[0].0.body, "Deploy runs on push to main");
        let recalled = s.recall("deploy to production", 5);
        assert!(recalled.iter().any(|m| m.body.contains("Deploy")));
        assert!(!recalled.iter().any(|m| m.body.contains("plant")), "noise filtered out of recall");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_reinforces_salience_and_recalls() {
        let (s, dir) = svc();
        let e = s.add("fact", vec![], "rate limit is 100 rpm").unwrap();
        assert_eq!(e.recalls, 0);
        let r1 = s.get(&e.id).unwrap();
        assert_eq!(r1.recalls, 1);
        assert!(r1.salience > e.salience, "salience reinforced on recall");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn learning_a_fact_twice_reinforces_it_instead_of_writing_it_twice() {
        // An agent re-learns the same thing constantly — it reads a config, saves what
        // it found, and does it again next run. Two files both ranked, so the model
        // paid twice to be told one thing.
        let (s, dir) = svc();
        let first = s.add("fact", vec!["deploy".into()], "Deploys go through `make ship`, never push to main").unwrap();
        let again = s.add("decision", vec!["ci".into()], "Deploys go through `make ship` and never push to main").unwrap();

        assert_eq!(again.id, first.id, "the same fact is one memory");
        assert_eq!(s.list().len(), 1, "no near-duplicate file was written");
        assert!(again.salience > first.salience, "learning it twice is evidence it matters");
        assert!(again.tags.contains(&"deploy".to_string()) && again.tags.contains(&"ci".to_string()), "tags merge: {:?}", again.tags);

        // A genuinely different fact is still a new memory.
        let other = s.add("fact", vec![], "The staging database resets every Sunday").unwrap();
        assert_ne!(other.id, first.id);
        assert_eq!(s.list().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recall_follows_links_one_hop() {
        // A decision ranks because it shares words with the question; the reason it was
        // made usually does not. Following the relation retrieves what lexical
        // matching structurally cannot.
        let (s, dir) = svc();
        let decision = s.add("decision", vec!["deploy".into()], "Deploys go through `make ship`").unwrap();
        let why = s.add("fact", vec![], "Direct pushes skipped the migration step and corrupted two tenants").unwrap();
        assert!(s.link(&decision.id, &why.id));

        let recalled = s.recall("how do we deploy", 3);
        assert!(recalled.iter().any(|m| m.id == decision.id), "the decision ranks on its own words");
        assert!(recalled.iter().any(|m| m.id == why.id), "and brings its reason, which shares no query words");

        // The relation is followable in both directions.
        assert!(s.get(&why.id).unwrap().links.contains(&decision.id));
        // Linking is refused when an id is wrong, rather than silently doing nothing.
        assert!(!s.link(&decision.id, "no-such-id"));
        assert!(!s.link(&decision.id, &decision.id), "a memory cannot link to itself");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn links_survive_the_file_and_can_be_written_by_hand() {
        let (s, dir) = svc();
        let a = s.add("fact", vec![], "the first note").unwrap();
        let b = s.add("fact", vec![], "a completely unrelated second note").unwrap();
        s.link(&a.id, &b.id);
        // Round-trips through the frontmatter.
        assert!(s.get(&a.id).unwrap().links.contains(&b.id));

        // And someone editing the file by hand can write `[[id]]` in the body instead
        // of maintaining the frontmatter list.
        let hand_written = format!("---\nkind = \"fact\"\n---\nSee [[{}]] for the reason.\n", b.id);
        std::fs::write(dir.join("hand-written.md"), hand_written).unwrap();
        let parsed = MemoryService::with_dirs(dir.clone(), vec![dir.clone()])
            .list()
            .into_iter()
            .find(|e| e.id == "hand-written")
            .expect("the hand-written note loaded");
        assert!(parsed.links.contains(&b.id), "a [[link]] in the body counts: {:?}", parsed.links);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_exact_tag_beats_the_same_word_in_prose() {
        // A tag is a deliberate act; the same word in a body may be an aside. BM25
        // alone cannot tell them apart, because it sees one flat bag of words.
        // Same word, same number of times, in bodies of the same length — the ONLY
        // difference is that somebody tagged one of them.
        let (s, dir) = svc();
        let tagged = s.add("fact", vec!["release".into()], "We cut a release from a dated branch").unwrap();
        let untagged = s.add("fact", vec![], "Ada brought cake to the release party").unwrap();
        let hits = s.search("release", 5);
        let rank = |id: &str| hits.iter().position(|(e, _)| e.id == id).expect("both matched");
        assert!(
            rank(&tagged.id) < rank(&untagged.id),
            "the deliberately tagged note wins: {:?}",
            hits.iter().map(|(e, s)| (&e.body, s)).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forget_removes() {
        let (s, dir) = svc();
        let e = s.add("fact", vec![], "ephemeral note").unwrap();
        assert!(s.forget(&e.id));
        assert!(s.get(&e.id).is_none());
        assert!(!s.forget(&e.id), "forgetting a missing id is false");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn consolidate_merges_duplicates() {
        // `add` now refuses to write a near-duplicate, so duplicates only reach the
        // store the other ways: a file written by hand, or a note that predates the
        // check. That is still `consolidate`'s job, and this is now the honest fixture
        // for it — going through `add` twice would silently test nothing.
        let (s, dir) = svc();
        s.add("fact", vec![], "Deploy runs on push to the main branch").unwrap();
        std::fs::write(
            dir.join("hand-written-copy.md"),
            "---\nkind = \"fact\"\n---\nDeploy runs on push to the main branch\n",
        )
        .unwrap();
        assert_eq!(s.list().len(), 2, "two files really are on disk");

        let (merged, _pruned) = s.consolidate();
        assert!(merged >= 1, "near-duplicate merged");
        assert_eq!(s.list().len(), 1, "one survives");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_read_dir_wins_on_same_id() {
        let base = std::env::temp_dir().join(format!("aiterm-mem-shadow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let proj = base.join("proj");
        let global = base.join("global");
        let g = MemoryService::with_dirs(global.clone(), vec![global.clone()]);
        let e = g.add("fact", vec![], "global value").unwrap();
        // Write an entry in the FIRST dir with the SAME id but different body.
        let mut pe = e.clone();
        pe.body = "project value".into();
        MemoryStore::save(&proj, &pe).unwrap();
        let s = MemoryService::with_dirs(proj.clone(), vec![proj, global]);
        assert_eq!(s.list().len(), 1, "deduped by id");
        assert_eq!(s.get(&e.id).unwrap().body, "project value", "project shadows global");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn for_folder_writes_folder_and_recalls_folder_then_global() {
        // A folder-scoped service writes to the folder store and recalls across BOTH the
        // folder and global stores — the mechanism behind per-folder AI memory.
        let base = std::env::temp_dir().join(format!("aiterm-mem-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let folder = base.join("proj-mem");
        let global = base.join("global-mem");
        // A global fact + a folder-scoped fact via the for_folder-style constructor.
        MemoryService::with_dirs(global.clone(), vec![global.clone()])
            .add("fact", vec![], "the org standard formatter is rustfmt").unwrap();
        let svc = MemoryService::with_dirs(folder.clone(), vec![folder.clone(), global.clone()]);
        let added = svc.add("decision", vec![], "this project deploys via scripts/ship.sh").unwrap();
        // The folder write landed in the FOLDER dir, not global.
        assert!(folder.join(format!("{}.md", added.id)).exists(), "folder write goes to the folder store");
        assert!(!global.join(format!("{}.md", added.id)).exists());
        // Recall reaches both stores.
        assert!(svc.search("ship deploy", 5).iter().any(|(e, _)| e.body.contains("ship.sh")), "folder fact recalled");
        assert!(svc.search("formatter", 5).iter().any(|(e, _)| e.body.contains("rustfmt")), "global fact still recalled");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn corpus_cache_skips_disk_until_the_store_changes() {
        let (svc, dir) = svc();
        svc.add("fact", vec![], "the deploy target is fly.io").unwrap();
        let base = DISK_LOADS.with(|c| c.get());
        assert_eq!(svc.search("deploy", 3).len(), 1);
        let after_first = DISK_LOADS.with(|c| c.get());
        assert!(after_first > base, "first search reads the store");
        // Unchanged store → the second and third searches are pure cache hits.
        assert_eq!(svc.search("deploy", 3).len(), 1);
        svc.list();
        assert_eq!(DISK_LOADS.with(|c| c.get()), after_first, "no re-read while the stamp is stable");
        // A write moves the stamp → the next search re-reads AND sees the new entry.
        svc.add("fact", vec![], "the cache invalidates on write").unwrap();
        assert!(svc.search("cache invalidates", 3).len() >= 1);
        assert!(DISK_LOADS.with(|c| c.get()) > after_first, "a write invalidates the cache");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
