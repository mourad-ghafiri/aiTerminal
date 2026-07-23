//! The `time.*` native family — rich date/time the SDK exposes for apps + AI: format,
//! parse, arithmetic, components, and human "relative" strings. Pure + free (no host,
//! no I/O beyond reading the OS local-offset once). Civil-date math lives in
//! `corelib::datetime`; the local UTC offset comes from `platform::os::utc_offset_secs`.

use corelib::wire::Json;

use super::host::Host;
use super::object::{MethodSpec, NativeObject};
use super::CapCtx;

pub struct TimeObj;

const SPECS: &[MethodSpec] = &[
    MethodSpec { method: "time.now", describe: "Current time (unix or formatted)" },
    MethodSpec { method: "time.format", describe: "Format a unix time" },
    MethodSpec { method: "time.parse", describe: "Parse a date string to unix" },
    MethodSpec { method: "time.add", describe: "Add a duration to a unix time" },
    MethodSpec { method: "time.diff", describe: "Seconds between two times" },
    MethodSpec { method: "time.relative", describe: "Human 'time ago' string" },
    MethodSpec { method: "time.components", describe: "Break a time into parts" },
];

impl NativeObject for TimeObj {
    fn family(&self) -> &'static str {
        "time"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    fn invoke(&self, method: &str, args: &[(String, String)], _ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        let off = platform::os::utc_offset_secs();
        let num = |name: &str| args.iter().find(|(k, _)| k == name).and_then(|(_, v)| v.parse::<i64>().ok());
        let s = |name: &str| args.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("");
        match method {
            "time.now" => {
                let now = now_unix();
                let fmt = s("format");
                if fmt.is_empty() {
                    Ok(Json::Num(now as f64))
                } else {
                    Ok(Json::Str(corelib::datetime::format(now, fmt, off)))
                }
            }
            "time.format" => {
                let unix = num("unix").unwrap_or_else(now_unix);
                let fmt = s("format");
                let fmt = if fmt.is_empty() { "%Y-%m-%d %H:%M:%S" } else { fmt };
                Ok(Json::Str(corelib::datetime::format(unix, fmt, off)))
            }
            "time.parse" => {
                let fmt = s("format");
                let fmt = (!fmt.is_empty()).then_some(fmt);
                corelib::datetime::parse(s("text"), fmt, off)
                    .map(|u| Json::Num(u as f64))
                    .ok_or_else(|| format!("time.parse: could not parse '{}'", s("text")))
            }
            "time.add" => {
                let base = num("unix").unwrap_or_else(now_unix);
                let delta = num("secs").unwrap_or(0)
                    + num("mins").unwrap_or(0) * 60
                    + num("hours").unwrap_or(0) * 3600
                    + num("days").unwrap_or(0) * 86_400;
                Ok(Json::Num((base + delta) as f64))
            }
            "time.diff" => {
                let a = num("a").or_else(|| num("from")).unwrap_or(0);
                let b = num("b").or_else(|| num("to")).unwrap_or_else(now_unix);
                Ok(Json::Num((b - a) as f64))
            }
            "time.relative" => {
                let unix = num("unix").unwrap_or_else(now_unix);
                Ok(Json::Str(corelib::datetime::relative(unix, now_unix())))
            }
            "time.components" => {
                let unix = num("unix").unwrap_or_else(now_unix);
                let dt = corelib::datetime::from_unix(unix, off);
                Ok(Json::Obj(vec![
                    ("year".into(), Json::Num(dt.year as f64)),
                    ("month".into(), Json::Num(dt.month as f64)),
                    ("day".into(), Json::Num(dt.day as f64)),
                    ("hour".into(), Json::Num(dt.hour as f64)),
                    ("minute".into(), Json::Num(dt.minute as f64)),
                    ("second".into(), Json::Num(dt.second as f64)),
                    ("weekday".into(), Json::Num(dt.weekday as f64)),
                ]))
            }
            _ => Err(format!("unknown time method '{method}'")),
        }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(method: &str, args: &[(&str, &str)]) -> Result<Json, String> {
        let ctx = CapCtx {
            policy: std::sync::Arc::new(crate::security::Policy::new()),
            app_data: None,
            remote_enabled: true,
            origin: String::new(),
            sandbox: None,
        };
        let a: Vec<(String, String)> = args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        TimeObj.invoke(method, &a, &ctx, &mut crate::caps::host::NullHost)
    }

    #[test]
    fn format_parse_add_diff_round_trip() {
        // parse a fixed instant, format it back (offset cancels: parse+format use the same)
        let unix = run("time.parse", &[("text", "2026-06-22 14:05:09")]).unwrap().as_f64().unwrap() as i64;
        let s = run("time.format", &[("unix", &unix.to_string()), ("format", "%Y-%m-%d %H:%M:%S")]).unwrap();
        assert_eq!(s.as_str(), Some("2026-06-22 14:05:09"));
        // add 1 day
        let plus = run("time.add", &[("unix", &unix.to_string()), ("days", "1")]).unwrap().as_f64().unwrap() as i64;
        assert_eq!(plus - unix, 86_400);
        // diff
        let d = run("time.diff", &[("a", &unix.to_string()), ("b", &plus.to_string())]).unwrap();
        assert_eq!(d.as_f64(), Some(86_400.0));
    }

    #[test]
    fn relative_and_components() {
        let now = now_unix();
        let r = run("time.relative", &[("unix", &(now - 7200).to_string())]).unwrap();
        assert_eq!(r.as_str(), Some("2 hours ago"));
        // unix 0 is 1970-01-01 UTC; the exact local fields depend on the machine offset,
        // so just assert they are well-formed (offset-robust).
        let c = run("time.components", &[("unix", "0")]).unwrap();
        let wd = c.get("weekday").and_then(Json::as_f64).unwrap();
        assert!((0.0..=6.0).contains(&wd));
        assert!((1969.0..=1970.0).contains(&c.get("year").and_then(Json::as_f64).unwrap()));
    }
}
