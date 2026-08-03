//! A real **MCP** (Model Context Protocol) client — protocol revision `2026-07-28`,
//! with the spec's own dual-era fallback to `initialize`-based servers.
//!
//! An `ai/mcp/<name>.toml` declares a tool-server: a local `command` the client
//! spawns and speaks newline-delimited JSON-RPC with over stdio, or a remote `url`
//! it POSTs to over Streamable HTTP. Each server's tools surface to the agent as
//! `@tool` entries named `mcp.<server>.<tool>` — with their **input schemas**, so
//! the model calls them with the arguments they actually take — and a server that
//! declares the `resources` capability additionally surfaces
//! `mcp.<server>.resources.list` / `.read`.
//!
//! The module is a set of small parts, each testable alone: [`wire`] builds and
//! classifies JSON-RPC, [`era`] decides modern-vs-legacy, [`content`] renders
//! results, [`client`] runs the loop, [`stdio`]/[`http`] move lines. Server output
//! is untrusted end to end: results are bounded and redacted before they re-enter
//! the model, and tool descriptions are sanitized before they enter a prompt.

mod client;
mod content;
mod era;
mod http;
mod stdio;
mod wire;

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use corelib::wire::{Json, Toml};

pub use client::McpClient;
pub(crate) use stdio::StdioTransport;

/// The line transport an [`McpClient`] speaks JSON-RPC over. Real = child stdio or
/// Streamable HTTP; scripted = canned responses (tests). `recv` returns `Ok(None)`
/// on timeout.
pub trait McpTransport {
    fn send(&mut self, line: &str) -> Result<(), String>;
    fn recv(&mut self, timeout: Duration) -> Result<Option<String>, String>;
    /// Told once, after negotiation — an HTTP transport keeps its
    /// `MCP-Protocol-Version` header in step; stdio needs nothing.
    fn era_settled(&mut self, _era: &wire::Era) {}
    /// Recent stderr, for diagnostics. Only a subprocess has any.
    fn stderr_tail(&self) -> Vec<String> {
        Vec::new()
    }
}

impl McpTransport for Box<dyn McpTransport + Send> {
    fn send(&mut self, line: &str) -> Result<(), String> {
        (**self).send(line)
    }
    fn recv(&mut self, timeout: Duration) -> Result<Option<String>, String> {
        (**self).recv(timeout)
    }
    fn era_settled(&mut self, era: &wire::Era) {
        (**self).era_settled(era)
    }
    fn stderr_tail(&self) -> Vec<String> {
        (**self).stderr_tail()
    }
}

/// How a declared server is reached.
#[derive(Clone, Debug, PartialEq)]
pub enum Reach {
    /// Spawn `command args…` with `env`, speak over its stdio.
    Stdio { command: String, args: Vec<String>, env: Vec<(String, String)> },
    /// POST to `url` with these extra headers (values may be `$VAR` references).
    Http { url: String, headers: Vec<(String, String)> },
}

/// A declared tool-server (`ai/mcp/<name>.toml`).
#[derive(Clone, Debug, PartialEq)]
pub struct McpServer {
    pub name: String,
    pub reach: Reach,
    /// Per-call timeout in seconds.
    pub timeout_s: u64,
}

/// Default per-call timeout.
const CALL_TIMEOUT_S: u64 = 30;

impl McpServer {
    /// Parse a declaration: `command` + `args[]` + `[env]` for a local server,
    /// `url` + `[headers]` for a remote one; `timeout_s` for either. A file
    /// declaring both (or neither) is not a server.
    pub fn parse(name: &str, text: &str) -> Option<McpServer> {
        if !valid_server_name(name) {
            return None;
        }
        let doc = Toml::parse(text).ok()?;
        let timeout_s = doc.get("timeout_s").and_then(Toml::as_int).map(|n| n.clamp(1, 600) as u64).unwrap_or(CALL_TIMEOUT_S);
        let table = |key: &str| -> Vec<(String, String)> {
            doc.get(key)
                .and_then(Toml::as_table)
                .map(|t| t.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
                .unwrap_or_default()
        };
        let reach = match (doc.get("command").and_then(Toml::as_str), doc.get("url").and_then(Toml::as_str)) {
            (Some(command), None) => Reach::Stdio {
                command: command.to_string(),
                args: doc
                    .get("args")
                    .and_then(Toml::as_array)
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
                env: table("env"),
            },
            (None, Some(url)) => Reach::Http { url: url.to_string(), headers: table("headers") },
            _ => return None,
        };
        Some(McpServer { name: name.to_string(), reach, timeout_s })
    }
}

/// A server name must be routable: `mcp.<server>.<tool>` splits at the FIRST dot
/// after the prefix, so a dotted name would swallow part of every tool's name.
fn valid_server_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A tool name per the spec: 1–128 chars of `[A-Za-z0-9_.-]`.
fn valid_tool_name(name: &str) -> bool {
    (1..=128).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// One tool a server advertised in `tools/list`.
#[derive(Clone, Debug, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    /// The `title`, when it says more than the name does.
    pub title: String,
    /// The JSON Schema for the arguments — serialized compact. THE fix for "the
    /// model guesses argument names": without this, every call was a guess.
    pub input_schema: String,
    /// Behaviour annotations, untrusted but useful as prompt hints.
    pub read_only: bool,
    pub destructive: bool,
}

impl McpTool {
    /// Parse one entry of a `tools/list` page; invalid names are rejected with a
    /// reason (the spec's rule: one bad definition must not take the rest down).
    fn parse(t: &Json) -> Result<McpTool, String> {
        let name = t.get("name").and_then(Json::as_str).unwrap_or("").to_string();
        if !valid_tool_name(&name) {
            return Err(format!("invalid tool name {name:?}"));
        }
        let hint = |k: &str| matches!(t.get("annotations").and_then(|a| a.get(k)), Some(Json::Bool(true)));
        Ok(McpTool {
            description: t.get("description").and_then(Json::as_str).unwrap_or("").to_string(),
            title: t.get("title").and_then(Json::as_str).unwrap_or("").to_string(),
            input_schema: t.get("inputSchema").map(Json::to_string).unwrap_or_default(),
            read_only: hint("readOnlyHint"),
            destructive: hint("destructiveHint"),
            name,
        })
    }

    /// The one-line description spliced into the agent's prompt: sanitized (this is
    /// a foreign server writing into a system prompt), schema included, hints named.
    fn describe(&self) -> String {
        let mut s = String::new();
        if !self.title.is_empty() && self.title != self.name {
            s.push_str(&sanitize(&self.title, 80));
            s.push_str(" \u{2014} ");
        }
        s.push_str(&sanitize(&self.description, 600));
        if self.destructive {
            s.push_str(" [destructive]");
        } else if self.read_only {
            s.push_str(" [read-only]");
        }
        if !self.input_schema.is_empty() {
            s.push_str(" \u{b7} args: ");
            s.push_str(&sanitize(&self.input_schema, 1500));
        }
        s.trim().to_string()
    }
}

/// Foreign text on its way into a prompt: control characters out, whitespace
/// collapsed, bounded. Not a defence against persuasion — nothing is — but a
/// description can no longer smuggle raw escapes or a fake transcript turn.
fn sanitize(text: &str, max: usize) -> String {
    // Whitespace-flavoured control chars (\n, \t) become separators; the rest —
    // escapes, NULs — are dropped outright.
    let clean: String = text.chars().filter(|c| !c.is_control() || c.is_whitespace()).collect();
    let one = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    match one.chars().count() > max {
        true => format!("{}\u{2026}", one.chars().take(max.saturating_sub(1)).collect::<String>()),
        false => one,
    }
}

/// Load all server declarations across `dirs` (project-first; first per name wins).
pub fn load_servers(dirs: &[PathBuf]) -> Vec<McpServer> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml")).collect();
        files.sort();
        for p in files {
            let Some(name) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            if !seen.insert(name.to_string()) {
                continue;
            }
            if let Some(s) = std::fs::read_to_string(&p).ok().and_then(|t| McpServer::parse(name, &t)) {
                out.push(s);
            }
        }
    }
    out
}

/// Qualify every server's tools and put them in one stable order.
///
/// The order is not cosmetic. This list is spliced into an agent's system prompt,
/// which is the prefix a provider caches — and a cache only pays out on a prefix
/// that matches token for token. A server answering `tools/list` in a different
/// order on Tuesday would silently void the cache for every run afterwards and
/// nothing would look wrong except the bill. (The 2026-07-28 spec now recommends
/// deterministic ordering for exactly this reason; sorting makes it unconditional.)
fn qualify<'a>(servers: impl Iterator<Item = (&'a str, Vec<(String, String)>)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = servers
        .flat_map(|(server, tools)| {
            tools.into_iter().map(move |(name, describe)| (format!("mcp.{server}.{name}"), describe))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// `$VAR` / `${VAR}` in an env or header value → that variable's value at launch
/// time, so a rotated token takes effect without editing the declaration. A literal
/// stays a literal; an unset variable resolves to empty (the server will say so).
fn expand(v: &str) -> String {
    let Some(rest) = v.strip_prefix('$') else { return v.to_string() };
    let name = rest.strip_prefix('{').and_then(|r| r.strip_suffix('}')).unwrap_or(rest);
    std::env::var(name).unwrap_or_default()
}

/// One server's standing, for `ai mcp`.
pub struct ServerReport {
    pub name: String,
    /// `stdio` or `http`.
    pub reach: &'static str,
    /// `modern 2026-07-28` / `legacy 2025-06-18` — empty when the server failed.
    pub era: String,
    /// The server's self-reported identity (display only).
    pub info: String,
    pub tools: usize,
    pub resources: bool,
    /// Why it is not running, when it is not.
    pub error: String,
    /// The tail of its stderr, when there is one.
    pub stderr: Vec<String>,
}

type ErasedClient = McpClient<Box<dyn McpTransport + Send>>;

/// One live server: its name, how it is reached (for display), and its client.
struct Live {
    name: String,
    reach: &'static str,
    client: ErasedClient,
}

/// A session-scoped set of live MCP servers: launched and negotiated concurrently,
/// the union of their tools exposed qualified, calls routed. Servers shut down
/// gracefully when the hub drops.
pub struct McpHub {
    clients: Vec<Live>,
    failed: Vec<(McpServer, String, Vec<String>)>,
}

impl McpHub {
    /// Launch every server, in parallel — a slow handshake on one must not stack
    /// onto the others. A server that fails is reported and skipped, never fatal.
    pub fn launch(servers: &[McpServer]) -> McpHub {
        let handles: Vec<_> = servers
            .iter()
            .cloned()
            .map(|s| std::thread::spawn(move || (connect(&s), s)))
            .collect();
        let mut hub = McpHub { clients: Vec::new(), failed: Vec::new() };
        for h in handles {
            let Ok((outcome, server)) = h.join() else { continue };
            match outcome {
                Ok(client) => hub.clients.push(Live { name: server.name.clone(), reach: reach_word(&server.reach), client }),
                Err((why, stderr)) => {
                    eprintln!("aiTerminal: mcp server '{}' failed to start \u{2014} {why}", server.name);
                    hub.failed.push((server, why, stderr));
                }
            }
        }
        // Launch order is thread-completion order; the hub's own order must not be.
        hub.clients.sort_by(|a, b| a.name.cmp(&b.name));
        hub
    }

    pub fn is_empty(&self) -> bool {
        self.clients.iter().all(|s| s.client.tools.is_empty() && !s.client.resources)
    }

    /// Every tool across all servers, qualified `mcp.<server>.<tool>` with its
    /// schema-bearing description, **sorted by name** — plus the two resource
    /// tools for each server that declared the capability.
    pub fn tools(&self) -> Vec<(String, String)> {
        qualify(self.clients.iter().map(|live| {
            let (name, c) = (&live.name, &live.client);
            let mut tools: Vec<(String, String)> = c.tools.iter().map(|t| (t.name.clone(), t.describe())).collect();
            if c.resources {
                tools.push((
                    "resources.list".into(),
                    format!("List the resources server '{name}' exposes (uri \u{2014} name: description) \u{b7} args: {{}}"),
                ));
                tools.push((
                    "resources.read".into(),
                    "Read one resource by uri \u{b7} args: {\"type\":\"object\",\"properties\":{\"uri\":{\"type\":\"string\"}},\"required\":[\"uri\"]}".into(),
                ));
            }
            (name.as_str(), tools)
        }))
    }

    /// Route a qualified `mcp.<server>.<tool>` call. `args` is the JSON arguments
    /// object (parsed from the model's `@tool` line). Returns the rendered text.
    pub fn call(&mut self, qualified: &str, args: Json) -> Result<String, String> {
        let rest = qualified.strip_prefix("mcp.").ok_or("mcp: not an mcp tool")?;
        let (server, tool) = rest.split_once('.').ok_or("mcp: expected mcp.<server>.<tool>")?;
        let client = self
            .clients
            .iter_mut()
            .find(|s| s.name == server)
            .map(|s| &mut s.client)
            .ok_or_else(|| format!("mcp: no server '{server}'"))?;
        match tool {
            "resources.list" if client.resources => client.list_resources(),
            "resources.read" if client.resources => {
                let uri = args.get("uri").and_then(Json::as_str).ok_or("mcp: resources.read needs {\"uri\": \"…\"}")?;
                client.read_resource(&uri.to_string())
            }
            _ => client.call(tool, args),
        }
    }

    /// The standing of every declared server — live and failed — for `ai mcp`.
    pub fn report(&self) -> Vec<ServerReport> {
        let mut out: Vec<ServerReport> = self
            .clients
            .iter()
            .map(|live| ServerReport {
                name: live.name.clone(),
                reach: live.reach,
                era: format!("{} {}", live.client.era.word(), live.client.era.version()),
                info: live.client.server_info.clone(),
                tools: live.client.tools.len(),
                resources: live.client.resources,
                error: String::new(),
                stderr: live.client.transport.stderr_tail(),
            })
            .collect();
        for (server, why, stderr) in &self.failed {
            out.push(ServerReport {
                name: server.name.clone(),
                reach: reach_word(&server.reach),
                era: String::new(),
                info: String::new(),
                tools: 0,
                resources: false,
                error: why.clone(),
                stderr: stderr.clone(),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

fn reach_word(reach: &Reach) -> &'static str {
    match reach {
        Reach::Stdio { .. } => "stdio",
        Reach::Http { .. } => "http",
    }
}

/// Open one server: build its transport, negotiate, list. On failure the stderr
/// tail rides along — the reason a subprocess died is usually written there.
fn connect(server: &McpServer) -> Result<ErasedClient, (String, Vec<String>)> {
    let timeout = Duration::from_secs(server.timeout_s);
    match &server.reach {
        Reach::Stdio { command, args, env } => {
            let env: Vec<(String, String)> = env.iter().map(|(k, v)| (k.clone(), expand(v))).collect();
            let transport = StdioTransport::spawn(&server.name, command, args, &env).map_err(|e| (e, Vec::new()))?;
            let tail = transport.stderr.clone();
            let boxed: Box<dyn McpTransport + Send> = Box::new(transport);
            McpClient::connect(boxed, timeout).map_err(|e| (e, tail.lines()))
        }
        Reach::Http { url, headers } => {
            let headers: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.clone(), expand(v))).collect();
            let http = http::HttpTransport::new(url, headers, Box::new(platform::transport::CurlExchange::default()));
            let boxed: Box<dyn McpTransport + Send> = Box::new(http);
            McpClient::connect(boxed, timeout).map_err(|e| (e, Vec::new()))
        }
    }
}

/// A hub over a canned in-process server — no subprocess, no network — for the
/// scenario worlds: the REAL client, negotiation, listing and routing run against
/// declared data, so a journey about MCP drives the code a run drives.
#[cfg(test)]
pub(crate) mod scripted {
    use super::*;

    /// Answers `server/discover`, `tools/list` and `tools/call` from declared data.
    struct FakeServer {
        /// `(name, description, input schema JSON)` per tool.
        tools: Vec<(String, String, String)>,
        /// `(tool name, text result)` — a call to anything else is an `isError`.
        results: Vec<(String, String)>,
        queue: std::collections::VecDeque<String>,
    }

    impl McpTransport for FakeServer {
        fn send(&mut self, line: &str) -> Result<(), String> {
            let v = Json::parse(line).map_err(|e| e)?;
            let method = v.get("method").and_then(Json::as_str).unwrap_or("");
            let Some(Json::Num(id)) = v.get("id") else { return Ok(()) };
            let reply = |result: Json| {
                Json::Obj(vec![
                    ("jsonrpc".into(), Json::Str("2.0".into())),
                    ("id".into(), Json::Num(*id)),
                    ("result".into(), result),
                ])
                .to_string()
            };
            match method {
                "server/discover" => self.queue.push_back(reply(Json::Obj(vec![
                    ("supportedVersions".into(), Json::Arr(vec![Json::Str(wire::MODERN.into())])),
                    ("serverInfo".into(), Json::Obj(vec![("name".into(), Json::Str("scripted".into())), ("version".into(), Json::Str("1".into()))])),
                    ("capabilities".into(), Json::Obj(vec![("tools".into(), Json::Obj(vec![]))])),
                ]))),
                "tools/list" => {
                    let tools = self
                        .tools
                        .iter()
                        .map(|(name, describe, schema)| {
                            Json::Obj(vec![
                                ("name".into(), Json::Str(name.clone())),
                                ("description".into(), Json::Str(describe.clone())),
                                ("inputSchema".into(), Json::parse(schema).unwrap_or(Json::Obj(vec![]))),
                            ])
                        })
                        .collect();
                    self.queue.push_back(reply(Json::Obj(vec![("tools".into(), Json::Arr(tools))])));
                }
                "tools/call" => {
                    let name = v.get("params").and_then(|p| p.get("name")).and_then(Json::as_str).unwrap_or("");
                    let (text, is_err) = match self.results.iter().find(|(n, _)| n == name) {
                        Some((_, text)) => (text.clone(), false),
                        None => (format!("no result scripted for {name:?}"), true),
                    };
                    let mut result = vec![(
                        "content".into(),
                        Json::Arr(vec![Json::Obj(vec![("type".into(), Json::Str("text".into())), ("text".into(), Json::Str(text))])]),
                    )];
                    if is_err {
                        result.push(("isError".into(), Json::Bool(true)));
                    }
                    self.queue.push_back(reply(Json::Obj(result)));
                }
                _ => {}
            }
            Ok(())
        }
        fn recv(&mut self, _timeout: Duration) -> Result<Option<String>, String> {
            Ok(self.queue.pop_front())
        }
    }

    /// Build a live-shaped hub named `server` around the declared tools/results.
    pub(crate) fn hub(server: &str, tools: &[(String, String, String)], results: &[(String, String)]) -> Result<McpHub, String> {
        let fake = FakeServer { tools: tools.to_vec(), results: results.to_vec(), queue: Default::default() };
        let boxed: Box<dyn McpTransport + Send> = Box::new(fake);
        let client = McpClient::connect(boxed, Duration::from_secs(30))?;
        Ok(McpHub { clients: vec![Live { name: server.to_string(), reach: "scripted", client }], failed: Vec::new() })
    }
}

#[cfg(test)]
mod tests;
