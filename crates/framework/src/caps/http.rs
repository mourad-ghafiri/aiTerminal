//! The `http.*` native family — a general HTTP(S) client for apps + AI (the existing
//! `net.get` is GET-only; `web.read` is GET→markdown). Full requests: any method,
//! headers, and a text/JSON body, returning `{status, headers, body, json?}`. HTTPS-only
//! and **SSRF-guarded** (resolve-and-pin to a vetted IP, no redirects) like `net.get`;
//! gated by the `network` permission.

use corelib::wire::Json;

use super::host::Host;
use super::object::{MethodSpec, NativeObject};
use super::CapCtx;

pub struct HttpObj;

const SPECS: &[MethodSpec] = &[
    MethodSpec { method: "http.request", describe: "Make an HTTP request" },
    MethodSpec { method: "http.get", describe: "HTTP GET" },
    MethodSpec { method: "http.post", describe: "HTTP POST" },
];

impl NativeObject for HttpObj {
    fn family(&self) -> &'static str {
        "http"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        if !ctx.remote_enabled {
            return Err("network is disabled ([ai] network = false)".into());
        }
        let arg = |name: &str| args.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("");
        let verb: String = match method {
            "http.get" => "GET".into(),
            "http.post" => "POST".into(),
            _ => {
                let m = arg("method").to_uppercase();
                if m.is_empty() {
                    "GET".into()
                } else {
                    m
                }
            }
        };
        let url = arg("url");
        if !url.starts_with("https://") {
            return Err("http: only https:// URLs are allowed".into());
        }

        // Headers from a `headers={...}` JSON object; a `json=` body adds Content-Type.
        let mut headers: Vec<(String, String)> = Json::parse(arg("headers"))
            .ok()
            .and_then(|j| match j {
                Json::Obj(f) => Some(f.into_iter().map(|(k, v)| (k, header_val(&v))).collect()),
                _ => None,
            })
            .unwrap_or_default();
        let body: Option<String> = if !arg("json").is_empty() {
            if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                headers.push(("Content-Type".into(), "application/json".into()));
            }
            Some(arg("json").to_string())
        } else if !arg("body").is_empty() {
            Some(arg("body").to_string())
        } else {
            None
        };

        let pin = super::backends::ssrf_pin(url)?;
        let (status, resp_headers, resp_body) = super::net::https_request(&verb, url, &headers, body.as_deref(), &pin)?;

        let json = Json::parse(&resp_body).ok();
        let mut out = vec![
            ("status".into(), Json::Num(status as f64)),
            ("ok".into(), Json::Bool((200..300).contains(&status))),
            ("body".into(), Json::Str(resp_body)),
            ("headers".into(), Json::Obj(resp_headers.into_iter().map(|(k, v)| (k, Json::Str(v))).collect())),
        ];
        if let Some(j) = json {
            out.push(("json".into(), j));
        }
        Ok(Json::Obj(out))
    }
}

/// A header value as a plain string (numbers/bools rendered).
fn header_val(v: &Json) -> String {
    match v {
        Json::Str(s) => s.clone(),
        other => other.to_string(),
    }
}
