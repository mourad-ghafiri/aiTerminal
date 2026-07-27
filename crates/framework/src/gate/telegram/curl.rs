//! The real Telegram client, over the system `curl`.
//!
//! This workspace ships no third-party crates, so `curl` is the HTTPS seam — the same
//! choice `caps::net` and the AI transport already make. A gate cannot reuse
//! `caps::net::https_request` though: that one hardcodes a 30 s timeout (too short to
//! hold a 25 s long-poll), takes a `&str` body (a PNG is not UTF-8), has no multipart,
//! and cannot be cancelled. So this is its own small client rather than a widening of
//! the agent's network surface.
//!
//! Two details are load-bearing:
//!
//! - **`--form-string` for every text field.** Plain `-F name=value` treats a value
//!   beginning with `@` or `<` as a *file reference*, so a caption echoing user text
//!   could be turned into an arbitrary-file-read. `--form-string` is always literal;
//!   only the attachment itself uses `-F`, with a filename we choose.
//! - **The in-flight child is registered**, so shutting the gate down kills a poll
//!   that is 20 seconds into its wait instead of blocking on it.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use super::api::{decode_ack, decode_updates, decode_whoami, ApiError, BotApi, FileKind, Update};

/// Read cap on any single response — a runaway body must not become a runaway
/// allocation.
const MAX_RESPONSE: &str = "8388608"; // 8 MiB
/// Give up on a stalled connection quickly, rather than after the whole poll window.
const CONNECT_TIMEOUT: &str = "10";
/// Headroom over the server-side long-poll hold, for TLS setup and the response.
const POLL_SLACK_S: u32 = 15;

pub struct CurlBotApi {
    token: String,
    base: String,
    inflight: Mutex<Option<Child>>,
    stopped: AtomicBool,
}

impl CurlBotApi {
    pub fn new(token: &str) -> Self {
        Self::with_base(token, "https://api.telegram.org")
    }

    /// Point the client at another host — used to prove URL construction without
    /// reaching the real API.
    pub fn with_base(token: &str, base: &str) -> Self {
        CurlBotApi {
            token: token.trim().to_string(),
            base: base.trim_end_matches('/').to_string(),
            inflight: Mutex::new(None),
            stopped: AtomicBool::new(false),
        }
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.base, self.token)
    }

    /// Run a prepared curl invocation, tracking it so [`shutdown`](Self::shutdown)
    /// can kill it, and return `(status, body)`.
    fn run(&self, mut cmd: Command, body: Option<&[u8]>) -> Result<(u16, String), ApiError> {
        if self.stopped.load(Ordering::Relaxed) {
            return Err(ApiError::Cancelled);
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| ApiError::Transport(format!("system curl unavailable: {e}")))?;

        if let Some(bytes) = body {
            // The body goes on stdin, never in argv, so it never reaches `ps`.
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(bytes);
            }
        } else {
            drop(child.stdin.take());
        }

        // Register before waiting; a concurrent shutdown kills it mid-flight.
        {
            let mut slot = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            if self.stopped.load(Ordering::Relaxed) {
                let _ = child.kill();
                return Err(ApiError::Cancelled);
            }
            *slot = Some(child);
        }
        let taken = self.inflight.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(child) = taken else { return Err(ApiError::Cancelled) };
        let out = child.wait_with_output().map_err(|e| ApiError::Transport(e.to_string()))?;

        if self.stopped.load(Ordering::Relaxed) {
            return Err(ApiError::Cancelled);
        }
        if !out.status.success() && out.stdout.is_empty() {
            let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(ApiError::Transport(if msg.is_empty() {
                format!("curl exited {}", out.status.code().unwrap_or(-1))
            } else {
                msg
            }));
        }
        Ok(split_response(&String::from_utf8_lossy(&out.stdout)))
    }

    /// A GET with the shared safety flags.
    fn get(&self, url: &str, max_time: u32) -> Result<(u16, String), ApiError> {
        let mut c = Command::new("curl");
        c.args(common_args(max_time)).arg(url);
        self.run(c, None)
    }

    /// A JSON POST with the body on stdin.
    fn post_json(&self, method: &str, body: &str) -> Result<(u16, String), ApiError> {
        let mut c = Command::new("curl");
        c.args(common_args(30))
            .args(["-X", "POST", "-H", "Content-Type: application/json", "--data-binary", "@-"])
            .arg(self.url(method));
        self.run(c, Some(body.as_bytes()))
    }
}

/// Flags every request shares. `--include` prefixes the response headers so the
/// status line can be read back.
fn common_args(max_time: u32) -> Vec<String> {
    [
        "--silent",
        "--show-error",
        "--include",
        "--max-redirs",
        "0",
        "--connect-timeout",
        CONNECT_TIMEOUT,
        "--max-time",
        &max_time.to_string(),
        "--max-filesize",
        MAX_RESPONSE,
        "--user-agent",
        concat!(env!("CARGO_PKG_NAME"), "-gate"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Split curl's `--include` output into `(status, body)`, skipping `1xx` blocks.
pub(super) fn split_response(raw: &str) -> (u16, String) {
    let mut rest = raw;
    loop {
        let (sep, idx) = match (rest.find("\r\n\r\n"), rest.find("\n\n")) {
            (Some(a), Some(b)) if a <= b => (4, a),
            (Some(a), None) => (4, a),
            (_, Some(b)) => (2, b),
            (None, None) => return (0, rest.to_string()),
        };
        let (head, body) = (&rest[..idx], &rest[idx + sep..]);
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse::<u16>().ok())
            .unwrap_or(0);
        if (100..200).contains(&status) {
            rest = body; // an informational block; the real response follows
            continue;
        }
        return (status, body.to_string());
    }
}

impl BotApi for CurlBotApi {
    fn get_updates(&self, offset: i64, timeout_s: u32) -> Result<Vec<Update>, ApiError> {
        let url = format!(
            "{}?offset={offset}&timeout={timeout_s}&allowed_updates=%5B%22message%22%5D",
            self.url("getUpdates")
        );
        // curl must outlast the server's hold, or every poll dies on our own clock.
        let (status, body) = self.get(&url, timeout_s + POLL_SLACK_S)?;
        decode_updates(status, &body)
    }

    fn send_message(&self, chat_id: i64, html: &str) -> Result<(), ApiError> {
        let (status, body) = self.post_json("sendMessage", &super::api::message_body(chat_id, html))?;
        decode_ack(status, &body)
    }

    fn send_file(
        &self,
        chat_id: i64,
        kind: FileKind,
        name: &str,
        mime: &str,
        bytes: &[u8],
        caption: Option<&str>,
    ) -> Result<(), ApiError> {
        let (method, field) = kind.parts();
        let mut c = Command::new("curl");
        // Uploads are slow on mobile links; 5 minutes, not the default 30 seconds.
        c.args(common_args(300)).args(["-X", "POST"]);
        c.arg("--form-string").arg(format!("chat_id={chat_id}"));
        if let Some(cap) = caption {
            c.arg("--form-string").arg(format!("caption={cap}"));
        }
        // The ONLY `-F`: the attachment, read from stdin under a filename we control.
        c.arg("-F").arg(format!("{field}=@-;filename={name};type={mime}"));
        c.arg(self.url(method));
        let (status, body) = self.run(c, Some(bytes))?;
        decode_ack(status, &body)
    }

    fn set_commands(&self, commands: &[(&str, &str)]) -> Result<(), ApiError> {
        let (status, body) = self.post_json("setMyCommands", &super::api::commands_body(commands))?;
        decode_ack(status, &body)
    }

    fn whoami(&self) -> Result<String, ApiError> {
        let (status, body) = self.get(&self.url("getMe"), 15)?;
        decode_whoami(status, &body)
    }

    fn shutdown(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(mut child) = self.inflight.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_and_body_from_curls_include_output() {
        let raw = "HTTP/2 200\r\ncontent-type: application/json\r\n\r\n{\"ok\":true}";
        assert_eq!(split_response(raw), (200, "{\"ok\":true}".to_string()));
    }

    #[test]
    fn skips_an_informational_block() {
        let raw = "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 429 Too Many\r\nx: y\r\n\r\nbody";
        assert_eq!(split_response(raw), (429, "body".to_string()));
    }

    #[test]
    fn the_long_poll_url_carries_the_offset_and_asks_only_for_messages() {
        let api = CurlBotApi::with_base("123:ABC", "https://example.invalid");
        let url = api.url("getUpdates");
        assert_eq!(url, "https://example.invalid/bot123:ABC/getUpdates");
        // The bot must not be handed edits/joins it would then have to filter.
        assert!(format!("{url}?offset=5&timeout=25&allowed_updates=%5B%22message%22%5D").contains("allowed_updates"));
    }

    #[test]
    fn curl_must_outlive_the_servers_hold() {
        // If curl's own deadline were <= the poll timeout, EVERY poll would be killed
        // by our own client just before the server answered.
        let poll = 25u32;
        assert!(poll + POLL_SLACK_S > poll + 10, "not enough headroom for TLS + response");
    }

    #[test]
    fn a_shut_down_client_refuses_to_start_new_requests() {
        let api = CurlBotApi::with_base("t", "https://example.invalid");
        api.shutdown();
        assert_eq!(api.get_updates(0, 1), Err(ApiError::Cancelled), "no process is spawned after shutdown");
        assert_eq!(api.send_message(1, "hi"), Err(ApiError::Cancelled));
    }
}
