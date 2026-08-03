//! The Streamable HTTP transport: each JSON-RPC message is one POST; the response
//! body is the reply — as plain JSON or as an SSE stream, the server's choice.
//!
//! It presents the same [`McpTransport`] line interface as stdio, so the client
//! above it cannot tell a subprocess from a URL — payloads that arrive in one POST's
//! response are queued and handed out by `recv`, exactly as a reader thread hands
//! out stdout lines.
//!
//! Headers carry what stdio carries inline: `MCP-Protocol-Version` (kept in step
//! with the negotiated era via [`McpTransport::era_settled`]), the 2026-07-28
//! routing headers `Mcp-Method`/`Mcp-Name`, and — for a legacy server — the
//! `Mcp-Session-Id` its `initialize` response minted, replayed on every request
//! after. Authorization is whatever static headers the declaration resolved; the
//! spec makes interactive auth optional and this client leaves it out on purpose.

use std::collections::VecDeque;
use std::time::Duration;

use corelib::wire::Json;
use platform::transport::HttpExchange;

use super::wire::Era;
use super::McpTransport;

pub struct HttpTransport {
    url: String,
    /// Declaration headers (auth and the like), sent verbatim on every request.
    extra: Vec<(String, String)>,
    http: Box<dyn HttpExchange>,
    /// The protocol version header value — updated when the era settles.
    version: String,
    /// A legacy server's session, from its `initialize` response.
    session: Option<String>,
    /// Payloads already received but not yet asked for.
    pending: VecDeque<String>,
}

impl HttpTransport {
    pub(crate) fn new(url: &str, extra: Vec<(String, String)>, http: Box<dyn HttpExchange>) -> HttpTransport {
        HttpTransport {
            url: url.to_string(),
            extra,
            http,
            version: super::wire::MODERN.to_string(),
            session: None,
            pending: VecDeque::new(),
        }
    }

    fn headers(&self, method: &str, name: Option<&str>) -> Vec<(String, String)> {
        let mut h = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "application/json, text/event-stream".to_string()),
            ("MCP-Protocol-Version".to_string(), self.version.clone()),
            ("Mcp-Method".to_string(), method.to_string()),
        ];
        if let Some(name) = name {
            h.push(("Mcp-Name".to_string(), name.to_string()));
        }
        if let Some(session) = &self.session {
            h.push(("Mcp-Session-Id".to_string(), session.clone()));
        }
        h.extend(self.extra.iter().cloned());
        h
    }
}

impl McpTransport for HttpTransport {
    fn send(&mut self, line: &str) -> Result<(), String> {
        let msg = Json::parse(line).map_err(|e| format!("mcp http: unsendable message: {e}"))?;
        let method = msg.get("method").and_then(Json::as_str).unwrap_or("").to_string();
        let name = msg.get("params").and_then(|p| p.get("name")).and_then(Json::as_str).map(str::to_string);
        let id = msg.get("id").cloned();
        let reply = self.http.post(&self.url, &self.headers(&method, name.as_deref()), line)?;
        if let Some(session) = reply.header("mcp-session-id") {
            self.session = Some(session.to_string());
        }
        let payloads = reply.payloads();
        // An error status whose body is not JSON-RPC still must reach the pending
        // request, or it would sit out its whole timeout to learn what the status
        // line already said. A synthesized error carries it — with an
        // implementation-range code, so era negotiation reads it as "not a modern
        // error" and falls back, which is the 2026-07-28 rule for exactly this reply.
        let jsonrpc = payloads.iter().any(|p| p.contains("\"jsonrpc\""));
        if reply.status >= 400 && !jsonrpc {
            if let Some(id) = id {
                self.pending.push_back(super::wire::respond_err(&id, -32000, &format!("http {}", reply.status)));
            }
            return Ok(());
        }
        self.pending.extend(payloads);
        Ok(())
    }

    fn recv(&mut self, _timeout: Duration) -> Result<Option<String>, String> {
        // Replies arrive with the POST that asked; by `recv` they are already here.
        Ok(self.pending.pop_front())
    }

    fn era_settled(&mut self, era: &Era) {
        self.version = era.version().to_string();
    }
}
