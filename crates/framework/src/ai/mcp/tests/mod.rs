use super::client::McpClient;
use super::era::{judge_discover, judge_initialize, Probe};
use super::wire::{self, Era, RpcError};
use super::*;
use std::collections::VecDeque;

// ── a scripted transport ────────────────────────────────────────────────────

/// Replies are templates keyed by method, consumed front-first (the last one
/// repeats). `{id}` is replaced by the request's id; a template may hold several
/// lines — server traffic the client must handle BEFORE its reply arrives.
struct Scripted {
    script: Vec<(&'static str, VecDeque<String>)>,
    queue: VecDeque<String>,
    sent: Vec<String>,
}

impl Scripted {
    fn new(entries: &[(&'static str, &str)]) -> Scripted {
        let mut script: Vec<(&'static str, VecDeque<String>)> = Vec::new();
        for (method, template) in entries {
            match script.iter_mut().find(|(m, _)| m == method) {
                Some((_, q)) => q.push_back(template.to_string()),
                None => script.push((method, VecDeque::from([template.to_string()]))),
            }
        }
        Scripted { script, queue: VecDeque::new(), sent: Vec::new() }
    }
    /// The requests/notifications the client sent, oldest first.
    fn sent(&self) -> &[String] {
        &self.sent
    }
}

impl McpTransport for Scripted {
    fn send(&mut self, line: &str) -> Result<(), String> {
        self.sent.push(line.to_string());
        let v = corelib::wire::Json::parse(line).unwrap();
        let method = v.get("method").and_then(corelib::wire::Json::as_str).unwrap_or("");
        let Some(corelib::wire::Json::Num(id)) = v.get("id") else { return Ok(()) };
        let id = *id as u64;
        if let Some((_, q)) = self.script.iter_mut().find(|(m, _)| *m == method) {
            let template = match q.len() {
                0 => return Ok(()),
                1 => q.front().cloned().unwrap(), // the last template repeats
                _ => q.pop_front().unwrap(),
            };
            for line in template.replace("{id}", &id.to_string()).lines() {
                self.queue.push_back(line.to_string());
            }
        }
        Ok(())
    }
    fn recv(&mut self, _timeout: std::time::Duration) -> Result<Option<String>, String> {
        Ok(self.queue.pop_front())
    }
}

const T: std::time::Duration = std::time::Duration::from_secs(30);

fn discover_ok() -> (&'static str, &'static str) {
    (
        "server/discover",
        "{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{\"supportedVersions\":[\"2026-07-28\"],\"serverInfo\":{\"name\":\"mock\",\"version\":\"1.2\"},\"capabilities\":{\"tools\":{}}}}",
    )
}

fn discover_unknown() -> (&'static str, &'static str) {
    ("server/discover", "{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{\"code\":-32601,\"message\":\"method not found\"}}")
}

fn init_ok(version: &'static str) -> (&'static str, String) {
    (
        "initialize",
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{{id}},\"result\":{{\"protocolVersion\":\"{version}\",\"serverInfo\":{{\"name\":\"old\",\"version\":\"0.3\"}},\"capabilities\":{{\"tools\":{{}}}}}}}}"
        ),
    )
}

fn tools_one() -> (&'static str, &'static str) {
    (
        "tools/list",
        "{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{\"tools\":[{\"name\":\"search\",\"description\":\"Search the index\",\"inputSchema\":{\"type\":\"object\",\"properties\":{\"q\":{\"type\":\"string\"}},\"required\":[\"q\"]}}]}}",
    )
}

// ── era decisions ───────────────────────────────────────────────────────────

#[test]
fn a_discover_result_offering_the_modern_version_settles_modern() {
    let reply = Json::parse("{\"supportedVersions\":[\"2025-11-25\",\"2026-07-28\"]}").unwrap();
    assert_eq!(judge_discover(Ok(reply)), Probe::Modern("2026-07-28".into()));
}

#[test]
fn a_discover_result_offering_only_legacy_versions_is_a_dual_era_server() {
    // It ANSWERED discover, so it is not deaf — but everything it offers is
    // initialize-based, and initialize is how a dual-era server serves those.
    let reply = Json::parse("{\"supportedVersions\":[\"2025-06-18\"]}").unwrap();
    assert_eq!(judge_discover(Ok(reply)), Probe::Legacy);
}

#[test]
fn a_modern_version_error_never_falls_back_to_initialize() {
    // The spec's sharpest rule: -32022 PROVES a modern server. Even when nothing
    // it offers is speakable, the answer is an error naming the versions — a
    // fallback to initialize against a modern server fails confusingly later.
    let e = |supported: &[&str]| RpcError { code: -32022, message: "unsupported".into(), supported: supported.iter().map(|s| s.to_string()).collect() };
    assert_eq!(judge_discover(Err(e(&["2026-07-28"]))), Probe::Modern("2026-07-28".into()));
    match judge_discover(Err(e(&["2099-01-01"]))) {
        Probe::Incompatible(why) => assert!(why.contains("2099-01-01"), "{why}"),
        other => panic!("expected Incompatible, got {other:?}"),
    }
}

#[test]
fn any_other_error_means_a_legacy_server() {
    let e = RpcError { code: -32601, message: "method not found".into(), supported: vec![] };
    assert_eq!(judge_discover(Err(e)), Probe::Legacy);
}

#[test]
fn initialize_accepts_every_known_legacy_revision_and_refuses_strangers() {
    for v in wire::LEGACY_KNOWN {
        let reply = Json::parse(&format!("{{\"protocolVersion\":\"{v}\"}}")).unwrap();
        assert_eq!(judge_initialize(&reply).unwrap(), Era::Legacy(v.to_string()));
    }
    let odd = Json::parse("{\"protocolVersion\":\"1999-09-09\"}").unwrap();
    assert!(judge_initialize(&odd).unwrap_err().contains("1999-09-09"));
}

// ── the wire ────────────────────────────────────────────────────────────────

#[test]
fn a_modern_request_carries_meta_and_a_legacy_one_does_not() {
    let modern = wire::request(7, "tools/list", Json::Obj(vec![]), &Era::Modern("2026-07-28".into()));
    for needle in ["io.modelcontextprotocol/protocolVersion", "2026-07-28", "io.modelcontextprotocol/clientCapabilities", "io.modelcontextprotocol/clientInfo"] {
        assert!(modern.contains(needle), "modern request missing {needle}: {modern}");
    }
    let legacy = wire::request(7, "tools/list", Json::Obj(vec![]), &Era::Legacy("2025-06-18".into()));
    assert!(!legacy.contains("_meta"), "legacy request must not carry _meta: {legacy}");
}

#[test]
fn incoming_lines_classify_into_all_four_kinds() {
    use wire::Incoming;
    assert!(matches!(wire::classify("{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}"), Incoming::Reply { id: 3, result: Ok(_) }));
    match wire::classify("{\"jsonrpc\":\"2.0\",\"id\":3,\"error\":{\"code\":-32022,\"message\":\"no\",\"data\":{\"supported\":[\"2026-07-28\"]}}}") {
        Incoming::Reply { result: Err(e), .. } => {
            assert_eq!(e.code, -32022);
            assert_eq!(e.supported, vec!["2026-07-28".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    // A server REQUEST has method + id — the kind the old client dropped.
    assert!(matches!(
        wire::classify("{\"jsonrpc\":\"2.0\",\"id\":\"srv-1\",\"method\":\"ping\"}"),
        Incoming::ServerRequest { method, .. } if method == "ping"
    ));
    assert!(matches!(
        wire::classify("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}"),
        Incoming::Notification { method, .. } if method == "notifications/tools/list_changed"
    ));
    assert!(matches!(wire::classify("not json"), Incoming::Noise));
}

#[test]
fn result_type_absent_is_complete_and_input_required_is_an_error() {
    assert!(wire::settle(Json::parse("{\"content\":[]}").unwrap()).is_ok());
    assert!(wire::settle(Json::parse("{\"resultType\":\"complete\"}").unwrap()).is_ok());
    assert!(wire::settle(Json::parse("{\"resultType\":\"input_required\"}").unwrap()).unwrap_err().contains("interactive input"));
    assert!(wire::settle(Json::parse("{\"resultType\":\"someday\"}").unwrap()).unwrap_err().contains("someday"));
}

// ── the client ──────────────────────────────────────────────────────────────

#[test]
fn a_modern_server_connects_without_initialize_and_lists_schemas() {
    let t = Scripted::new(&[discover_ok(), tools_one()]);
    let c = McpClient::connect(t, T).unwrap();
    assert_eq!(c.era, Era::Modern("2026-07-28".into()));
    assert_eq!(c.server_info, "mock 1.2");
    assert_eq!(c.tools.len(), 1);
    assert!(c.tools[0].input_schema.contains("\"required\":[\"q\"]"), "the schema must survive: {}", c.tools[0].input_schema);
    let sent = c.transport.sent();
    assert!(sent.iter().all(|l| !l.contains("\"initialize\"")), "a modern server must never see initialize");
    assert!(sent.iter().filter(|l| l.contains("\"method\":")).all(|l| l.contains("io.modelcontextprotocol/protocolVersion")), "every modern request carries _meta");
}

#[test]
fn a_silent_or_refusing_server_gets_the_legacy_handshake() {
    // Refusing: discover answered with method-not-found.
    let (m, tmpl) = init_ok("2025-03-26");
    let t = Scripted::new(&[discover_unknown(), (m, &tmpl), tools_one()]);
    let c = McpClient::connect(t, T).unwrap();
    assert_eq!(c.era, Era::Legacy("2025-03-26".into()), "the SERVER's negotiated version wins");
    let sent = c.transport.sent();
    assert!(sent.iter().any(|l| l.contains("notifications/initialized")), "the handshake's second half must be sent");
    assert!(sent.iter().filter(|l| l.contains("\"initialize\"")).all(|l| !l.contains("_meta")), "legacy requests carry no _meta");

    // Deaf: no discover entry at all — recv yields nothing and the probe times out.
    let (m, tmpl) = init_ok("2024-11-05");
    let t2 = Scripted::new(&[(m, &tmpl), tools_one()]);
    let c2 = McpClient::connect(t2, T).unwrap();
    assert_eq!(c2.era, Era::Legacy("2024-11-05".into()));
}

#[test]
fn a_server_ping_mid_call_is_answered_and_the_call_still_completes() {
    // The reply template smuggles a ping REQUEST in front of the real reply — the
    // exact traffic that used to leave the server waiting forever.
    let (m, tmpl) = init_ok("2025-06-18");
    let mut c = McpClient::connect(
        Scripted::new(&[
            discover_unknown(),
            (m, &tmpl),
            tools_one(),
            (
                "tools/call",
                "{\"jsonrpc\":\"2.0\",\"id\":\"srv-9\",\"method\":\"ping\"}\n{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"found it\"}]}}",
            ),
        ]),
        T,
    )
    .unwrap();
    let out = c.call("search", Json::Obj(vec![("q".into(), Json::Str("rust".into()))])).unwrap();
    assert_eq!(out, "found it");
    let answered = c.transport.sent().iter().any(|l| l.contains("\"id\":\"srv-9\"") && l.contains("\"result\":{}"));
    assert!(answered, "the ping must be answered: {:?}", c.transport.sent());
}

#[test]
fn an_unserved_server_request_gets_method_not_found_not_silence() {
    let (m, tmpl) = init_ok("2025-06-18");
    let mut c = McpClient::connect(
        Scripted::new(&[
            discover_unknown(),
            (m, &tmpl),
            tools_one(),
            (
                "tools/call",
                "{\"jsonrpc\":\"2.0\",\"id\":\"srv-2\",\"method\":\"sampling/createMessage\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{\"content\":[]}}",
            ),
        ]),
        T,
    )
    .unwrap();
    let _ = c.call("search", Json::Obj(vec![])).unwrap();
    let refused = c.transport.sent().iter().any(|l| l.contains("\"id\":\"srv-2\"") && l.contains("-32601"));
    assert!(refused, "sampling must be refused by code, not ignored");
}

#[test]
fn tools_list_follows_the_cursor_and_a_circular_cursor_is_refused() {
    let page1 = "{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{\"tools\":[{\"name\":\"a\",\"description\":\"first\"}],\"nextCursor\":\"p2\"}}";
    let page2 = "{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{\"tools\":[{\"name\":\"b\",\"description\":\"second\"}]}}";
    let c = McpClient::connect(Scripted::new(&[discover_ok(), ("tools/list", page1), ("tools/list", page2)]), T).unwrap();
    let names: Vec<&str> = c.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["a", "b"], "the second page must not be lost");

    // A server that hands back the same cursor forever must error, not spin.
    let circular = "{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{\"tools\":[],\"nextCursor\":\"again\"}}";
    let Err(err) = McpClient::connect(Scripted::new(&[discover_ok(), ("tools/list", circular)]), T) else {
        panic!("a circular cursor must not connect");
    };
    assert!(err.contains("paginating"), "{err}");
}

#[test]
fn an_invalid_tool_name_is_rejected_and_the_rest_survive() {
    let page = "{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{\"tools\":[{\"name\":\"has space\",\"description\":\"bad\"},{\"name\":\"fine\",\"description\":\"good\"}]}}";
    let c = McpClient::connect(Scripted::new(&[discover_ok(), ("tools/list", page)]), T).unwrap();
    assert_eq!(c.tools.len(), 1);
    assert_eq!(c.tools[0].name, "fine");
    assert!(c.notes.iter().any(|n| n.contains("has space")), "the rejection is noted: {:?}", c.notes);
}

#[test]
fn a_timed_out_call_sends_cancelled_and_names_the_wait() {
    let mut c = McpClient::connect(Scripted::new(&[discover_ok(), tools_one()]), T).unwrap();
    // No tools/call entry: recv yields nothing, the deadline passes immediately in
    // scripted time, and the client must both report and cancel.
    let err = c.call("search", Json::Obj(vec![])).unwrap_err();
    assert!(err.contains("timed out"), "{err}");
    let cancelled = c.transport.sent().iter().any(|l| l.contains("notifications/cancelled") && l.contains("requestId"));
    assert!(cancelled, "the server must be told to stop the work: {:?}", c.transport.sent());
}

#[test]
fn a_list_changed_notification_marks_the_catalogue_stale() {
    let (m, tmpl) = init_ok("2025-11-25");
    let mut c = McpClient::connect(
        Scripted::new(&[
            discover_unknown(),
            (m, &tmpl),
            tools_one(),
            (
                "tools/call",
                "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}",
            ),
        ]),
        T,
    )
    .unwrap();
    assert!(!c.stale);
    let _ = c.call("search", Json::Obj(vec![])).unwrap();
    assert!(c.stale, "the NEXT run should relist; this one must not move its prompt prefix");
}

// ── content rendering ───────────────────────────────────────────────────────

#[test]
fn every_content_shape_reaches_the_model_as_text() {
    let r = Json::parse(
        "{\"content\":[{\"type\":\"text\",\"text\":\"plain\"},{\"type\":\"resource_link\",\"uri\":\"file:///a.rs\",\"name\":\"a.rs\",\"description\":\"entry\"},{\"type\":\"image\",\"data\":\"QUJD\",\"mimeType\":\"image/png\"},{\"type\":\"resource\",\"resource\":{\"uri\":\"doc://x\",\"text\":\"embedded\"}}]}",
    )
    .unwrap();
    let out = content::render(&r).unwrap();
    assert!(out.contains("plain"));
    assert!(out.contains("\u{2192} file:///a.rs \u{2014} a.rs: entry"));
    assert!(out.contains("[image image/png, 4 bytes base64]"));
    assert!(out.contains("embedded"));
}

#[test]
fn a_structured_only_result_serializes_instead_of_vanishing() {
    let r = Json::parse("{\"structuredContent\":{\"temp\":21.5}}").unwrap();
    assert_eq!(content::render(&r).unwrap(), "{\"temp\":21.5}");
}

#[test]
fn is_error_maps_to_err_with_the_text() {
    let r = Json::parse("{\"isError\":true,\"content\":[{\"type\":\"text\",\"text\":\"boom\"}]}").unwrap();
    assert_eq!(content::render(&r).unwrap_err(), "boom");
    let silent = Json::parse("{\"isError\":true}").unwrap();
    assert!(content::render(&silent).unwrap_err().contains("no message"));
}

#[test]
fn a_gigantic_result_is_clipped_with_the_cut_named() {
    let big = format!("{{\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}", "x".repeat(300 * 1024));
    let out = content::render(&Json::parse(&big).unwrap()).unwrap();
    assert!(out.len() < 257 * 1024);
    assert!(out.ends_with("[result truncated at 256 KiB]"));
}

#[test]
fn resources_read_renders_text_and_describes_blobs() {
    let r = Json::parse("{\"contents\":[{\"uri\":\"doc://a\",\"text\":\"hello\"},{\"uri\":\"doc://b\",\"mimeType\":\"application/pdf\",\"blob\":\"QUJDRA==\"}]}").unwrap();
    let out = content::render_read(&r).unwrap();
    assert!(out.contains("hello"));
    assert!(out.contains("[application/pdf resource doc://b, 8 bytes base64]"));
}

// ── the tool surface ────────────────────────────────────────────────────────

#[test]
fn describe_carries_the_schema_the_hints_and_nothing_hostile() {
    let tool = McpTool {
        name: "drop_db".into(),
        title: "Drop database".into(),
        description: "Careful\u{1b}[31m now\nreally".into(),
        input_schema: "{\"type\":\"object\"}".into(),
        read_only: false,
        destructive: true,
    };
    let d = tool.describe();
    assert!(d.contains("Drop database \u{2014} Careful[31m now really"), "control chars stripped, lines joined: {d}");
    assert!(d.contains("[destructive]"));
    assert!(d.contains("args: {\"type\":\"object\"}"));
}

#[test]
fn the_tool_catalogue_is_ordered_whatever_order_the_servers_answered_in() {
    // This list is spliced into an agent's system prompt, which is the prefix a
    // provider caches — a cache pays out only on a prefix that matches token for
    // token, so the order must be a fact about the NAMES, not about startup timing.
    let tools = |names: &[&str]| names.iter().map(|n| (n.to_string(), format!("does {n}"))).collect::<Vec<_>>();
    let one = qualify([("srv", tools(&["zeta", "alpha"])), ("other", tools(&["mid"]))].into_iter().map(|(a, b)| (a, b)));
    let two = qualify([("other", tools(&["mid"])), ("srv", tools(&["zeta", "alpha"]))].into_iter().map(|(a, b)| (a, b)));
    assert_eq!(one, two);
    let names: Vec<&str> = one.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["mcp.other.mid", "mcp.srv.alpha", "mcp.srv.zeta"]);
}

#[test]
fn a_resources_capable_server_grows_the_two_synthetic_tools() {
    let with_resources = "{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{\"supportedVersions\":[\"2026-07-28\"],\"capabilities\":{\"tools\":{},\"resources\":{}}}}";
    let client = McpClient::connect(Scripted::new(&[("server/discover", with_resources), tools_one()]), T).unwrap();
    assert!(client.resources);
    let boxed: Box<dyn McpTransport + Send> = Box::new(Scripted::new(&[]));
    let _ = boxed; // the hub is exercised through its private constructor below
    let hub = McpHub {
        clients: vec![Live { name: "docs".into(), reach: "stdio", client: erase(client) }],
        failed: Vec::new(),
    };
    let names: Vec<String> = hub.tools().into_iter().map(|(n, _)| n).collect();
    assert!(names.contains(&"mcp.docs.resources.list".to_string()), "{names:?}");
    assert!(names.contains(&"mcp.docs.resources.read".to_string()));
    assert!(names.contains(&"mcp.docs.search".to_string()));
}

/// Rebuild a scripted client behind the hub's erased transport type.
fn erase(c: McpClient<Scripted>) -> super::McpClient<Box<dyn McpTransport + Send>> {
    let boxed: Box<dyn McpTransport + Send> = Box::new(c.transport);
    let mut out = McpClient::raw(boxed, T);
    out.tools = c.tools;
    out.resources = c.resources;
    out.server_info = c.server_info;
    out.era = c.era;
    out
}

// ── declarations ────────────────────────────────────────────────────────────

#[test]
fn declarations_parse_stdio_or_http_but_never_both() {
    let s = McpServer::parse("fs", "command = \"node\"\nargs = [\"server.js\"]\ntimeout_s = 90\n[env]\nTOKEN = \"$MY_TOKEN\"\n").unwrap();
    assert_eq!(s.timeout_s, 90);
    assert_eq!(s.reach, Reach::Stdio { command: "node".into(), args: vec!["server.js".into()], env: vec![("TOKEN".into(), "$MY_TOKEN".into())] });

    let h = McpServer::parse("remote", "url = \"https://mcp.example.com/mcp\"\n[headers]\nAuthorization = \"Bearer $API\"\n").unwrap();
    assert_eq!(h.reach, Reach::Http { url: "https://mcp.example.com/mcp".into(), headers: vec![("Authorization".into(), "Bearer $API".into())] });

    assert!(McpServer::parse("x", "args = []\n").is_none(), "neither command nor url");
    assert!(McpServer::parse("x", "command = \"a\"\nurl = \"https://b\"\n").is_none(), "both is ambiguous");
    assert!(McpServer::parse("dotted.name", "command = \"a\"\n").is_none(), "a dot would break mcp.<server>.<tool> routing");
}

// ── the http transport ──────────────────────────────────────────────────────

struct ScriptedHttp {
    replies: std::sync::Mutex<VecDeque<platform::transport::HttpReply>>,
    seen: std::sync::Mutex<Vec<(String, Vec<(String, String)>, String)>>,
}

impl ScriptedHttp {
    fn new(replies: Vec<platform::transport::HttpReply>) -> ScriptedHttp {
        ScriptedHttp { replies: std::sync::Mutex::new(replies.into()), seen: std::sync::Mutex::new(Vec::new()) }
    }
}

impl platform::transport::HttpExchange for ScriptedHttp {
    fn post(&self, url: &str, headers: &[(String, String)], body: &str) -> Result<platform::transport::HttpReply, String> {
        self.seen.lock().unwrap().push((url.to_string(), headers.to_vec(), body.to_string()));
        self.replies.lock().unwrap().pop_front().ok_or_else(|| "no reply scripted".to_string())
    }
}

#[test]
fn http_sends_the_routing_headers_and_replays_a_session() {
    let json = |body: &str, session: Option<&str>| platform::transport::HttpReply {
        status: 200,
        headers: session
            .map(|s| vec![("content-type".into(), "application/json".into()), ("mcp-session-id".into(), s.into())])
            .unwrap_or_else(|| vec![("content-type".into(), "application/json".into())]),
        body: body.into(),
    };
    let exchange = std::sync::Arc::new(ScriptedHttp::new(vec![
        json("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}", Some("sess-7")),
        json("{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}", None),
    ]));
    struct Shared(std::sync::Arc<ScriptedHttp>);
    impl platform::transport::HttpExchange for Shared {
        fn post(&self, u: &str, h: &[(String, String)], b: &str) -> Result<platform::transport::HttpReply, String> {
            self.0.post(u, h, b)
        }
    }
    let mut t = http::HttpTransport::new("https://x/mcp", vec![("Authorization".into(), "Bearer k".into())], Box::new(Shared(exchange.clone())));
    t.send("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"search\"}}").unwrap();
    assert!(t.recv(T).unwrap().unwrap().contains("\"ok\":true"));
    t.send("{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}").unwrap();

    let seen = exchange.seen.lock().unwrap();
    let has = |i: usize, k: &str, v: &str| seen[i].1.iter().any(|(hk, hv)| hk == k && hv == v);
    assert!(has(0, "Mcp-Method", "tools/call"));
    assert!(has(0, "Mcp-Name", "search"), "the 2026-07-28 routing header for named calls");
    assert!(has(0, "Authorization", "Bearer k"));
    assert!(has(0, "MCP-Protocol-Version", "2026-07-28"));
    assert!(has(1, "Mcp-Session-Id", "sess-7"), "the minted session must be replayed: {:?}", seen[1].1);
}

#[test]
fn an_http_error_without_jsonrpc_body_becomes_a_legacy_signal() {
    let reply = platform::transport::HttpReply { status: 400, headers: vec![("content-type".into(), "text/plain".into())], body: "Bad Request".into() };
    let mut t = http::HttpTransport::new("https://x/mcp", vec![], Box::new(ScriptedHttp::new(vec![reply])));
    t.send("{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"server/discover\",\"params\":{}}").unwrap();
    let line = t.recv(T).unwrap().unwrap();
    // A synthesized implementation-range error: era negotiation reads "not a
    // recognised modern error" and falls back to initialize — the spec's HTTP rule.
    assert!(line.contains("\"id\":5") && line.contains("-32000") && line.contains("http 400"), "{line}");
    match wire::classify(&line) {
        wire::Incoming::Reply { id: 5, result: Err(e) } => assert_ne!(e.code, -32022),
        other => panic!("{other:?}"),
    }
}

// ── the real subprocess, end to end ─────────────────────────────────────────

/// A tiny POSIX-sh legacy server: refuses `server/discover`, answers `initialize`
/// → `tools/list` → `tools/call` in order. Validates `StdioTransport`'s spawn +
/// reader thread + the era fallback + the hub's qualified routing + shutdown.
#[cfg(unix)]
#[test]
fn real_subprocess_handshake_and_call() {
    let dir = std::env::temp_dir().join(format!("tt-mcp-srv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("server.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
             read a\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"method not found\"}}\\n'\n\
             read b\nprintf '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"tiny\",\"version\":\"1\"}}}\\n'\n\
             read c\nread d\nprintf '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"tools\":[{\"name\":\"ping\",\"description\":\"pong\",\"inputSchema\":{\"type\":\"object\"}}]}}\\n'\n\
             read e\nprintf '{\"jsonrpc\":\"2.0\",\"id\":4,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"pong!\"}]}}\\n'\n",
    )
    .unwrap();
    let server = McpServer {
        name: "mock".into(),
        reach: Reach::Stdio { command: "sh".into(), args: vec![script.display().to_string()], env: vec![] },
        timeout_s: 10,
    };
    let mut hub = McpHub::launch(&[server]);
    let tools = hub.tools();
    assert!(tools.iter().any(|(n, d)| n == "mcp.mock.ping" && d.contains("args:")), "tools: {tools:?}");
    assert_eq!(hub.call("mcp.mock.ping", Json::Obj(vec![])).unwrap(), "pong!");
    let report = hub.report();
    assert_eq!(report[0].era, "legacy 2025-06-18");
    let _ = std::fs::remove_dir_all(&dir);
}
