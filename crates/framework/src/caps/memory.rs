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
    MethodSpec { method: "memory.consolidate", describe: "Merge + prune memories" },
    MethodSpec { method: "memory.stats", describe: "Memory store stats" },
];

impl NativeObject for MemoryObj {
    fn family(&self) -> &'static str {
        "memory"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    fn invoke(&self, method: &str, args: &[(String, String)], _ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        let svc = MemoryService::open();
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
            "memory.add" => {
                let body = first_nonempty(args, &["text", "body", "note"]);
                if body.trim().is_empty() {
                    return Err("memory.add needs `text=`".into());
                }
                let e = svc.add(opt("kind").unwrap_or("fact"), tags("tags"), body).map_err(|e| e.to_string())?;
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
