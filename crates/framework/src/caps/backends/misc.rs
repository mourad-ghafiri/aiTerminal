use std::time::{SystemTime, UNIX_EPOCH};

use corelib::wire::Json;

use crate::caps::*;

// ----- os / sec / clock ----------------------------------------------------

pub(crate) fn os(method: &str, args: &[(String, String)]) -> Result<Json, String> {
    match method {
        "os.open" => {
            let url = arg(args, 0, "url").ok_or("os.open: missing url")?;
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err("os.open only opens http(s) URLs".into());
            }
            platform::os::open_external(url)?;
            Ok(Json::Str(format!("opened {url}")))
        }
        _ => Err(format!("unknown os method '{method}'")),
    }
}


/// The `guard` family — an agent asking the guard a question instead of finding out by
/// being refused. `act` is the same word the guard's own vocabulary uses, so what an agent
/// can ask about is exactly what the guard can decide.
pub(crate) fn guard(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    match method {
        "guard.check" => {
            let target = arg(args, 1, "target").or_else(|| arg(args, 0, "cmd")).ok_or("guard.check: missing target")?;
            let path = std::path::Path::new(target);
            let act = match arg(args, 0, "act").unwrap_or("run").trim().to_ascii_lowercase().as_str() {
                "read" => crate::guard::Act::Read(path),
                "write" => crate::guard::Act::Write(path),
                _ => crate::guard::Act::Run(target),
            };
            let (verdict, reason) = match ctx.guard.judge(act) {
                crate::guard::Decision::Allow => ("allow", String::new()),
                crate::guard::Decision::Confirm { reason } => ("confirm", reason),
                crate::guard::Decision::Deny { reason } => ("deny", reason),
            };
            Ok(obj(&[("verdict", Json::Str(verdict.into())), ("reason", Json::Str(reason))]))
        }
        // Masking, not hiding: an agent scrubbing something it is about to write into a
        // report wants the secret gone for good, not a placeholder that would put it back.
        "guard.mask" => Ok(Json::Str(ctx.guard.mask(arg(args, 0, "text").unwrap_or("")))),
        _ => Err(format!("unknown guard method '{method}'")),
    }
}

pub(crate) fn clock(method: &str) -> Result<Json, String> {
    match method {
        "clock.now" => {
            let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            Ok(obj(&[("unix", Json::Num(secs as f64))]))
        }
        _ => Err(format!("unknown clock method '{method}'")),
    }
}

// ----- store (per-app sandbox) ---------------------------------------------

pub(crate) fn store(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let dir = ctx.app_data.clone().ok_or("store is only available to installed apps")?;
    let key = arg(args, 0, "key").ok_or("store: missing key")?;
    if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || key.is_empty() {
        return Err("store: key must be [a-z0-9-_]".into());
    }
    let path = dir.join(format!("{key}.json"));
    match method {
        "store.get" => Ok(std::fs::read_to_string(&path).ok().and_then(|s| Json::parse(&s).ok()).unwrap_or(Json::Null)),
        "store.set" => {
            let value = arg(args, 1, "value").unwrap_or("");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            // store the raw value (parsed if JSON, else a string)
            let json = Json::parse(value).unwrap_or_else(|_| Json::Str(value.to_string()));
            std::fs::write(&path, json.to_string()).map_err(|e| e.to_string())?;
            Ok(Json::Bool(true))
        }
        "store.delete" => {
            let _ = std::fs::remove_file(&path);
            Ok(Json::Bool(true))
        }
        "store.list" => {
            let mut keys = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    if let Some(stem) = e.path().file_stem().and_then(|s| s.to_str()) {
                        keys.push(Json::Str(stem.to_string()));
                    }
                }
            }
            Ok(Json::Arr(keys))
        }
        _ => Err(format!("unknown store method '{method}'")),
    }
}
