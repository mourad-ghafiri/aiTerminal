//! The `memory.*` native-object family — the harness's structured, retrieval-based
//! memory exposed as an ordinary capability — a **pure** family operating on the
//! global store files through [`MemoryService`], so it runs identically anywhere
//! `caps::run` does. Mutations additionally emit a trusted `memory:*` system event through
//! the host (a no-op on `NullHost`), so apps can react.
//!
//! Reads + dir-confined writes are all `consent:false` — the model curates its own
//! memory mid-loop without a prompt per save (the differentiator vs a static brief).

use corelib::wire::Json;

use crate::ai::MemoryService;

use super::host::Host;
use super::object::{MethodSpec, NativeObject};
use super::CapCtx;

pub struct MemoryObj;

const SPECS: &[MethodSpec] = &[
    MethodSpec { method: "memory.add", describe: "Save a memory" },
    MethodSpec { method: "memory.search", describe: "Search memories" },
    MethodSpec { method: "memory.recall", describe: "Recall relevant memories" },
    MethodSpec { method: "memory.get", describe: "Read a memory (reinforces it)" },
    MethodSpec { method: "memory.list", describe: "List all memories" },
    MethodSpec { method: "memory.update", describe: "Edit a memory" },
    MethodSpec { method: "memory.forget", describe: "Delete a memory" },
    MethodSpec { method: "memory.link", describe: "Relate two memories (args: from, to)" },
    MethodSpec { method: "memory.consolidate", describe: "Merge + prune memories" },
    MethodSpec { method: "memory.stats", describe: "Memory store stats" },
    MethodSpec { method: "memory.sessions", describe: "Search this folder's past conversations (args: query)" },
];

impl NativeObject for MemoryObj {
    fn family(&self) -> &'static str {
        "memory"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        // In a folder run, curate the FOLDER store (recall folder-first, then global);
        // otherwise the global store. This makes `@coder`'s mid-loop `memory.add` remember
        // project facts where the project can find them again.
        let svc = match &ctx.memory_dir {
            Some(dir) => MemoryService::for_folder(dir.clone()),
            None => MemoryService::open(),
        };
        let arg = |name: &str| args.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("");
        let k = arg("k").parse::<usize>().ok().filter(|n| *n > 0).unwrap_or(5);
        let tags = |name: &str| {
            arg(name).split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect::<Vec<_>>()
        };
        let opt = |name: &str| -> Option<&str> {
            let v = arg(name);
            (!v.is_empty()).then_some(v)
        };
        match method {
            // Cross-session recall: search the FOLDER's redacted conversation
            // logs (the `chat/` dir beside the memory store) for a phrase — the
            // agent's own past sittings, bounded and plain-text.
            "memory.sessions" => {
                let query = arg("query");
                if query.trim().is_empty() {
                    return Err("memory.sessions needs `query=`".into());
                }
                let Some(chat) = ctx.memory_dir.as_ref().and_then(|d| d.parent()).map(|d| d.join("chat")) else {
                    return Err("no folder session here \u{2014} conversations are per-project".into());
                };
                let needle = query.to_lowercase();
                let mut logs: Vec<std::path::PathBuf> = std::fs::read_dir(&chat)
                    .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md")).collect())
                    .unwrap_or_default();
                logs.sort();
                let mut hits: Vec<Json> = Vec::new();
                let mut lines_kept = 0usize;
                // Newest sittings first; bounded files and bounded lines.
                for path in logs.iter().rev().take(5) {
                    let Ok(text) = std::fs::read_to_string(path) else { continue };
                    let stamp = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_string();
                    let matched: Vec<Json> = text
                        .lines()
                        .filter(|l| l.to_lowercase().contains(&needle))
                        .take(40 - lines_kept.min(40))
                        .map(|l| Json::Str(l.chars().take(200).collect()))
                        .collect();
                    if matched.is_empty() {
                        continue;
                    }
                    lines_kept += matched.len();
                    hits.push(Json::Obj(vec![("session".into(), Json::Str(stamp)), ("lines".into(), Json::Arr(matched))]));
                    if lines_kept >= 40 {
                        break;
                    }
                }
                return Ok(Json::Arr(hits));
            }
            "memory.add" => {
                let body = first_nonempty(args, &["text", "body", "note"]);
                if body.trim().is_empty() {
                    return Err("memory.add needs `text=`".into());
                }
                // Scrubbed on the way in. A memory outlives the run that wrote it and is
                // recalled into a later one's context — where a placeholder means nothing
                // and a secret would leak again on a different day, through a different
                // run, with nothing to connect it to this one.
                let e = svc
                    .add(opt("kind").unwrap_or("fact"), tags("tags"), &ctx.guard.scrub(body))
                    .map_err(|e| e.to_string())?;
                Ok(e.to_json())
            }
            "memory.search" => {
                let q = first_nonempty(args, &["query", "q", "text"]);
                Ok(Json::Arr(svc.search(q, k).into_iter().map(|(e, s)| with_score(e.to_json(), s)).collect()))
            }
            "memory.recall" => {
                let q = first_nonempty(args, &["context", "query", "q", "text"]);
                Ok(Json::Arr(svc.recall(q, k).into_iter().map(|e| e.to_json()).collect()))
            }
            "memory.get" => match svc.get(arg("id")) {
                Some(e) => Ok(e.to_json()),
                None => Err(format!("no memory '{}'", arg("id"))),
            },
            "memory.list" => Ok(Json::Arr(svc.list().into_iter().map(|e| e.to_json()).collect())),
            "memory.update" => {
                let id = arg("id");
                let tags = opt("tags").map(|_| tags("tags"));
                match svc.update(id, first_nonempty_opt(args, &["text", "body"]), tags, opt("kind")) {
                    Some(e) => {
                        Ok(e.to_json())
                    }
                    None => Err(format!("no memory '{id}'")),
                }
            }
            "memory.forget" => {
                let id = arg("id");
                if svc.forget(id) {
                    Ok(Json::Bool(true))
                } else {
                    Err(format!("no memory '{id}'"))
                }
            }
            "memory.link" => {
                let (from, to) = (arg("from"), arg("to"));
                if from.is_empty() || to.is_empty() {
                    return Err("memory.link needs `from=` and `to=`".into());
                }
                if svc.link(from, to) {
                    Ok(Json::Bool(true))
                } else {
                    Err(format!("cannot link '{from}' to '{to}' \u{2014} check both ids exist and differ"))
                }
            }
            "memory.consolidate" => {
                let (merged, pruned) = svc.consolidate();
                Ok(Json::Obj(vec![
                    ("merged".into(), Json::Num(merged as f64)),
                    ("pruned".into(), Json::Num(pruned as f64)),
                ]))
            }
            "memory.stats" => Ok(svc.stats()),
            _ => Err(format!("unknown memory method '{method}'")),
        }
    }
}

/// First non-empty value among the named arg aliases (e.g. `text`/`body`/`note`).
fn first_nonempty<'a>(args: &'a [(String, String)], names: &[&str]) -> &'a str {
    names
        .iter()
        .find_map(|n| args.iter().find(|(k, v)| k == n && !v.trim().is_empty()).map(|(_, v)| v.as_str()))
        .unwrap_or("")
}

fn first_nonempty_opt<'a>(args: &'a [(String, String)], names: &[&str]) -> Option<&'a str> {
    let v = first_nonempty(args, names);
    (!v.is_empty()).then_some(v)
}

/// Attach a relevance `score` field to a memory's JSON (search results).
fn with_score(entry: Json, score: f32) -> Json {
    match entry {
        Json::Obj(mut fields) => {
            fields.push(("score".into(), Json::Num(score as f64)));
            Json::Obj(fields)
        }
        other => other,
    }
}
