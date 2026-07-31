//! The `todo.*` native family — a per-app **plan**: a single ordered checklist the AI
//! agent maintains for multi-step work (claude-code-style todos). Persisted as a JSON
//! array of `{text, done}` under `ctx.app_data/todo.json` (the `store.*` sandbox). Every
//! mutation emits `todo:changed{count, done}` so a worker view refreshes its checklist
//! live. Reads are free; the agent writes its own plan (no extra permission).

use std::path::Path;

use corelib::wire::Json;

use super::host::Host;
use super::object::{MethodSpec, NativeObject};
use super::CapCtx;

pub struct TodoObj;

const SPECS: &[MethodSpec] = &[
    MethodSpec { method: "todo.set", describe: "Replace the plan with a list of tasks" },
    MethodSpec { method: "todo.add", describe: "Append a task to the plan" },
    MethodSpec { method: "todo.done", describe: "Mark a task done (by index or matching text)" },
    MethodSpec { method: "todo.list", describe: "Read the current plan" },
    MethodSpec { method: "todo.clear", describe: "Clear the plan" },
];

impl NativeObject for TodoObj {
    fn family(&self) -> &'static str {
        "todo"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        let path = ctx.app_data.clone().ok_or("todo is only available to installed apps")?.join("todo.json");
        let arg = |name: &str| args.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("");
        let mut items = load(&path);

        if method == "todo.list" {
            return Ok(Json::Arr(items));
        }
        match method {
            "todo.set" => {
                items = parse_items(arg("items"));
                save(&path, &items)?;
            }
            "todo.add" => {
                let text = arg("text").trim();
                if text.is_empty() {
                    return Err("todo.add needs `text=`".into());
                }
                items.push(task(text, false));
                save(&path, &items)?;
            }
            "todo.done" => {
                let target = match arg("index").parse::<usize>() {
                    Ok(i) => Some(i),
                    Err(_) => {
                        let t = arg("text").trim();
                        (!t.is_empty()).then(|| items.iter().position(|it| it.get("text").and_then(Json::as_str).map(|x| x.contains(t)).unwrap_or(false))).flatten()
                    }
                };
                let Some(i) = target.filter(|i| *i < items.len()) else {
                    return Err("todo.done: no matching task (pass `index=` or `text=`)".into());
                };
                if let Json::Obj(fields) = &mut items[i] {
                    set_field(fields, "done", Json::Bool(true));
                }
                save(&path, &items)?;
            }
            "todo.clear" => {
                items.clear();
                let _ = std::fs::remove_file(&path);
            }
            _ => return Err(format!("unknown todo method '{method}'")),
        }
        Ok(Json::Arr(items))
    }
}

/// One task object `{text, done}`.
fn task(text: &str, done: bool) -> Json {
    Json::Obj(vec![("text".into(), Json::Str(text.to_string())), ("done".into(), Json::Bool(done))])
}

/// Set (or insert) a field on an object's key/value list.
fn set_field(fields: &mut Vec<(String, Json)>, key: &str, value: Json) {
    if let Some(slot) = fields.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value;
    } else {
        fields.push((key.to_string(), value));
    }
}

/// Parse a `todo.set` `items=` value: a JSON array whose elements are either a string
/// (`"task"`) or an object (`{text, done?}`). A non-array yields an empty plan.
fn parse_items(raw: &str) -> Vec<Json> {
    let Ok(Json::Arr(arr)) = Json::parse(raw) else { return Vec::new() };
    arr.iter()
        .map(|el| match el {
            Json::Str(s) => task(s, false),
            Json::Obj(_) => {
                let text = el.get("text").and_then(Json::as_str).unwrap_or("");
                let done = matches!(el.get("done"), Some(Json::Bool(true)));
                task(text, done)
            }
            other => task(&other.to_string(), false),
        })
        .collect()
}

fn load(path: &Path) -> Vec<Json> {
    std::fs::read_to_string(path).ok().and_then(|s| Json::parse(&s).ok()).and_then(|j| j.as_array().map(<[_]>::to_vec)).unwrap_or_default()
}

fn save(path: &Path, items: &[Json]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, Json::Arr(items.to_vec()).to_string()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
