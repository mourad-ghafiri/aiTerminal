use super::*;

fn run(method: &str, args: &[(&str, &str)]) -> Result<Json, String> {
    let ctx = CapCtx {
        policy: std::sync::Arc::new(crate::security::Policy::new()),
        app_data: None,
        remote_enabled: true,
        origin: String::new(),
        sandbox: None,
        memory_dir: None,
    };
    let a: Vec<(String, String)> = args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    CodecObj.invoke(method, &a, &ctx, &mut crate::caps::host::NullHost)
}

#[test]
fn base64_hex_url_round_trip() {
    assert_eq!(run("codec.base64_encode", &[("text", "foobar")]).unwrap().as_str(), Some("Zm9vYmFy"));
    assert_eq!(run("codec.base64_decode", &[("text", "Zm9vYmFy")]).unwrap().as_str(), Some("foobar"));
    assert_eq!(run("codec.url_encode", &[("text", "a b")]).unwrap().as_str(), Some("a%20b"));
}

#[test]
fn sha256_and_json_and_csv() {
    assert_eq!(
        run("codec.sha256", &[("text", "abc")]).unwrap().as_str(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    let parsed = run("codec.json_parse", &[("text", "{\"a\":1}")]).unwrap();
    assert_eq!(parsed.get("a").and_then(Json::as_f64), Some(1.0));
    let rows = run("codec.csv_parse", &[("text", "a,b\n1,2")]).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);
}

#[test]
fn uuid_is_v4_shaped() {
    let id = run("codec.uuid", &[]).unwrap();
    let s = id.as_str().unwrap();
    assert_eq!(s.len(), 36);
    assert_eq!(&s[14..15], "4", "version nibble is 4");
}
