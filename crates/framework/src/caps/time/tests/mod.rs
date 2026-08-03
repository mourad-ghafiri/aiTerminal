use super::*;

fn run(method: &str, args: &[(&str, &str)]) -> Result<Json, String> {
    let ctx = CapCtx {
        guard: std::sync::Arc::new(crate::guard::Guard::default()),
        app_data: None,
        remote_enabled: true,
        origin: String::new(),
        sandbox: None,
        memory_dir: None,
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
