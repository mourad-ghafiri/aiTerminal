//! The JSON-RPC wire, pure: build outgoing messages, classify incoming ones.
//!
//! Nothing here touches a process or a socket — a message is a `String` in and a
//! [`Incoming`] out, which is what lets every protocol rule below be a unit test.
//!
//! Two protocol **eras** share this wire (spec: *Versioning and Compatibility*,
//! revision 2026-07-28). A *modern* request carries its protocol version, client
//! capabilities and identity in `_meta` on every call; a *legacy* request relies on
//! the `initialize` handshake having said it all once. The builders here stamp or
//! omit `_meta` accordingly — the one place the difference exists on the wire.

use corelib::wire::Json;

/// The modern revision this client speaks.
pub(crate) const MODERN: &str = "2026-07-28";
/// The legacy revision proposed in an `initialize` fallback.
pub(crate) const LEGACY_PREFERRED: &str = "2025-11-25";
/// Every legacy revision this client accepts. The tools surface — `initialize`,
/// `notifications/initialized`, paginated `tools/list`, `tools/call`, `ping` — is
/// identical across all of them, which is what "accepts" means here.
pub(crate) const LEGACY_KNOWN: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// `UnsupportedProtocolVersionError` — the one error code that PROVES the peer is a
/// modern server (spec: stdio backward compatibility). Everything else proves nothing.
pub(crate) const UNSUPPORTED_VERSION: i64 = -32022;

/// Which era a connection settled into, with the negotiated revision.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Era {
    Modern(String),
    Legacy(String),
}

impl Era {
    pub(crate) fn version(&self) -> &str {
        match self {
            Era::Modern(v) | Era::Legacy(v) => v,
        }
    }
    pub(crate) fn word(&self) -> &'static str {
        match self {
            Era::Modern(_) => "modern",
            Era::Legacy(_) => "legacy",
        }
    }
}

/// The `_meta` object a modern request must carry: protocol version and client
/// capabilities are required on EVERY request; identity is a SHOULD.
/// We declare no capabilities — no roots, no sampling, no elicitation — so a server
/// that needs one refuses with `-32021` instead of hanging on input we cannot give.
fn meta(version: &str) -> Json {
    Json::Obj(vec![
        ("io.modelcontextprotocol/protocolVersion".into(), Json::Str(version.into())),
        ("io.modelcontextprotocol/clientCapabilities".into(), Json::Obj(vec![])),
        (
            "io.modelcontextprotocol/clientInfo".into(),
            Json::Obj(vec![
                ("name".into(), Json::Str(corelib::brand::NAME.into())),
                ("version".into(), Json::Str(env!("CARGO_PKG_VERSION").into())),
            ]),
        ),
    ])
}

/// One request line. `era` decides the stamp: modern gets `_meta`, legacy gets none.
pub(crate) fn request(id: u64, method: &str, params: Json, era: &Era) -> String {
    let params = match era {
        Era::Modern(v) => {
            let Json::Obj(mut pairs) = params else { unreachable!("params is always an object") };
            pairs.push(("_meta".into(), meta(v)));
            Json::Obj(pairs)
        }
        Era::Legacy(_) => params,
    };
    Json::Obj(vec![
        ("jsonrpc".into(), Json::Str("2.0".into())),
        ("id".into(), Json::Num(id as f64)),
        ("method".into(), Json::Str(method.into())),
        ("params".into(), params),
    ])
    .to_string()
}

/// One notification line (no id, never answered).
pub(crate) fn notification(method: &str, params: Json) -> String {
    Json::Obj(vec![
        ("jsonrpc".into(), Json::Str("2.0".into())),
        ("method".into(), Json::Str(method.into())),
        ("params".into(), params),
    ])
    .to_string()
}

/// A success response to a SERVER request (its `id` echoed verbatim — the spec allows
/// string ids, so it is carried as `Json`, never coerced through a number).
pub(crate) fn respond_ok(id: &Json, result: Json) -> String {
    Json::Obj(vec![("jsonrpc".into(), Json::Str("2.0".into())), ("id".into(), id.clone()), ("result".into(), result)]).to_string()
}

/// An error response to a server request this client does not serve.
pub(crate) fn respond_err(id: &Json, code: i64, message: &str) -> String {
    Json::Obj(vec![
        ("jsonrpc".into(), Json::Str("2.0".into())),
        ("id".into(), id.clone()),
        (
            "error".into(),
            Json::Obj(vec![("code".into(), Json::Num(code as f64)), ("message".into(), Json::Str(message.into()))]),
        ),
    ])
    .to_string()
}

/// A JSON-RPC error as received, with the fields negotiation needs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RpcError {
    pub code: i64,
    pub message: String,
    /// `data.supported` from an `UnsupportedProtocolVersionError` — the server's own
    /// list of what it speaks, which is what the retry picks from.
    pub supported: Vec<String>,
}

/// One incoming line, classified. The old client matched only "my id or not", which
/// dropped server *requests* on the floor — a legacy server's `ping` went unanswered
/// and the server, still waiting, eventually gave up on us.
#[derive(Debug, PartialEq)]
pub(crate) enum Incoming {
    /// A response to one of OUR requests.
    Reply { id: u64, result: Result<Json, RpcError> },
    /// A request FROM the server (has both `method` and `id`) — must be answered.
    ServerRequest { id: Json, method: String },
    /// A one-way message from the server.
    Notification { method: String, params: Json },
    /// Not JSON-RPC; ignored (a hostile or broken line must not wedge the loop).
    Noise,
}

pub(crate) fn classify(line: &str) -> Incoming {
    let Ok(v) = Json::parse(line) else { return Incoming::Noise };
    let method = v.get("method").and_then(Json::as_str).map(str::to_string);
    let id = v.get("id").cloned();
    match (method, id) {
        (Some(method), Some(id)) if !matches!(id, Json::Null) => Incoming::ServerRequest { id, method },
        (Some(method), _) => Incoming::Notification { method, params: v.get("params").cloned().unwrap_or(Json::Null) },
        (None, Some(Json::Num(n))) => {
            let id = n as u64;
            match v.get("error") {
                Some(err) => {
                    let code = err.get("code").and_then(Json::as_f64).unwrap_or(0.0) as i64;
                    let message = err.get("message").and_then(Json::as_str).unwrap_or("error").to_string();
                    let supported = err
                        .get("data")
                        .and_then(|d| d.get("supported"))
                        .and_then(Json::as_array)
                        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
                        .unwrap_or_default();
                    Incoming::Reply { id, result: Err(RpcError { code, message, supported }) }
                }
                None => Incoming::Reply { id, result: Ok(v.get("result").cloned().unwrap_or(Json::Null)) },
            }
        }
        _ => Incoming::Noise,
    }
}

/// Apply the `resultType` rule to a settled result (spec: *ResultType*).
///
/// Absent means `"complete"` — that is how every legacy result arrives. A modern
/// `"input_required"` asks for interactive input (elicitation, sampling) that a
/// headless run cannot give; it becomes a tool-level error the model can read and
/// route around. Anything else is a value this client does not recognise, and the
/// spec's word for that is invalid — not "probably fine".
pub(crate) fn settle(result: Json) -> Result<Json, String> {
    match result.get("resultType").and_then(Json::as_str) {
        None | Some("complete") => Ok(result),
        Some("input_required") => Err("the tool needs interactive input (elicitation) this run cannot provide".into()),
        Some(other) => Err(format!("unrecognised resultType {other:?}")),
    }
}
