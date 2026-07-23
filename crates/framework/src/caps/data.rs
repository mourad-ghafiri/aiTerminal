//! The `data.*` native family — a structured, per-app **store + query** (the "SQL" the
//! SDK was missing). Each table is a JSON array of row objects, persisted per app under
//! `ctx.app_data/db/<table>.json` (the same sandbox as `store.*`). Queries use a
//! **structured API** (declarative-template-friendly): `where` is a JSON object of
//! equality or `{op:value}` conditions, `order` is `field`/`-field`. The query engine
//! (filter/order/slice over `&[Json]`) is pure + unit-tested; mutations emit
//! `data:changed{table}` so reactive views refresh.

use std::path::{Path, PathBuf};

use corelib::wire::Json;

use super::host::Host;
use super::object::{MethodSpec, NativeObject};
use super::CapCtx;

pub struct DataObj;

const SPECS: &[MethodSpec] = &[
    MethodSpec { method: "data.insert", describe: "Insert a row" },
    MethodSpec { method: "data.query", describe: "Query rows" },
    MethodSpec { method: "data.get", describe: "Get a row by id" },
    MethodSpec { method: "data.update", describe: "Update matching rows" },
    MethodSpec { method: "data.delete", describe: "Delete matching rows" },
    MethodSpec { method: "data.count", describe: "Count matching rows" },
    MethodSpec { method: "data.tables", describe: "List tables" },
    MethodSpec { method: "data.drop", describe: "Drop a table" },
];

impl NativeObject for DataObj {
    fn family(&self) -> &'static str {
        "data"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        let db = ctx.app_data.clone().ok_or("data is only available to installed apps")?.join("db");
        let arg = |name: &str| args.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("");
        let json_arg = |name: &str| Json::parse(arg(name)).ok();

        if method == "data.tables" {
            return Ok(Json::Arr(list_tables(&db).into_iter().map(Json::Str).collect()));
        }
        let table = arg("table");
        if table.trim().is_empty() {
            return Err(format!("{method} needs `table=`"));
        }
        let path = table_path(&db, table);

        match method {
            "data.insert" => {
                let mut row = json_arg("row").or_else(|| json_arg("value")).ok_or("data.insert needs `row=` (a JSON object)")?;
                if !matches!(row, Json::Obj(_)) {
                    return Err("data.insert: `row` must be a JSON object".into());
                }
                let now = now_secs();
                set_field(&mut row, "id", Json::Str(gen_id()));
                set_field(&mut row, "created", Json::Num(now));
                set_field(&mut row, "updated", Json::Num(now));
                let mut rows = load_table(&path);
                rows.push(row.clone());
                save_table(&path, &rows)?;
                Ok(row)
            }
            "data.query" => {
                let rows = load_table(&path);
                let filtered = filter(&rows, json_arg("where").as_ref());
                let mut out: Vec<Json> = filtered.into_iter().cloned().collect();
                if let Some(order) = (!arg("order").is_empty()).then(|| arg("order")) {
                    order_rows(&mut out, order);
                }
                let offset = arg("offset").parse::<usize>().unwrap_or(0);
                let limit = arg("limit").parse::<usize>().ok();
                let sliced = out.into_iter().skip(offset).take(limit.unwrap_or(usize::MAX)).collect();
                Ok(Json::Arr(sliced))
            }
            "data.get" => {
                let id = arg("id");
                Ok(load_table(&path).into_iter().find(|r| r.get("id").and_then(Json::as_str) == Some(id)).unwrap_or(Json::Null))
            }
            "data.count" => {
                let rows = load_table(&path);
                Ok(Json::Num(filter(&rows, json_arg("where").as_ref()).len() as f64))
            }
            "data.update" => {
                let set = json_arg("set").ok_or("data.update needs `set=` (a JSON object)")?;
                let Json::Obj(fields) = &set else { return Err("data.update: `set` must be a JSON object".into()) };
                let clause = json_arg("where");
                let mut rows = load_table(&path);
                let now = now_secs();
                let mut n = 0;
                for r in rows.iter_mut() {
                    if matches(r, clause.as_ref()) {
                        for (k, v) in fields {
                            set_field(r, k, v.clone());
                        }
                        set_field(r, "updated", Json::Num(now));
                        n += 1;
                    }
                }
                save_table(&path, &rows)?;
                Ok(Json::Num(n as f64))
            }
            "data.delete" => {
                let clause = json_arg("where");
                let mut rows = load_table(&path);
                let before = rows.len();
                rows.retain(|r| !matches(r, clause.as_ref()));
                let removed = before - rows.len();
                save_table(&path, &rows)?;
                Ok(Json::Num(removed as f64))
            }
            "data.drop" => {
                let _ = std::fs::remove_file(&path);
                Ok(Json::Bool(true))
            }
            _ => Err(format!("unknown data method '{method}'")),
        }
    }
}

// ===== the pure query engine (filter / order, over &[Json]) ================

/// Rows matching `clause` (a `where` JSON object). `None` matches all.
fn filter<'a>(rows: &'a [Json], clause: Option<&Json>) -> Vec<&'a Json> {
    rows.iter().filter(|r| matches(r, clause)).collect()
}

/// Does `row` satisfy `clause`? A clause is `{field: value}` (equality) or
/// `{field: {op: value}}` with `op` ∈ eq/ne/gt/lt/gte/lte/in/contains. ANDed.
fn matches(row: &Json, clause: Option<&Json>) -> bool {
    let Some(Json::Obj(conds)) = clause else { return true };
    conds.iter().all(|(field, cond)| match_cond(row.get(field).unwrap_or(&Json::Null), cond))
}

fn match_cond(val: &Json, cond: &Json) -> bool {
    match cond {
        Json::Obj(ops) => ops.iter().all(|(op, target)| apply_op(val, op, target)),
        other => json_eq(val, other),
    }
}

fn apply_op(val: &Json, op: &str, target: &Json) -> bool {
    use std::cmp::Ordering::*;
    let ord = cmp_json(val, target);
    match op {
        "eq" => json_eq(val, target),
        "ne" => !json_eq(val, target),
        "gt" => ord == Greater,
        "gte" => ord != Less,
        "lt" => ord == Less,
        "lte" => ord != Greater,
        "in" => target.as_array().map(|a| a.iter().any(|t| json_eq(val, t))).unwrap_or(false),
        "contains" => match (val, target) {
            (Json::Str(s), Json::Str(t)) => s.to_lowercase().contains(&t.to_lowercase()),
            (Json::Arr(a), t) => a.iter().any(|e| json_eq(e, t)),
            _ => false,
        },
        _ => false,
    }
}

fn json_eq(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Str(x), Json::Str(y)) => x == y,
        (Json::Num(x), Json::Num(y)) => x == y,
        (Json::Bool(x), Json::Bool(y)) => x == y,
        (Json::Null, Json::Null) => true,
        // a JSON-string arg `"5"`/`"true"` compared to a typed cell
        (Json::Str(s), Json::Num(n)) | (Json::Num(n), Json::Str(s)) => s.parse::<f64>().map(|v| v == *n).unwrap_or(false),
        (Json::Str(s), Json::Bool(b2)) | (Json::Bool(b2), Json::Str(s)) => s.parse::<bool>().map(|v| v == *b2).unwrap_or(false),
        _ => false,
    }
}

fn cmp_json(a: &Json, b: &Json) -> std::cmp::Ordering {
    use std::cmp::Ordering::Equal;
    let num = |j: &Json| match j {
        Json::Num(n) => Some(*n),
        Json::Str(s) => s.parse::<f64>().ok(),
        _ => None,
    };
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Equal),
        _ => a.as_str().unwrap_or("").cmp(b.as_str().unwrap_or("")),
    }
}

/// Sort `rows` by `order` (`field` ascending, `-field` descending).
fn order_rows(rows: &mut [Json], order: &str) {
    let (field, desc) = order.strip_prefix('-').map(|f| (f, true)).unwrap_or((order, false));
    rows.sort_by(|a, b| {
        let c = cmp_json(a.get(field).unwrap_or(&Json::Null), b.get(field).unwrap_or(&Json::Null));
        if desc { c.reverse() } else { c }
    });
}

// ===== persistence (one JSON array per table) ==============================

fn table_path(db: &Path, table: &str) -> PathBuf {
    let safe: String = table.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    db.join(format!("{safe}.json"))
}

fn load_table(path: &Path) -> Vec<Json> {
    std::fs::read_to_string(path).ok().and_then(|s| Json::parse(&s).ok()).and_then(|j| j.as_array().map(<[_]>::to_vec)).unwrap_or_default()
}

fn save_table(path: &Path, rows: &[Json]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, Json::Arr(rows.to_vec()).to_string()).map_err(|e| e.to_string())
}

fn list_tables(db: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(db) else { return Vec::new() };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| e.path().file_stem().and_then(|s| s.to_str()).map(str::to_string))
        .collect();
    out.sort();
    out
}

fn set_field(obj: &mut Json, key: &str, value: Json) {
    if let Json::Obj(fields) = obj {
        if let Some(slot) = fields.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else {
            fields.push((key.to_string(), value));
        }
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as f64).unwrap_or(0.0)
}

/// A short, unique row id (12 hex chars from the OS CSPRNG).
fn gen_id() -> String {
    let mut b = [0u8; 6];
    let _ = platform::os::random_bytes(&mut b);
    corelib::codec::hex_encode(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Json> {
        let mk = |id: &str, n: f64, done: bool| {
            Json::Obj(vec![
                ("id".into(), Json::Str(id.into())),
                ("n".into(), Json::Num(n)),
                ("done".into(), Json::Bool(done)),
                ("text".into(), Json::Str(format!("item {id}"))),
            ])
        };
        vec![mk("a", 3.0, false), mk("b", 1.0, true), mk("c", 2.0, false)]
    }

    #[test]
    fn where_equality_and_ops() {
        let r = rows();
        // equality
        assert_eq!(filter(&r, Some(&Json::parse("{\"done\":false}").unwrap())).len(), 2);
        // operator object: n >= 2
        assert_eq!(filter(&r, Some(&Json::parse("{\"n\":{\"gte\":2}}").unwrap())).len(), 2);
        // contains (case-insensitive)
        assert_eq!(filter(&r, Some(&Json::parse("{\"text\":{\"contains\":\"ITEM A\"}}").unwrap())).len(), 1);
        // in
        assert_eq!(filter(&r, Some(&Json::parse("{\"id\":{\"in\":[\"a\",\"c\"]}}").unwrap())).len(), 2);
        // no clause → all
        assert_eq!(filter(&r, None).len(), 3);
    }

    #[test]
    fn order_ascending_and_descending() {
        let mut r = rows();
        order_rows(&mut r, "n");
        assert_eq!(r.iter().map(|x| x.get("id").and_then(Json::as_str).unwrap()).collect::<Vec<_>>(), vec!["b", "c", "a"]);
        order_rows(&mut r, "-n");
        assert_eq!(r.iter().map(|x| x.get("id").and_then(Json::as_str).unwrap()).collect::<Vec<_>>(), vec!["a", "c", "b"]);
    }

    fn svc() -> (DataObj, CapCtx, PathBuf) {
        let dir = std::env::temp_dir().join(format!("tt-data-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ctx = CapCtx {
            policy: std::sync::Arc::new(crate::security::Policy::new()),
            app_data: Some(dir.clone()),
            remote_enabled: true,
            origin: "terminal://ai/".into(),
            sandbox: None,
        };
        (DataObj, ctx, dir)
    }

    #[test]
    fn insert_query_update_delete_round_trip() {
        let (o, ctx, dir) = svc();
        let run = |m: &str, a: &[(&str, &str)]| {
            let args: Vec<(String, String)> = a.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            o.invoke(m, &args, &ctx, &mut crate::caps::host::NullHost)
        };
        run("data.insert", &[("table", "todos"), ("row", "{\"text\":\"ship\",\"done\":false}")]).unwrap();
        run("data.insert", &[("table", "todos"), ("row", "{\"text\":\"sleep\",\"done\":true}")]).unwrap();
        // query open todos
        let open = run("data.query", &[("table", "todos"), ("where", "{\"done\":false}")]).unwrap();
        assert_eq!(open.as_array().unwrap().len(), 1);
        assert_eq!(open.as_array().unwrap()[0].get("text").and_then(Json::as_str), Some("ship"));
        // count + tables
        assert_eq!(run("data.count", &[("table", "todos")]).unwrap().as_f64(), Some(2.0));
        assert_eq!(run("data.tables", &[]).unwrap().as_array().unwrap().len(), 1);
        // update all done=true
        let n = run("data.update", &[("table", "todos"), ("set", "{\"done\":true}")]).unwrap();
        assert_eq!(n.as_f64(), Some(2.0));
        assert_eq!(run("data.query", &[("table", "todos"), ("where", "{\"done\":false}")]).unwrap().as_array().unwrap().len(), 0);
        // delete done
        let d = run("data.delete", &[("table", "todos"), ("where", "{\"done\":true}")]).unwrap();
        assert_eq!(d.as_f64(), Some(2.0));
        assert_eq!(run("data.count", &[("table", "todos")]).unwrap().as_f64(), Some(0.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn insert_stamps_id_created_updated() {
        let (o, ctx, dir) = svc();
        let args: Vec<(String, String)> = [("table", "t"), ("row", "{\"x\":1}")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let row = o.invoke("data.insert", &args, &ctx, &mut crate::caps::host::NullHost).unwrap();
        assert!(row.get("id").and_then(Json::as_str).is_some());
        assert!(row.get("created").and_then(Json::as_f64).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_requires_app_sandbox() {
        let ctx = CapCtx {
            policy: std::sync::Arc::new(crate::security::Policy::new()),
            app_data: None,
            remote_enabled: true,
            origin: String::new(),
            sandbox: None,
        };
        assert!(DataObj.invoke("data.tables", &[], &ctx, &mut crate::caps::host::NullHost).is_err());
    }
}
