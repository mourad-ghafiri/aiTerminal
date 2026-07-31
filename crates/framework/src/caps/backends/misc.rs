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


pub(crate) fn sec(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    match method {
        "sec.check_command" => {
            let cmd = arg(args, 0, "cmd").ok_or("sec.check_command: missing cmd")?;
            let (verdict, reason) = match ctx.policy.check_command(cmd) {
                crate::security::Verdict::Allow => ("allow", String::new()),
                crate::security::Verdict::Confirm { reason } => ("confirm", reason),
                crate::security::Verdict::Deny { reason } => ("deny", reason),
            };
            Ok(obj(&[("verdict", Json::Str(verdict.into())), ("reason", Json::Str(reason))]))
        }
        "sec.redact" => {
            let text = arg(args, 0, "text").unwrap_or("");
            let scope = crate::security::RedactScope::parse(arg(args, 1, "scope").unwrap_or("all"));
            Ok(Json::Str(ctx.policy.redact(text, scope)))
        }
        _ => Err(format!("unknown sec method '{method}'")),
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
