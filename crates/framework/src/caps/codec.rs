//! The `codec.*` native family — pure encoders/decoders, hashing, JSON/CSV, and
//! id/randomness the SDK exposes for apps + AI. Stateless and free (no permission):
//! it never touches the host, the filesystem, or the network — just transforms text.
//! The algorithms live in `corelib::codec` (reusable, layer-correct); this is the thin
//! capability wrapper.

use corelib::wire::Json;

use super::object::{MethodSpec, NativeObject};
use super::CapCtx;

pub struct CodecObj;

const SPECS: &[MethodSpec] = &[
    MethodSpec { method: "codec.base64_encode", describe: "Base64-encode text" },
    MethodSpec { method: "codec.base64_decode", describe: "Base64-decode text" },
    MethodSpec { method: "codec.hex_encode", describe: "Hex-encode text" },
    MethodSpec { method: "codec.hex_decode", describe: "Hex-decode text" },
    MethodSpec { method: "codec.url_encode", describe: "URL percent-encode" },
    MethodSpec { method: "codec.url_decode", describe: "URL percent-decode" },
    MethodSpec { method: "codec.json_parse", describe: "Parse JSON text" },
    MethodSpec { method: "codec.json_stringify", describe: "Serialize a value to JSON" },
    MethodSpec { method: "codec.csv_parse", describe: "Parse CSV into rows" },
    MethodSpec { method: "codec.csv_format", describe: "Format rows as CSV" },
    MethodSpec { method: "codec.sha256", describe: "SHA-256 hex digest" },
    MethodSpec { method: "codec.hash", describe: "Fast (FNV) hash" },
    MethodSpec { method: "codec.uuid", describe: "Generate a UUID v4" },
    MethodSpec { method: "codec.random", describe: "Random hex bytes" },
];

impl NativeObject for CodecObj {
    fn family(&self) -> &'static str {
        "codec"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    fn invoke(&self, method: &str, args: &[(String, String)], _ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        let text = || arg(args, "text").or_else(|| arg(args, "value")).or_else(|| arg(args, "data")).unwrap_or("").to_string();
        match method {
            "codec.base64_encode" => Ok(Json::Str(corelib::codec::base64_encode(text().as_bytes()))),
            "codec.base64_decode" => corelib::codec::base64_decode(&text()).map(|b| Json::Str(String::from_utf8_lossy(&b).into_owned())),
            "codec.hex_encode" => Ok(Json::Str(corelib::codec::hex_encode(text().as_bytes()))),
            "codec.hex_decode" => corelib::codec::hex_decode(&text()).map(|b| Json::Str(String::from_utf8_lossy(&b).into_owned())),
            "codec.url_encode" => Ok(Json::Str(corelib::codec::url_encode(&text()))),
            "codec.url_decode" => corelib::codec::url_decode(&text()).map(Json::Str),
            "codec.json_parse" => Json::parse(&text()).map_err(|e| format!("json: {e}")),
            "codec.json_stringify" => {
                // The value arrives as a JSON string; normalize (or wrap a bare string).
                let v = Json::parse(&text()).unwrap_or_else(|_| Json::Str(text()));
                Ok(Json::Str(v.to_string()))
            }
            "codec.csv_parse" => Ok(Json::Arr(
                corelib::codec::csv_parse(&text())
                    .into_iter()
                    .map(|r| Json::Arr(r.into_iter().map(Json::Str).collect()))
                    .collect(),
            )),
            "codec.csv_format" => {
                let rows = Json::parse(&text())
                    .ok()
                    .and_then(|j| j.as_array().map(<[_]>::to_vec))
                    .map(|outer| {
                        outer
                            .into_iter()
                            .map(|row| row.as_array().map(|a| a.iter().map(cell_text).collect()).unwrap_or_else(|| vec![cell_text(&row)]))
                            .collect::<Vec<Vec<String>>>()
                    })
                    .ok_or("codec.csv_format needs a JSON array of arrays")?;
                Ok(Json::Str(corelib::codec::csv_format(&rows)))
            }
            "codec.sha256" => Ok(Json::Str(corelib::codec::sha256_hex(text().as_bytes()))),
            "codec.hash" => Ok(Json::Str(fnv1a(&text()))),
            "codec.uuid" => Ok(Json::Str(uuid_v4())),
            "codec.random" => {
                let n = arg(args, "bytes").and_then(|s| s.parse::<usize>().ok()).unwrap_or(16).clamp(1, 256);
                let mut b = vec![0u8; n];
                let _ = platform::os::random_bytes(&mut b);
                Ok(Json::Str(corelib::codec::hex_encode(&b)))
            }
            _ => Err(format!("unknown codec method '{method}'")),
        }
    }
}

fn arg<'a>(args: &'a [(String, String)], name: &str) -> Option<&'a str> {
    args.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// A JSON value as plain cell text (string verbatim, numbers/bools rendered).
fn cell_text(v: &Json) -> String {
    match v {
        Json::Str(s) => s.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}

/// 64-bit FNV-1a, hex — the fast non-crypto hash (cache keys, dedup).
fn fnv1a(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// A random UUID v4 from the OS CSPRNG.
fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    let _ = platform::os::random_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10
    let h = corelib::codec::hex_encode(&b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

use super::host::Host;

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
}
