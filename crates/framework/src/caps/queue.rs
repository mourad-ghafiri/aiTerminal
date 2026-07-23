//! The `queue.*` native family — durable, per-app message/work queues (the SDK's
//! `event.*` is ephemeral in-pane pub/sub; this persists). Each queue is a JSON array
//! of `{p:priority, v:item}` entries under `ctx.app_data/queue/<name>.json` (the
//! `store.*` sandbox). `push` emits `queue:item{q,size}` so a worker view can react.
//! Higher priority pops first; equal priority is FIFO.

use std::path::{Path, PathBuf};

use corelib::wire::Json;

use super::host::Host;
use super::object::{MethodSpec, NativeObject};
use super::CapCtx;

pub struct QueueObj;

const SPECS: &[MethodSpec] = &[
    MethodSpec { method: "queue.push", describe: "Enqueue an item" },
    MethodSpec { method: "queue.pop", describe: "Dequeue the next item" },
    MethodSpec { method: "queue.peek", describe: "Read the next item" },
    MethodSpec { method: "queue.size", describe: "Queue length" },
    MethodSpec { method: "queue.list", describe: "List all items" },
    MethodSpec { method: "queue.clear", describe: "Empty a queue" },
    MethodSpec { method: "queue.queues", describe: "List queues" },
];

impl NativeObject for QueueObj {
    fn family(&self) -> &'static str {
        "queue"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        let dir = ctx.app_data.clone().ok_or("queue is only available to installed apps")?.join("queue");
        let arg = |name: &str| args.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("");

        if method == "queue.queues" {
            return Ok(Json::Arr(list_queues(&dir).into_iter().map(Json::Str).collect()));
        }
        let name = arg("q").trim();
        let name = if name.is_empty() { arg("queue").trim() } else { name };
        if name.is_empty() {
            return Err(format!("{method} needs `q=`"));
        }
        let path = queue_path(&dir, name);
        let mut entries = load(&path);

        match method {
            "queue.push" => {
                let item = Json::parse(arg("item")).or_else(|_| Json::parse(arg("value"))).unwrap_or_else(|_| Json::Str(arg("item").to_string()));
                let p = arg("priority").parse::<f64>().unwrap_or(0.0);
                entries.push(Json::Obj(vec![("p".into(), Json::Num(p)), ("v".into(), item)]));
                save(&path, &entries)?;
                Ok(Json::Num(entries.len() as f64))
            }
            "queue.pop" => {
                let Some(i) = head_index(&entries) else { return Ok(Json::Null) };
                let entry = entries.remove(i);
                save(&path, &entries)?;
                Ok(entry.get("v").cloned().unwrap_or(Json::Null))
            }
            "queue.peek" => Ok(head_index(&entries).and_then(|i| entries[i].get("v").cloned()).unwrap_or(Json::Null)),
            "queue.size" => Ok(Json::Num(entries.len() as f64)),
            "queue.list" => Ok(Json::Arr(entries.iter().filter_map(|e| e.get("v").cloned()).collect())),
            "queue.clear" => {
                let _ = std::fs::remove_file(&path);
                Ok(Json::Bool(true))
            }
            _ => Err(format!("unknown queue method '{method}'")),
        }
    }
}

/// The index of the next entry to serve: the highest priority, FIFO among ties.
fn head_index(entries: &[Json]) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            let pa = a.get("p").and_then(Json::as_f64).unwrap_or(0.0);
            let pb = b.get("p").and_then(Json::as_f64).unwrap_or(0.0);
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
        })
        // max_by returns the LAST max on ties; we want the FIRST (FIFO) → re-find it.
        .map(|(_, best)| best.get("p").and_then(Json::as_f64).unwrap_or(0.0))
        .and_then(|top| entries.iter().position(|e| e.get("p").and_then(Json::as_f64).unwrap_or(0.0) == top))
}

fn queue_path(dir: &Path, name: &str) -> PathBuf {
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    dir.join(format!("{safe}.json"))
}

fn load(path: &Path) -> Vec<Json> {
    std::fs::read_to_string(path).ok().and_then(|s| Json::parse(&s).ok()).and_then(|j| j.as_array().map(<[_]>::to_vec)).unwrap_or_default()
}

fn save(path: &Path, entries: &[Json]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, Json::Arr(entries.to_vec()).to_string()).map_err(|e| e.to_string())
}

fn list_queues(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| e.path().file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc() -> (QueueObj, CapCtx, PathBuf) {
        let dir = std::env::temp_dir().join(format!("tt-queue-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = CapCtx {
            policy: std::sync::Arc::new(crate::security::Policy::new()),
            app_data: Some(dir.clone()),
            remote_enabled: true,
            origin: "terminal://ai/".into(),
            sandbox: None,
        };
        (QueueObj, ctx, dir)
    }

    #[test]
    fn fifo_push_pop_peek_size() {
        let (o, ctx, dir) = svc();
        let run = |m: &str, a: &[(&str, &str)]| {
            let args: Vec<(String, String)> = a.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            o.invoke(m, &args, &ctx, &mut crate::caps::host::NullHost)
        };
        run("queue.push", &[("q", "jobs"), ("item", "{\"id\":1}")]).unwrap();
        run("queue.push", &[("q", "jobs"), ("item", "{\"id\":2}")]).unwrap();
        assert_eq!(run("queue.size", &[("q", "jobs")]).unwrap().as_f64(), Some(2.0));
        // FIFO: first pushed pops first
        assert_eq!(run("queue.peek", &[("q", "jobs")]).unwrap().get("id").and_then(Json::as_f64), Some(1.0));
        assert_eq!(run("queue.pop", &[("q", "jobs")]).unwrap().get("id").and_then(Json::as_f64), Some(1.0));
        assert_eq!(run("queue.pop", &[("q", "jobs")]).unwrap().get("id").and_then(Json::as_f64), Some(2.0));
        assert_eq!(run("queue.pop", &[("q", "jobs")]).unwrap(), Json::Null); // empty
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn priority_pops_first_then_fifo() {
        let (o, ctx, dir) = svc();
        let run = |m: &str, a: &[(&str, &str)]| {
            let args: Vec<(String, String)> = a.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            o.invoke(m, &args, &ctx, &mut crate::caps::host::NullHost)
        };
        run("queue.push", &[("q", "q"), ("item", "\"low1\""), ("priority", "0")]).unwrap();
        run("queue.push", &[("q", "q"), ("item", "\"hi\""), ("priority", "5")]).unwrap();
        run("queue.push", &[("q", "q"), ("item", "\"low2\""), ("priority", "0")]).unwrap();
        assert_eq!(run("queue.pop", &[("q", "q")]).unwrap().as_str(), Some("hi")); // priority first
        assert_eq!(run("queue.pop", &[("q", "q")]).unwrap().as_str(), Some("low1")); // then FIFO
        assert_eq!(run("queue.queues", &[]).unwrap().as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
