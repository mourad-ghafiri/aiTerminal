//! The stdio transport: a spawned subprocess, newline-delimited JSON-RPC on its
//! standard streams — the spec's canonical binding.
//!
//! Three duties beyond "pipe lines":
//!
//! 1. **Bounded reads.** A hostile server writing one endless line must die at
//!    [`MAX_LINE`], not in the allocator — the cap is applied *during* the read.
//! 2. **stderr is diagnostics, not garbage.** The spec says a server MAY log there
//!    and the client MAY capture it. The old transport nulled it, so a server that
//!    printed exactly why it could not start took the reason to its grave. The last
//!    [`STDERR_KEEP`] lines ride in a shared tail for `ai mcp` to show.
//! 3. **Shutdown is a sequence, not a `kill()`.** Spec order: close stdin, give the
//!    server a moment to exit on EOF, then terminate. Killing first is how servers
//!    with cleanup (lock files, child processes of their own) leave debris.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::McpTransport;

/// Max bytes for a single JSON-RPC line (a hostile server can't OOM us).
pub(crate) const MAX_LINE: usize = 4 * 1024 * 1024;
/// How many trailing stderr lines are kept for diagnostics.
const STDERR_KEEP: usize = 40;
/// How long a closing server gets to exit on EOF before it is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// A shared, bounded tail of a server's stderr.
#[derive(Clone, Default)]
pub struct StderrTail(Arc<Mutex<std::collections::VecDeque<String>>>);

impl StderrTail {
    fn push(&self, line: String) {
        let mut q = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() == STDERR_KEEP {
            q.pop_front();
        }
        q.push_back(line);
    }
    /// The captured tail, oldest first.
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
    }
}

/// Live transport: the spawned server's stdin + a reader thread draining stdout.
pub struct StdioTransport {
    child: Child,
    rx: Receiver<String>,
    /// `Option` so shutdown can close it (EOF is the graceful signal) before waiting.
    sink: Option<ChildStdin>,
    pub(crate) stderr: StderrTail,
}

impl StdioTransport {
    /// Spawn `command args…` with the given extra environment, stdio piped.
    pub(crate) fn spawn(name: &str, command: &str, args: &[String], env: &[(String, String)]) -> Result<StdioTransport, String> {
        let mut cmd = Command::new(command);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| format!("mcp '{name}': spawn failed: {e}"))?;
        let sink = child.stdin.take().ok_or("mcp: no stdin")?;
        let stdout = child.stdout.take().ok_or("mcp: no stdout")?;
        let stderr = StderrTail::default();
        if let Some(pipe) = child.stderr.take() {
            let tail = stderr.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    tail.push(line);
                }
            });
        }
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                // BOUND the read at MAX_LINE bytes (not after — `read_line` would grow
                // the buffer to gigabytes on a newline-less line first → OOM). A line
                // that hits the cap without a terminator is a protocol error: surface a
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
        Ok(StdioTransport { child, rx, sink: Some(sink), stderr })
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // The spec's shutdown: EOF on stdin first, a bounded wait, then force.
        drop(self.sink.take());
        let deadline = std::time::Instant::now() + SHUTDOWN_GRACE;
        while std::time::Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl McpTransport for StdioTransport {
    fn send(&mut self, line: &str) -> Result<(), String> {
        let sink = self.sink.as_mut().ok_or("mcp: connection is closed")?;
        sink.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        sink.write_all(b"\n").map_err(|e| e.to_string())?;
        sink.flush().map_err(|e| e.to_string())
    }
    fn recv(&mut self, timeout: Duration) -> Result<Option<String>, String> {
        match self.rx.recv_timeout(timeout) {
            Ok(line) => Ok(Some(line)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err("mcp: server closed the connection".into()),
        }
    }
    fn stderr_tail(&self) -> Vec<String> {
        self.stderr.lines()
    }
}
