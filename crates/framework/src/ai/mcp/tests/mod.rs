use super::*;
use std::collections::VecDeque;

#[test]
fn the_tool_catalogue_is_ordered_whatever_order_the_servers_answered_in() {
    // This list is spliced into an agent's system prompt, which is the prefix a
    // provider caches — and a cache pays out only on a prefix that matches token for
    // token. A server answering `tools/list` in a different order, or two servers
    // starting in a different order, would void the cache for every run afterwards
    // and nothing would look wrong except the bill.
    let tool = |n: &str| McpTool { name: n.to_string(), description: format!("does {n}") };
    let a = [tool("zeta"), tool("alpha")];
    let b = [tool("mid")];
    let one = qualify([("srv", a.as_slice()), ("other", b.as_slice())].into_iter());
    let two = qualify([("other", b.as_slice()), ("srv", a.as_slice())].into_iter());
    assert_eq!(one, two, "the same servers in a different order give the same catalogue");
    let names: Vec<&str> = one.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["mcp.other.mid", "mcp.srv.alpha", "mcp.srv.zeta"]);
    // Descriptions ride along with their tool, not with their position.
    assert_eq!(one[1].1, "does alpha");
}

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
