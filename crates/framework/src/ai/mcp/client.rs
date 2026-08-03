//! One connected server: the receive loop, the negotiation, the calls.
//!
//! The loop is the part the old client got wrong. A single shared channel carries
//! four kinds of traffic — replies to us, *requests from the server*, notifications,
//! and noise — and "skip anything that isn't my id" answers only the first. A legacy
//! server's `ping` went unanswered until the server concluded we were dead. Every
//! incoming line is now classified ([`wire::classify`]) and every kind is handled.

use std::time::Duration;

use corelib::wire::Json;

use super::era::{judge_discover, judge_initialize, Probe};
use super::wire::{self, Era, RpcError, LEGACY_PREFERRED, MODERN};
use super::{McpTool, McpTransport};

/// How long the era probe waits before declaring the server legacy. Short on
/// purpose: a modern server answers `server/discover` immediately, and a silent
/// legacy server should not cost every launch ten seconds.
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);
/// How long the legacy handshake and the tool listing may take.
pub(crate) const INIT_TIMEOUT: Duration = Duration::from_secs(10);
/// A hostile `nextCursor` that never ends must not spin forever.
const MAX_PAGES: usize = 16;
/// How many server log notifications are kept for diagnostics.
const NOTES_KEEP: usize = 20;

/// What one round-trip produced: a reply, silence, or a dead transport.
enum Round {
    Reply(Result<Json, RpcError>),
    TimedOut(u64),
}

pub struct McpClient<T: McpTransport> {
    pub(crate) transport: T,
    next_id: u64,
    /// Per-call timeout, from the declaration.
    timeout: Duration,
    pub(crate) era: Era,
    pub tools: Vec<McpTool>,
    /// Whether the server declared the `resources` capability.
    pub(crate) resources: bool,
    /// `name vX` from the server's self-identification, for display only —
    /// the spec is explicit that it must never drive behaviour.
    pub(crate) server_info: String,
    /// Set by `notifications/tools/list_changed`: the catalogue is stale for the
    /// NEXT run. Mid-run the tool list is part of the prompt prefix and must not move.
    pub(crate) stale: bool,
    /// Recent `notifications/message` log lines, bounded, for `ai mcp`.
    pub(crate) notes: Vec<String>,
}

impl<T: McpTransport> McpClient<T> {
    /// A bare client — no probe, no listing. For tests and synthetic hubs only.
    #[cfg(test)]
    pub(crate) fn raw(transport: T, timeout: Duration) -> McpClient<T> {
        McpClient {
            transport,
            next_id: 1,
            timeout,
            era: Era::Modern(MODERN.into()),
            tools: Vec::new(),
            resources: false,
            server_info: String::new(),
            stale: false,
            notes: Vec::new(),
        }
    }

    /// Connect: probe the era (`server/discover` → modern, else the legacy
    /// `initialize` handshake), then list the tools.
    pub(crate) fn connect(transport: T, timeout: Duration) -> Result<McpClient<T>, String> {
        let mut c = McpClient {
            transport,
            next_id: 1,
            timeout,
            era: Era::Modern(MODERN.into()),
            tools: Vec::new(),
            resources: false,
            server_info: String::new(),
            stale: false,
            notes: Vec::new(),
        };
        match c.roundtrip("server/discover", Json::Obj(vec![]), PROBE_TIMEOUT)? {
            Round::Reply(reply) => match judge_discover(reply.clone()) {
                Probe::Modern(version) => {
                    c.era = Era::Modern(version);
                    if let Ok(result) = reply {
                        c.adopt(&result);
                    }
                }
                Probe::Legacy => c.legacy_handshake()?,
                Probe::Incompatible(why) => return Err(why),
            },
            // Silence is the other legacy signal the spec names.
            Round::TimedOut(_) => c.legacy_handshake()?,
        }
        c.transport.era_settled(&c.era);
        c.tools = c.list_tools()?;
        Ok(c)
    }

    /// The legacy `initialize` → `notifications/initialized` handshake.
    fn legacy_handshake(&mut self) -> Result<(), String> {
        self.era = Era::Legacy(LEGACY_PREFERRED.into());
        let params = Json::Obj(vec![
            ("protocolVersion".into(), Json::Str(LEGACY_PREFERRED.into())),
            ("capabilities".into(), Json::Obj(vec![])),
            (
                "clientInfo".into(),
                Json::Obj(vec![
                    ("name".into(), Json::Str(corelib::brand::NAME.into())),
                    ("version".into(), Json::Str(env!("CARGO_PKG_VERSION").into())),
                ]),
            ),
        ]);
        let result = self.expect("initialize", params, INIT_TIMEOUT)?;
        self.era = judge_initialize(&result)?;
        self.adopt(&result);
        let line = wire::notification("notifications/initialized", Json::Obj(vec![]));
        self.transport.send(&line)
    }

    /// Record what a handshake-shaped result says about the server: its identity
    /// (display only) and whether it serves resources.
    fn adopt(&mut self, result: &Json) {
        if let Some(info) = result.get("serverInfo") {
            let name = info.get("name").and_then(Json::as_str).unwrap_or("");
            let version = info.get("version").and_then(Json::as_str).unwrap_or("");
            self.server_info = format!("{name} {version}").trim().to_string();
        }
        self.resources = result.get("capabilities").and_then(|c| c.get("resources")).is_some();
    }

    /// `tools/list`, following `nextCursor` until the catalogue is whole — a server
    /// past its first page used to lose every later tool silently.
    fn list_tools(&mut self) -> Result<Vec<McpTool>, String> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => Json::Obj(vec![("cursor".into(), Json::Str(c.clone()))]),
                None => Json::Obj(vec![]),
            };
            let page = self.expect("tools/list", params, INIT_TIMEOUT)?;
            for t in page.get("tools").and_then(Json::as_array).unwrap_or(&[]) {
                match McpTool::parse(t) {
                    Ok(tool) => out.push(tool),
                    Err(why) => self.note(format!("tool rejected: {why}")),
                }
            }
            cursor = page.get("nextCursor").and_then(Json::as_str).map(str::to_string);
            if cursor.is_none() {
                return Ok(out);
            }
        }
        Err(format!("tools/list never finished paginating ({MAX_PAGES} pages)"))
    }

    /// Call a tool by its bare (server-local) name. Returns the rendered text;
    /// `isError` results and protocol errors map to `Err` for the loop to classify.
    pub fn call(&mut self, tool: &str, args: Json) -> Result<String, String> {
        let params = Json::Obj(vec![("name".into(), Json::Str(tool.to_string())), ("arguments".into(), args)]);
        let result = self.expect("tools/call", params, self.timeout)?;
        super::content::render(&result)
    }

    /// `resources/list`, one line per resource.
    pub(crate) fn list_resources(&mut self) -> Result<String, String> {
        let mut lines = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => Json::Obj(vec![("cursor".into(), Json::Str(c.clone()))]),
                None => Json::Obj(vec![]),
            };
            let page = self.expect("resources/list", params, self.timeout)?;
            for r in page.get("resources").and_then(Json::as_array).unwrap_or(&[]) {
                let get = |k: &str| r.get(k).and_then(Json::as_str).unwrap_or("");
                let mut line = get("uri").to_string();
                if !get("name").is_empty() {
                    line.push_str(&format!(" \u{2014} {}", get("name")));
                }
                if !get("description").is_empty() {
                    line.push_str(&format!(": {}", get("description")));
                }
                if !get("mimeType").is_empty() {
                    line.push_str(&format!(" ({})", get("mimeType")));
                }
                lines.push(line);
            }
            cursor = page.get("nextCursor").and_then(Json::as_str).map(str::to_string);
            if cursor.is_none() {
                let text = lines.join("\n");
                return Ok(if text.is_empty() { "no resources".into() } else { text });
            }
        }
        Err(format!("resources/list never finished paginating ({MAX_PAGES} pages)"))
    }

    /// `resources/read` for one uri.
    pub(crate) fn read_resource(&mut self, uri: &str) -> Result<String, String> {
        let params = Json::Obj(vec![("uri".into(), Json::Str(uri.to_string()))]);
        let result = self.expect("resources/read", params, self.timeout)?;
        super::content::render_read(&result)
    }

    fn note(&mut self, line: String) {
        if self.notes.len() == NOTES_KEEP {
            self.notes.remove(0);
        }
        self.notes.push(line);
    }

    /// One request that must produce a settled result: timeout is cancelled and
    /// surfaced, RPC errors are named with their method.
    fn expect(&mut self, method: &str, params: Json, timeout: Duration) -> Result<Json, String> {
        match self.roundtrip(method, params, timeout)? {
            Round::Reply(Ok(result)) => wire::settle(result).map_err(|e| format!("mcp '{method}': {e}")),
            Round::Reply(Err(e)) => Err(format!("mcp '{method}': {}", e.message)),
            Round::TimedOut(id) => {
                // The spec's shape for giving up: tell the server to stop the work,
                // then report. The id names WHICH work — a later stray reply to it
                // is ignored by the loop, never misread as some other call's answer.
                let params = Json::Obj(vec![
                    ("requestId".into(), Json::Num(id as f64)),
                    ("reason".into(), Json::Str("timed out".into())),
                ]);
                let _ = self.transport.send(&wire::notification("notifications/cancelled", params));
                Err(format!("mcp: '{method}' timed out after {}s", timeout.as_secs()))
            }
        }
    }

    /// Send one request and pump the channel until ITS reply, answering the server's
    /// own traffic along the way.
    fn roundtrip(&mut self, method: &str, params: Json, timeout: Duration) -> Result<Round, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.transport.send(&wire::request(id, method, params, &self.era))?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Ok(Round::TimedOut(id));
            }
            let Some(line) = self.transport.recv(left)? else {
                return Ok(Round::TimedOut(id));
            };
            match wire::classify(&line) {
                wire::Incoming::Reply { id: got, result } if got == id => return Ok(Round::Reply(result)),
                wire::Incoming::Reply { .. } => continue, // a cancelled call's stray answer
                wire::Incoming::ServerRequest { id, method } => {
                    // `ping` is the one server request a tools-only client serves.
                    // Everything else — roots, sampling, elicitation — is a capability
                    // this client deliberately never declared, and the spec's answer
                    // for an unserved method is an error, not silence: silence leaves
                    // the server blocked on us for its own timeout.
                    let answer = match method.as_str() {
                        "ping" => wire::respond_ok(&id, Json::Obj(vec![])),
                        _ => wire::respond_err(&id, -32601, "this client does not serve that method"),
                    };
                    self.transport.send(&answer)?;
                }
                wire::Incoming::Notification { method, params } => match method.as_str() {
                    "notifications/tools/list_changed" => self.stale = true,
                    "notifications/message" => {
                        let text = params.get("data").map(Json::to_string).unwrap_or_default();
                        self.note(text.chars().take(200).collect());
                    }
                    _ => {} // progress and future notification kinds — informational
                },
                wire::Incoming::Noise => continue,
            }
        }
    }
}
