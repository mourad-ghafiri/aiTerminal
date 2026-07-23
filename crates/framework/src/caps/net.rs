//! Minimal HTTP(S) GET (for `mds://` + the `net.get` capability).
//!
//! The architecture's end state fetches through the platform `Http` trait
//! (NSURLSession / WinHTTP / system libcurl). Until that seam is wired we shell
//! out to the system `curl` (a system tool, like `git` for the git plugin),
//! bounded by a hard timeout + size cap — preserving the zero-third-party-crate
//! invariant.

use std::process::Command;

/// Fetch `url` (http/https) as text, PINNED to a pre-vetted IP (`resolve` = `host:port:ip`
/// from the SSRF check), or an error message suitable for display.
pub fn https_get(url: &str, resolve: &str) -> Result<String, String> {
    https_get_bytes(url, resolve).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Fetch `url` (http/https) as raw bytes, pinned to the vetted IP.
pub fn https_get_bytes(url: &str, resolve: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("only http(s) is supported".into());
    }
    // `--resolve host:port:ip` pins the connection to the SSRF-vetted IP (no DNS
    // re-resolution → no rebinding), and we DO NOT follow redirects (`--max-redirs 0`): a
    // 30x to another host would re-resolve an unvetted target and escape the pin.
    let out = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--max-redirs",
            "0",
            "--resolve",
            resolve,
            "--fail",
            "--max-time",
            "15",
            "--max-filesize",
            "33554432", // 32 MiB cap
            "--user-agent",
            "aiTerminal/0.1",
            url,
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(o.stdout),
        Ok(o) => {
            let code = o.status.code().unwrap_or(-1);
            let msg = String::from_utf8_lossy(&o.stderr);
            let msg = msg.trim();
            if msg.is_empty() {
                Err(format!("fetch failed (curl exit {code})"))
            } else {
                Err(format!("fetch failed: {msg}"))
            }
        }
        Err(e) => Err(format!("system curl unavailable: {e}")),
    }
}

/// A general HTTP(S) request (the `http.*` family) PINNED to the SSRF-vetted IP. Sends
/// `method` + `headers` + optional `body` (on stdin), and returns `(status, headers,
/// body)`. No redirects (the pin), HTTPS-only enforced by the caller.
pub fn https_request(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    resolve: &str,
) -> Result<(u16, Vec<(String, String)>, String), String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut cmd = Command::new("curl");
    cmd.args(["--silent", "--show-error", "--include", "--max-redirs", "0", "--resolve", resolve, "--max-time", "30", "--max-filesize", "33554432", "--user-agent", "aiTerminal/0.1", "-X", method]);
    for (k, v) in headers {
        cmd.arg("-H").arg(format!("{k}: {v}"));
    }
    if body.is_some() {
        cmd.arg("--data-binary").arg("@-");
    }
    cmd.arg(url);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("system curl unavailable: {e}"))?;
    if let (Some(b), Some(mut stdin)) = (body, child.stdin.take()) {
        let _ = stdin.write_all(b.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() && out.stdout.is_empty() {
        let msg = String::from_utf8_lossy(&out.stderr);
        return Err(format!("request failed: {}", msg.trim()));
    }
    Ok(parse_response(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse curl `--include` output into `(status, headers, body)`, skipping any
/// informational `1xx` header blocks (e.g. `100 Continue`).
fn parse_response(raw: &str) -> (u16, Vec<(String, String)>, String) {
    let mut rest = raw;
    loop {
        // Header/body separator (tolerate both CRLF and LF servers).
        let (sep, idx) = match (rest.find("\r\n\r\n"), rest.find("\n\n")) {
            (Some(a), Some(b)) if a <= b => (4, a),
            (Some(a), None) => (4, a),
            (_, Some(b)) => (2, b),
            (None, None) => return (0, Vec::new(), rest.to_string()),
        };
        let head = &rest[..idx];
        let body = &rest[idx + sep..];
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse::<u16>().ok())
            .unwrap_or(0);
        if (100..200).contains(&status) {
            rest = body; // skip an informational block, parse the real response next
            continue;
        }
        let headers = head
            .lines()
            .skip(1)
            .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
            .collect();
        return (status, headers, body.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::parse_response;

    #[test]
    fn parses_status_headers_body() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-A: b\r\n\r\n{\"ok\":true}";
        let (status, headers, body) = parse_response(raw);
        assert_eq!(status, 200);
        assert_eq!(body, "{\"ok\":true}");
        assert!(headers.iter().any(|(k, v)| k == "Content-Type" && v == "application/json"));
    }

    #[test]
    fn skips_100_continue_block() {
        let raw = "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 201 Created\r\nLocation: /x\r\n\r\nbody";
        let (status, _h, body) = parse_response(raw);
        assert_eq!(status, 201);
        assert_eq!(body, "body");
    }
}
