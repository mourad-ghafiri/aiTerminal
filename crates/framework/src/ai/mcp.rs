//! A real **MCP** (Model Context Protocol) client. An `ai/mcp/<name>.toml` declares a
//! tool-server (`command` + `args` + `env`); this module spawns it, speaks
//! newline-delimited JSON-RPC 2.0 over its stdio, performs the `initialize` →
//! `tools/list` handshake, and routes `tools/call`. Each server's tools surface to the
//! agent as `@tool` entries named `mcp.<server>.<tool>` — the same text protocol, so
//! MCP is model-agnostic and never edits the loop.
//!
//! The JSON-RPC engine is generic over an [`McpTransport`] (mirroring the AI
//! `Transport` seam) so it unit-tests offline against a scripted transport — no
//! subprocess, no network. The live transport spawns the declared process and is
//! killed on drop. Server output is untrusted: results are bounded + returned as tainted
//! text the caller redacts before it re-enters the model.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;

use corelib::wire::{Json, Toml};

/// Max bytes for a single JSON-RPC line (a hostile server can't OOM us).
const MAX_LINE: usize = 4 * 1024 * 1024;
/// How long to wait for a server's response to one request.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for the handshake.
const INIT_TIMEOUT: Duration = Duration::from_secs(10);

/// A declared tool-server (`ai/mcp/<name>.toml`).
#[derive(Clone, Debug, PartialEq)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// One tool a server advertised in `tools/list`.
#[derive(Clone, Debug, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: String,
}

impl McpServer {
    /// Parse a server declaration from its TOML (`command`, `args[]`, `[env]`).
    pub fn parse(name: &str, text: &str) -> Option<McpServer> {
        let doc = Toml::parse(text).ok()?;
        let command = doc.get("command").and_then(Toml::as_str)?.to_string();
        let args = doc
            .get("args")
            .and_then(Toml::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let env = doc
            .get("env")
            .and_then(Toml::as_table)
            .map(|t| t.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
            .unwrap_or_default();
        Some(McpServer { name: name.to_string(), command, args, env })
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

/// The line transport an [`McpClient`] speaks JSON-RPC over. Real = child stdio;
/// scripted = canned responses (tests). `recv` returns `None` on timeout.
pub trait McpTransport {
    fn send(&mut self, line: &str) -> Result<(), String>;
    fn recv(&mut self, timeout: Duration) -> Result<Option<String>, String>;
}

/// Live transport: the spawned server's stdin (`sink`) + a reader thread draining its
/// stdout into `rx`. The `child` handle is kept so the server is killed on drop.
pub struct StdioTransport {
    child: Child,
    rx: Receiver<String>,
    sink: std::process::ChildStdin,
}

impl StdioTransport {
    /// Spawn `server` with piped stdio + a reader thread for its stdout lines.
    pub fn spawn(server: &McpServer) -> Result<StdioTransport, String> {
        let mut cmd = Command::new(&server.command);
        cmd.args(&server.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, v) in &server.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| format!("mcp '{}': spawn failed: {e}", server.name))?;
        let sink = child.stdin.take().ok_or("mcp: no stdin")?;
        let stdout = child.stdout.take().ok_or("mcp: no stdout")?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                // BOUND the read at MAX_LINE bytes (not after — `read_line` would grow the
                // buffer to gigabytes on a newline-less line first → OOM). A line that hits
                // the cap without a terminator is treated as a protocol error: surface a
                // sentinel so a pending request fails fast, then stop reading.
                let mut buf: Vec<u8> = Vec::new();
                match reader.by_ref().take(MAX_LINE as u64).read_until(b'\n', &mut buf) {
                    Ok(0) => break, // EOF
                    Ok(_) if buf.last() == Some(&b'\n') => {
                        if tx.send(String::from_utf8_lossy(&buf).trim_end().to_string()).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {
                        let _ = tx.send("{\"jsonrpc\":\"2.0\",\"error\":{\"message\":\"mcp: oversize response line\"}}".to_string());
                        break;
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(StdioTransport { child, rx, sink })
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl McpTransport for StdioTransport {
    fn send(&mut self, line: &str) -> Result<(), String> {
        self.sink.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        self.sink.write_all(b"\n").map_err(|e| e.to_string())?;
        self.sink.flush().map_err(|e| e.to_string())
    }
    fn recv(&mut self, timeout: Duration) -> Result<Option<String>, String> {
        match self.rx.recv_timeout(timeout) {
            Ok(line) => Ok(Some(line)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err("mcp: server closed the connection".into()),
        }
    }
}

/// A JSON-RPC 2.0 MCP client over an [`McpTransport`]. Serial request/response (the
/// harness calls one tool per turn): each request loops `recv` until the matching id.
pub struct McpClient<T: McpTransport> {
    transport: T,
    next_id: u64,
    pub tools: Vec<McpTool>,
}

impl<T: McpTransport> McpClient<T> {
    /// Perform the `initialize` → `notifications/initialized` → `tools/list` handshake.
    pub fn connect(transport: T) -> Result<McpClient<T>, String> {
        let mut c = McpClient { transport, next_id: 1, tools: Vec::new() };
        let init = Json::Obj(vec![
            ("protocolVersion".into(), Json::Str("2024-11-05".into())),
            ("capabilities".into(), Json::Obj(vec![])),
            ("clientInfo".into(), Json::Obj(vec![("name".into(), Json::Str(corelib::brand::NAME.into())), ("version".into(), Json::Str("1.0".into()))])),
        ]);
        c.request("initialize", init, INIT_TIMEOUT)?;
        c.notify("notifications/initialized", Json::Obj(vec![]))?;
        let listed = c.request("tools/list", Json::Obj(vec![]), INIT_TIMEOUT)?;
        c.tools = listed
            .get("tools")
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|t| {
                        let name = t.get("name").and_then(Json::as_str)?.to_string();
                        let description = t.get("description").and_then(Json::as_str).unwrap_or("").to_string();
                        Some(McpTool { name, description })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(c)
    }

    /// Call a tool by its bare name (server-local). Returns the joined text content;
    /// `isError: true` results map to an `Err`.
    pub fn call(&mut self, tool: &str, args: Json) -> Result<String, String> {
        let params = Json::Obj(vec![("name".into(), Json::Str(tool.to_string())), ("arguments".into(), args)]);
        let result = self.request("tools/call", params, CALL_TIMEOUT)?;
        let text = result
            .get("content")
            .and_then(Json::as_array)
            .map(|a| a.iter().filter_map(|c| c.get("text").and_then(Json::as_str)).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        if matches!(result.get("isError"), Some(Json::Bool(true))) {
            Err(if text.is_empty() { format!("mcp tool '{tool}' failed") } else { text })
        } else {
            Ok(text)
        }
    }

    fn notify(&mut self, method: &str, params: Json) -> Result<(), String> {
        let msg = Json::Obj(vec![("jsonrpc".into(), Json::Str("2.0".into())), ("method".into(), Json::Str(method.into())), ("params".into(), params)]);
        self.transport.send(&msg.to_string())
    }

    fn request(&mut self, method: &str, params: Json, timeout: Duration) -> Result<Json, String> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = Json::Obj(vec![
            ("jsonrpc".into(), Json::Str("2.0".into())),
            ("id".into(), Json::Num(id as f64)),
            ("method".into(), Json::Str(method.into())),
            ("params".into(), params),
        ]);
        self.transport.send(&msg.to_string())?;
        // Read until the response with our id (skip notifications + other ids).
        loop {
            match self.transport.recv(timeout)? {
                None => return Err(format!("mcp: '{method}' timed out")),
                Some(line) if line.trim().is_empty() => continue,
                Some(line) => {
                    let Ok(v) = Json::parse(&line) else { continue };
                    let matches_id = matches!(v.get("id"), Some(Json::Num(n)) if *n as u64 == id);
                    if !matches_id {
                        continue; // a notification or a different response
                    }
                    if let Some(err) = v.get("error") {
                        let m = err.get("message").and_then(Json::as_str).unwrap_or("error");
                        return Err(format!("mcp '{method}': {m}"));
                    }
                    return Ok(v.get("result").cloned().unwrap_or(Json::Null));
                }
            }
        }
    }
}

/// A session-scoped set of live MCP servers. Spawns each declared server, performs the
/// handshake, and exposes the union of their tools (qualified `mcp.<server>.<tool>`) +
/// routes a qualified call to the right server. Servers are killed when the hub drops.
pub struct McpHub {
    clients: Vec<(String, McpClient<StdioTransport>)>,
}

impl McpHub {
    /// Launch every server in `servers`; a server that fails to start/handshake is
    /// warned about and skipped rather than failing the whole hub.
    pub fn launch(servers: &[McpServer]) -> McpHub {
        let mut clients = Vec::new();
        for s in servers {
            match StdioTransport::spawn(s).and_then(McpClient::connect) {
                Ok(c) => clients.push((s.name.clone(), c)),
                Err(e) => eprintln!("aiTerminal: mcp server '{}' failed to start — {e}", s.name),
            }
        }
        McpHub { clients }
    }

    pub fn is_empty(&self) -> bool {
        self.clients.iter().all(|(_, c)| c.tools.is_empty())
    }

    /// Every tool across all servers, qualified `mcp.<server>.<tool>` with its description.
    pub fn tools(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (server, c) in &self.clients {
            for t in &c.tools {
                out.push((format!("mcp.{server}.{}", t.name), t.description.clone()));
            }
        }
        out
    }

    /// Route a qualified `mcp.<server>.<tool>` call. `args` is a JSON arguments object
    /// (parsed from the model's `@tool` JSON). Returns the tool's text result.
    pub fn call(&mut self, qualified: &str, args: Json) -> Result<String, String> {
        let rest = qualified.strip_prefix("mcp.").ok_or("mcp: not an mcp tool")?;
        let (server, tool) = rest.split_once('.').ok_or("mcp: expected mcp.<server>.<tool>")?;
        let client = self.clients.iter_mut().find(|(s, _)| s == server).map(|(_, c)| c).ok_or_else(|| format!("mcp: no server '{server}'"))?;
        client.call(tool, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A scripted transport: each `send` of a request enqueues a canned response line
    /// (keyed by the JSON-RPC `method`); `recv` dequeues. Notifications enqueue nothing.
    struct ScriptedMcp {
        responses: VecDeque<String>,
        last_id: u64,
        // method → result JSON (string), echoed back with the request's id
        canned: Vec<(&'static str, &'static str)>,
        pub sent: Vec<String>,
    }
    impl ScriptedMcp {
        fn new(canned: Vec<(&'static str, &'static str)>) -> Self {
            ScriptedMcp { responses: VecDeque::new(), last_id: 0, canned, sent: Vec::new() }
        }
    }
    impl McpTransport for ScriptedMcp {
        fn send(&mut self, line: &str) -> Result<(), String> {
            self.sent.push(line.to_string());
            let v = Json::parse(line).unwrap();
            let method = v.get("method").and_then(Json::as_str).unwrap_or("");
            // notifications have no id → no response
            if let Some(Json::Num(n)) = v.get("id") {
                self.last_id = *n as u64;
                if let Some((_, result)) = self.canned.iter().find(|(m, _)| *m == method) {
                    let resp = format!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}", self.last_id, result);
                    self.responses.push_back(resp);
                }
            }
            Ok(())
        }
        fn recv(&mut self, _timeout: Duration) -> Result<Option<String>, String> {
            Ok(self.responses.pop_front())
        }
    }

    #[test]
    fn parses_server_declaration() {
        let s = McpServer::parse("fs", "command = \"node\"\nargs = [\"server.js\", \"--root\"]\n[env]\nTOKEN = \"abc\"\n").unwrap();
        assert_eq!(s.name, "fs");
        assert_eq!(s.command, "node");
        assert_eq!(s.args, vec!["server.js".to_string(), "--root".to_string()]);
        assert_eq!(s.env, vec![("TOKEN".to_string(), "abc".to_string())]);
        // missing command → not a server
        assert!(McpServer::parse("x", "args = []\n").is_none());
    }

    #[test]
    fn handshake_lists_tools_and_calls_them() {
        let t = ScriptedMcp::new(vec![
            ("initialize", "{\"serverInfo\":{\"name\":\"mock\"}}"),
            ("tools/list", "{\"tools\":[{\"name\":\"search\",\"description\":\"Search the index\"},{\"name\":\"fetch\",\"description\":\"Fetch a doc\"}]}"),
            ("tools/call", "{\"content\":[{\"type\":\"text\",\"text\":\"hello from mcp\"}]}"),
        ]);
        let mut c = McpClient::connect(t).unwrap();
        assert_eq!(c.tools.len(), 2);
        assert_eq!(c.tools[0].name, "search");
        assert_eq!(c.tools[0].description, "Search the index");
        let out = c.call("search", Json::Obj(vec![("q".into(), Json::Str("rust".into()))])).unwrap();
        assert_eq!(out, "hello from mcp");
        // the initialized notification was sent (no id)
        assert!(c.transport.sent.iter().any(|l| l.contains("notifications/initialized")));
    }

    /// End-to-end over a REAL subprocess: a tiny POSIX-sh server that answers
    /// initialize → tools/list → tools/call in order. Validates `StdioTransport`'s
    /// spawn + reader thread + the hub's qualified routing + kill-on-drop.
    #[cfg(unix)]
    #[test]
    fn real_subprocess_handshake_and_call() {
        let dir = std::env::temp_dir().join(format!("tt-mcp-srv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("server.sh");
        // `printf` (a sh builtin) writes unbuffered; the client sends one JSON line per
        // request. read order: initialize, initialized-notif, tools/list, tools/call.
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             read a\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\\n'\n\
             read b\nread c\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"ping\",\"description\":\"pong\"}]}}\\n'\n\
             read d\nprintf '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"pong!\"}]}}\\n'\n",
        )
        .unwrap();
        let server = McpServer { name: "mock".into(), command: "sh".into(), args: vec![script.display().to_string()], env: vec![] };
        let mut hub = McpHub::launch(&[server]);
        let tools = hub.tools();
        assert!(tools.iter().any(|(n, _)| n == "mcp.mock.ping"), "tools: {tools:?}");
        assert_eq!(hub.call("mcp.mock.ping", Json::Obj(vec![])).unwrap(), "pong!");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_error_and_timeout_surface_as_err() {
        // isError → Err with the text
        let t = ScriptedMcp::new(vec![
            ("initialize", "{}"),
            ("tools/list", "{\"tools\":[{\"name\":\"x\"}]}"),
            ("tools/call", "{\"isError\":true,\"content\":[{\"type\":\"text\",\"text\":\"boom\"}]}"),
        ]);
        let mut c = McpClient::connect(t).unwrap();
        assert_eq!(c.call("x", Json::Obj(vec![])).unwrap_err(), "boom");

        // no canned tools/call response → recv returns None → timeout error
        let t2 = ScriptedMcp::new(vec![("initialize", "{}"), ("tools/list", "{\"tools\":[]}")]);
        let mut c2 = McpClient::connect(t2).unwrap();
        assert!(c2.call("missing", Json::Obj(vec![])).unwrap_err().contains("timed out"));
    }
}
