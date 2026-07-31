//! `platform-transport` — the egress seam: streaming HTTP + SSE framing.
//!
//! A [`Transport`] fires one streaming POST and yields decoded SSE [`Chunk`]s over
//! a channel. The default [`CurlTransport`] shells out to the system `curl` (the
//! same system-tool precedent as the `mds://` browser fetch); tests inject
//! [`MockTransport`] / [`ScriptedTransport`], so no network ever runs in CI.
//!
//! This crate is **AI-agnostic**: it line-frames the SSE stream into raw `data:`
//! payload strings ([`Chunk::Data`]) and reports stream end ([`Chunk::Done`]) or a
//! transport/HTTP error ([`Chunk::Error`]). Interpreting a payload (e.g. mapping
//! Anthropic's `message_stop`) is the caller's job, one layer up.
#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use corelib::wire::Json;

/// A cooperative cancellation flag shared between the caller and an in-flight
/// [`Transport::stream`]. Cloning shares the same underlying flag; setting it once
/// (`cancel`) is observed everywhere. The default is "not cancelled".
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        CancelToken::default()
    }
    /// Request cancellation — the in-flight request aborts (its process is killed).
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// One decoded item from a streaming response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chunk {
    /// One SSE `data:` payload (already de-framed; multi-line data joined by `\n`).
    Data(String),
    /// The stream closed cleanly (the process exited 0 with no error body).
    Done,
    /// A transport or HTTP-level error (curl failed, or the body carried a JSON
    /// `error.message`). Terminal.
    Error(String),
}

/// The receiving half of a streaming request.
pub struct StreamHandle {
    pub rx: Receiver<Chunk>,
}

/// Fires one streaming request. Implementations own a worker thread and push
/// chunks as they decode, so the caller never blocks.
pub trait Transport: Send + Sync {
    /// `headers` are sent verbatim (the caller supplies auth/content-type).
    /// `body` is written on the request stdin. Returns immediately. Setting `cancel`
    /// aborts the in-flight request (the streaming process is killed).
    fn stream(&self, url: &str, headers: &[(String, String)], body: &str, cancel: &CancelToken) -> StreamHandle;
}

/// A boxed transport is itself a [`Transport`] — so a caller can hold a runtime-selected
/// `Box<dyn Transport>` (e.g. a transport *factory* for delegated sub-agents) and still
/// satisfy `Client<T: Transport>`.
impl Transport for Box<dyn Transport> {
    fn stream(&self, url: &str, headers: &[(String, String)], body: &str, cancel: &CancelToken) -> StreamHandle {
        (**self).stream(url, headers, body, cancel)
    }
}

/// Incremental SSE parser. Feed it one newline-stripped line at a time; it returns
/// `Some(payload)` when a blank line dispatches the buffered `data:` field(s).
/// Generic: it does not parse the payload, only de-frames it.
#[derive(Default)]
pub struct SseDecoder {
    data: String,
}

/// One SSE event's maximum buffered payload — a stream that keeps sending
/// `data:` lines without ever dispatching (blank line) is broken or hostile and
/// must error out, not grow without bound.
const MAX_SSE_EVENT: usize = 8 * 1024 * 1024;

/// One raw stream line's maximum length. Matches the MCP transport's `MAX_LINE`
/// contract: an unterminated multi-gigabyte "line" from a broken server dies
/// here instead of in the allocator.
const MAX_SSE_LINE: u64 = 4 * 1024 * 1024;

/// How much of the raw stream head is retained for the trailing API-error sniff.
/// Error bodies are small JSON; the stream itself must never be kept whole.
const ERROR_SNIFF_CAP: usize = 64 * 1024;

impl SseDecoder {
    pub fn new() -> Self {
        SseDecoder::default()
    }

    /// Feed ONE line (already stripped of trailing `\r`/`\n`). A blank line
    /// dispatches the buffered payload (if any). `Err` when the buffered event
    /// exceeds [`MAX_SSE_EVENT`] — the stream must be aborted.
    pub fn push_line(&mut self, line: &str) -> Result<Option<String>, String> {
        if line.is_empty() {
            return Ok(self.dispatch());
        }
        if line.starts_with(':') {
            return Ok(None); // SSE comment / keep-alive
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if self.data.len().saturating_add(rest.len()) > MAX_SSE_EVENT {
                self.data.clear();
                return Err(format!("SSE event exceeded {} bytes — aborting the stream", MAX_SSE_EVENT));
            }
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(rest);
        }
        // `event:`, `id:`, `retry:` are ignored — the caller keys on the payload.
        Ok(None)
    }

    /// Flush any buffered (un-dispatched) payload at end of stream.
    pub fn finish(&mut self) -> Option<String> {
        self.dispatch()
    }

    fn dispatch(&mut self) -> Option<String> {
        if self.data.is_empty() {
            return None;
        }
        let data = std::mem::take(&mut self.data);
        let trimmed = data.trim();
        if trimmed.is_empty() || trimmed == "[DONE]" {
            return None;
        }
        Some(trimmed.to_string())
    }
}

/// Streams via the system `curl` (`-N --no-buffer`), writing the body on stdin so
/// the body never appears in the process argv.
pub struct CurlTransport {
    pub timeout_s: u32,
}

impl Default for CurlTransport {
    fn default() -> Self {
        CurlTransport { timeout_s: 120 }
    }
}

impl Transport for CurlTransport {
    fn stream(&self, url: &str, headers: &[(String, String)], body: &str, cancel: &CancelToken) -> StreamHandle {
        use std::io::{BufReader, Read, Write};
        use std::process::{Command, Stdio};
        use std::sync::Mutex;

        let (tx, rx) = channel();
        let mut cmd = Command::new("curl");
        cmd.args([
            "-sS",
            "-N",
            "--no-buffer",
            "--max-time",
            &self.timeout_s.to_string(),
            // Belt to the in-process line/event caps (chunked streams bypass it,
            // sized bodies die here) — mirrors caps/net.rs.
            "--max-filesize",
            "33554432",
            "-X",
            "POST",
            url,
            "--data-binary",
            "@-", // read the body from stdin
        ]);
        for (k, v) in headers {
            cmd.arg("-H").arg(format!("{k}: {v}"));
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Chunk::Error(format!("system curl unavailable: {e}")));
                return StreamHandle { rx };
            }
        };
        if let Some(mut sin) = child.stdin.take() {
            let _ = sin.write_all(body.as_bytes());
            // sin dropped here → curl sees EOF and proceeds
        }
        // Take the pipes out before sharing the child handle: the reader owns the pipes
        // (no lock needed to read), while a watcher owns a clone of the handle so it can
        // KILL the request the moment `cancel` flips — a true abort, not just a UI flag.
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr_pipe = child.stderr.take();
        let child = Arc::new(Mutex::new(child));

        // Watcher: kill the curl child on cancellation; exit once it has finished.
        {
            let child = Arc::clone(&child);
            let cancel = cancel.clone();
            std::thread::spawn(move || loop {
                if cancel.is_cancelled() {
                    if let Ok(mut c) = child.lock() {
                        let _ = c.kill();
                    }
                    return;
                }
                match child.lock().map(|mut c| c.try_wait()) {
                    Ok(Ok(Some(_))) | Ok(Err(_)) | Err(_) => return, // finished / gone
                    Ok(Ok(None)) => std::thread::sleep(std::time::Duration::from_millis(50)),
                }
            });
        }

        std::thread::spawn(move || {
            let pumped = pump_sse(BufReader::new(stdout), &tx, MAX_SSE_LINE);
            match pumped.end {
                PumpEnd::ReceiverGone => return, // dropping stdout EPIPEs curl
                PumpEnd::Overflow(msg) => {
                    // A byte bound tripped: kill curl NOW, reap it, report the abort.
                    if let Ok(mut c) = child.lock() {
                        let _ = c.kill();
                    }
                    loop {
                        match child.lock().map(|mut c| c.try_wait()) {
                            Ok(Ok(Some(_))) | Ok(Err(_)) | Err(_) => break,
                            Ok(Ok(None)) => std::thread::sleep(std::time::Duration::from_millis(2)),
                        }
                    }
                    let _ = tx.send(Chunk::Error(msg));
                    return;
                }
                PumpEnd::Eof => {}
            }

            // Reap the child WITHOUT holding the lock across a blocking wait (the watcher
            // needs the lock to kill) — poll `try_wait` instead. After stdout EOF the
            // process is already exiting (or was killed), so this settles immediately.
            let status = loop {
                match child.lock().map(|mut c| c.try_wait()) {
                    Ok(Ok(Some(s))) => break Some(s),
                    Ok(Err(_)) | Err(_) => break None,
                    Ok(Ok(None)) => std::thread::sleep(std::time::Duration::from_millis(2)),
                }
            };
            let mut stderr = String::new();
            if let Some(mut se) = stderr_pipe.take() {
                let _ = se.read_to_string(&mut stderr);
            }
            let sniff = String::from_utf8_lossy(&pumped.sniff);
            let api_err = Json::parse(sniff.trim()).ok().and_then(|j| {
                j.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Json::as_str)
                    .map(str::to_string)
            });
            let terminal = if let Some(msg) = api_err {
                Chunk::Error(msg)
            } else if matches!(status, Some(s) if !s.success()) {
                let m = stderr.trim();
                Chunk::Error(if m.is_empty() { "request failed".into() } else { m.into() })
            } else if pumped.saw_any {
                Chunk::Done
            } else {
                Chunk::Error("empty response from server".into())
            };
            let _ = tx.send(terminal);
        });

        StreamHandle { rx }
    }
}

/// Why [`pump_sse`] stopped reading.
enum PumpEnd {
    /// Clean end of stream.
    Eof,
    /// A byte bound tripped (the message explains which) — abort the transfer.
    Overflow(String),
    /// The consumer dropped its receiver — stop silently.
    ReceiverGone,
}

/// The result of draining one SSE stream.
struct Pumped {
    saw_any: bool,
    /// The retained stream head (≤ [`ERROR_SNIFF_CAP`] bytes) for error sniffing.
    sniff: Vec<u8>,
    end: PumpEnd,
}

/// Drain SSE lines from `reader` into `tx` with HARD byte bounds — separated from
/// the curl plumbing so tests drive it with an in-memory reader and a tiny cap.
/// Every line is read via `take(max_line)`, so a hostile unterminated body can
/// never balloon the line buffer; the decoder enforces its own event cap.
fn pump_sse(mut reader: impl std::io::BufRead, tx: &std::sync::mpsc::Sender<Chunk>, max_line: u64) -> Pumped {
    use std::io::{BufRead, Read};
    let mut dec = SseDecoder::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut saw_any = false;
    let mut sniff: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let n = match reader.by_ref().take(max_line).read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if n as u64 >= max_line && buf.last() != Some(&b'\n') {
            return Pumped {
                saw_any,
                sniff,
                end: PumpEnd::Overflow(format!("stream line exceeded {} bytes — aborting", max_line)),
            };
        }
        if sniff.len() < ERROR_SNIFF_CAP {
            let take = (ERROR_SNIFF_CAP - sniff.len()).min(buf.len());
            sniff.extend_from_slice(&buf[..take]);
        }
        let line = String::from_utf8_lossy(&buf);
        match dec.push_line(line.trim_end_matches(['\n', '\r'])) {
            Ok(Some(payload)) => {
                saw_any = true;
                if tx.send(Chunk::Data(payload)).is_err() {
                    return Pumped { saw_any, sniff, end: PumpEnd::ReceiverGone };
                }
            }
            Ok(None) => {}
            Err(msg) => return Pumped { saw_any, sniff, end: PumpEnd::Overflow(msg) },
        }
    }
    if let Some(payload) = dec.finish() {
        saw_any = true;
        let _ = tx.send(Chunk::Data(payload));
    }
    Pumped { saw_any, sniff, end: PumpEnd::Eof }
}

/// Blocking GET: fetch `url` and return the raw response body. Shells out to the
/// system `curl` (`-fsSL`). Replaces ad-hoc inline `curl` calls in higher layers.
pub fn fetch(url: &str) -> std::io::Result<Vec<u8>> {
    use std::process::Command;
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "30", "--max-filesize", "33554432", url])
        .output()?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("fetch failed: {}", msg.trim()),
        ));
    }
    Ok(out.stdout)
}

/// A transport that replays a recorded SSE string through the real decoder — for
/// headless tests, no network. Optionally asserts the request body.
pub struct MockTransport {
    sse: String,
    expect_body_contains: Vec<String>,
}

impl MockTransport {
    pub fn from_fixture(sse: impl Into<String>) -> Self {
        MockTransport { sse: sse.into(), expect_body_contains: Vec::new() }
    }
    /// Additionally assert that the request body contains each given substring.
    pub fn expecting(sse: impl Into<String>, must_contain: &[&str]) -> Self {
        MockTransport {
            sse: sse.into(),
            expect_body_contains: must_contain.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl Transport for MockTransport {
    fn stream(&self, _url: &str, _headers: &[(String, String)], body: &str, _cancel: &CancelToken) -> StreamHandle {
        for needle in &self.expect_body_contains {
            assert!(body.contains(needle.as_str()), "request body missing {needle:?}: {body}");
        }
        feed(&self.sse)
    }
}

/// A transport that returns a *different* scripted SSE per call (for testing a
/// multi-step loop). Past the last entry it repeats the last one.
pub struct ScriptedTransport {
    responses: Vec<String>,
    next: std::sync::atomic::AtomicUsize,
    /// Every body that was posted, in order.
    ///
    /// What a caller SENDS is half of what a harness does, and until now no test could
    /// see it — only the reply was scripted. A request carries the cache breakpoints,
    /// the message order, the tool catalogue: all things whose correctness is invisible
    /// in the answer that comes back.
    sent: std::sync::Mutex<Vec<String>>,
}

impl ScriptedTransport {
    pub fn new(responses: Vec<String>) -> Self {
        ScriptedTransport { responses, next: std::sync::atomic::AtomicUsize::new(0), sent: std::sync::Mutex::new(Vec::new()) }
    }

    /// The request bodies posted so far, oldest first.
    pub fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Transport for ScriptedTransport {
    fn stream(&self, _url: &str, _headers: &[(String, String)], body: &str, _cancel: &CancelToken) -> StreamHandle {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).push(body.to_string());
        let i = self.next.fetch_add(1, Ordering::SeqCst);
        let idx = i.min(self.responses.len().saturating_sub(1));
        let sse = self.responses.get(idx).cloned().unwrap_or_default();
        feed(&sse)
    }
}

/// De-frame a recorded SSE string into `Chunk::Data(payload)*` then a `Chunk::Done`.
fn feed(sse: &str) -> StreamHandle {
    let (tx, rx) = channel();
    let mut dec = SseDecoder::new();
    let mut saw_any = false;
    for raw in sse.split('\n') {
        match dec.push_line(raw.trim_end_matches('\r')) {
            Ok(Some(payload)) => {
                saw_any = true;
                let _ = tx.send(Chunk::Data(payload));
            }
            Ok(None) => {}
            Err(msg) => {
                let _ = tx.send(Chunk::Error(msg));
                return StreamHandle { rx };
            }
        }
    }
    if let Some(payload) = dec.finish() {
        saw_any = true;
        let _ = tx.send(Chunk::Data(payload));
    }
    let _ = tx.send(if saw_any { Chunk::Done } else { Chunk::Error("empty response from server".into()) });
    StreamHandle { rx }
}

#[cfg(test)]
mod tests;
