//! The Telegram Bot API seam: the trait a gate talks to, and the pure codecs.
//!
//! Everything that can be decided from bytes alone lives here, so the whole protocol
//! — including every failure mode that matters (a bad token, a rate limit, a garbled
//! response) — is unit-tested without touching the network.

use corelib::wire::Json;

/// One inbound message.
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub update_id: i64,
    pub chat_id: i64,
    pub from_id: i64,
    /// The sender's display name, for the local echo line.
    pub from_name: String,
    pub text: String,
}

/// How an attachment is delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// Byte-exact; the chat app stores what we sent.
    Document,
    /// Recompressed to JPEG by the chat app — smaller, but small glyphs smear.
    Photo,
}

impl FileKind {
    /// `(API method, multipart field name)`.
    pub fn parts(self) -> (&'static str, &'static str) {
        match self {
            FileKind::Document => ("sendDocument", "document"),
            FileKind::Photo => ("sendPhoto", "photo"),
        }
    }

    pub fn parse(s: &str) -> FileKind {
        if s.eq_ignore_ascii_case("photo") {
            FileKind::Photo
        } else {
            FileKind::Document
        }
    }
}

/// Why a call failed — the distinctions the poller acts on differently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiError {
    /// The token is wrong or revoked. Fatal for the gateway; retrying cannot help.
    Unauthorized,
    /// Too many requests; wait this long.
    RateLimited { retry_after: u32 },
    /// A malformed request — our bug, or output the API refused. Not retryable.
    Request { code: u16, description: String },
    /// Network, DNS, TLS, timeout, 5xx. Retry with backoff.
    Transport(String),
    /// We shut the gate down mid-call.
    Cancelled,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized => write!(f, "the bot token was rejected (401)"),
            ApiError::RateLimited { retry_after } => write!(f, "rate limited, retry in {retry_after}s"),
            ApiError::Request { code, description } => write!(f, "request rejected ({code}): {description}"),
            ApiError::Transport(m) => write!(f, "{m}"),
            ApiError::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// What a gate needs from a chat backend. Implemented by the real `curl` client and
/// by a mock, so the poller's retry logic is testable end to end.
pub trait BotApi: Send + Sync {
    /// Long-poll for messages. `timeout_s` is the *server-side* hold.
    fn get_updates(&self, offset: i64, timeout_s: u32) -> Result<Vec<Update>, ApiError>;
    /// Send an HTML-formatted message.
    fn send_message(&self, chat_id: i64, html: &str) -> Result<(), ApiError>;
    /// Upload a file with an optional caption.
    fn send_file(
        &self,
        chat_id: i64,
        kind: FileKind,
        name: &str,
        mime: &str,
        bytes: &[u8],
        caption: Option<&str>,
    ) -> Result<(), ApiError>;
    /// Publish the slash-command menu so the chat app offers it natively.
    fn set_commands(&self, commands: &[(&str, &str)]) -> Result<(), ApiError>;
    /// The bot's own @name, for the local banner.
    fn whoami(&self) -> Result<String, ApiError>;
    /// Abort any in-flight request and refuse further ones.
    fn shutdown(&self);
}

/// Map a non-success response to the error the poller should act on.
///
/// Telegram reports failures twice — in the HTTP status and in the body — and the
/// two do not always agree, so both are consulted.
pub fn decode_error(status: u16, body: &str) -> ApiError {
    let doc = Json::parse(body).ok();
    let code = doc
        .as_ref()
        .and_then(|d| d.get("error_code"))
        .and_then(|v| v.as_f64())
        .map(|n| n as u16)
        .unwrap_or(status);
    let description = doc
        .as_ref()
        .and_then(|d| d.get("description"))
        .and_then(|v| v.as_str())
        .unwrap_or("no description")
        .to_string();
    match code {
        401 | 403 => ApiError::Unauthorized,
        429 => {
            let retry_after = doc
                .as_ref()
                .and_then(|d| d.get("parameters"))
                .and_then(|p| p.get("retry_after"))
                .and_then(|v| v.as_f64())
                .map(|n| n.max(0.0) as u32)
                .unwrap_or(5);
            ApiError::RateLimited { retry_after }
        }
        // 5xx and anything unrecognized (a proxy's HTML error page, a truncated
        // body) are transient by assumption: retrying is safe, giving up is not.
        500..=599 | 0 => ApiError::Transport(format!("server error {code}: {description}")),
        _ => ApiError::Request { code, description },
    }
}

/// Decode a `getUpdates` response.
pub fn decode_updates(status: u16, body: &str) -> Result<Vec<Update>, ApiError> {
    let doc = decode_envelope(status, body)?;
    let Some(items) = doc.get("result").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    Ok(items.iter().filter_map(update_from).collect())
}

/// Decode a response whose payload we don't need, only its success.
pub fn decode_ack(status: u16, body: &str) -> Result<(), ApiError> {
    decode_envelope(status, body).map(|_| ())
}

/// Decode `getMe` into the bot's @name.
pub fn decode_whoami(status: u16, body: &str) -> Result<String, ApiError> {
    let doc = decode_envelope(status, body)?;
    Ok(doc
        .get("result")
        .and_then(|r| r.get("username"))
        .and_then(|v| v.as_str())
        .map(|u| format!("@{u}"))
        .unwrap_or_else(|| "the bot".to_string()))
}

fn decode_envelope(status: u16, body: &str) -> Result<Json, ApiError> {
    if !(200..300).contains(&status) {
        return Err(decode_error(status, body));
    }
    let doc = Json::parse(body).map_err(|e| ApiError::Transport(format!("unreadable response: {e}")))?;
    if doc.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return Err(decode_error(status, body));
    }
    Ok(doc)
}

/// Pull the fields we use out of one update, skipping anything that isn't a text
/// message (edits, joins, reactions — a gate has no use for them).
fn update_from(v: &Json) -> Option<Update> {
    // `update_id` and chat ids are well inside f64's exact integer range.
    let update_id = v.get("update_id").and_then(|v| v.as_f64())? as i64;
    let msg = v.get("message")?;
    let text = msg.get("text").and_then(|v| v.as_str())?.trim().to_string();
    let chat_id = msg.get("chat").and_then(|c| c.get("id")).and_then(|v| v.as_f64())? as i64;
    let from = msg.get("from");
    let from_id = from.and_then(|f| f.get("id")).and_then(|v| v.as_f64()).map(|n| n as i64).unwrap_or(chat_id);
    let from_name = from
        .and_then(|f| f.get("first_name").or_else(|| f.get("username")))
        .and_then(|v| v.as_str())
        .unwrap_or("someone")
        .to_string();
    Some(Update { update_id, chat_id, from_id, from_name, text })
}

/// The JSON body for `sendMessage`.
pub fn message_body(chat_id: i64, html: &str) -> String {
    Json::Obj(vec![
        ("chat_id".into(), Json::Num(chat_id as f64)),
        ("text".into(), Json::Str(html.to_string())),
        ("parse_mode".into(), Json::Str("HTML".into())),
        // Link previews turn a path or URL in command output into a giant card.
        ("disable_web_page_preview".into(), Json::Bool(true)),
    ])
    .to_string()
}

/// The JSON body for `setMyCommands`.
pub fn commands_body(commands: &[(&str, &str)]) -> String {
    let list = commands
        .iter()
        .map(|(c, d)| {
            Json::Obj(vec![
                ("command".into(), Json::Str(c.trim_start_matches('/').to_string())),
                ("description".into(), Json::Str(d.to_string())),
            ])
        })
        .collect();
    Json::Obj(vec![("commands".into(), Json::Arr(list))]).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK_UPDATE: &str = r#"{"ok":true,"result":[{"update_id":870001,"message":{"message_id":5,
        "from":{"id":51234903,"first_name":"Mourad","username":"mg"},
        "chat":{"id":51234903,"type":"private"},"date":1,"text":" git status "}}]}"#;

    #[test]
    fn a_text_message_decodes_into_an_update() {
        let u = decode_updates(200, OK_UPDATE).unwrap();
        assert_eq!(
            u,
            vec![Update {
                update_id: 870001,
                chat_id: 51234903,
                from_id: 51234903,
                from_name: "Mourad".into(),
                text: "git status".into(),
            }]
        );
    }

    #[test]
    fn updates_without_text_are_skipped_not_fatal() {
        // Joins, edits, stickers, reactions all arrive on the same stream.
        let body = r#"{"ok":true,"result":[
            {"update_id":1,"edited_message":{"chat":{"id":1},"text":"x"}},
            {"update_id":2,"message":{"chat":{"id":1},"sticker":{}}},
            {"update_id":3,"message":{"chat":{"id":9},"from":{"id":9,"first_name":"A"},"text":"ls"}}]}"#;
        let u = decode_updates(200, body).unwrap();
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].update_id, 3);
    }

    #[test]
    fn an_empty_result_is_the_normal_long_poll_timeout() {
        assert_eq!(decode_updates(200, r#"{"ok":true,"result":[]}"#).unwrap(), vec![]);
    }

    #[test]
    fn a_bad_token_is_fatal_not_retryable() {
        let body = r#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#;
        assert_eq!(decode_updates(401, body), Err(ApiError::Unauthorized));
    }

    #[test]
    fn a_rate_limit_carries_its_retry_delay() {
        let body = r#"{"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":17}}"#;
        assert_eq!(decode_updates(429, body), Err(ApiError::RateLimited { retry_after: 17 }));
    }

    #[test]
    fn a_rate_limit_without_a_delay_still_backs_off() {
        let body = r#"{"ok":false,"error_code":429,"description":"Too Many Requests"}"#;
        assert_eq!(decode_ack(429, body), Err(ApiError::RateLimited { retry_after: 5 }));
    }

    #[test]
    fn a_server_error_is_transient() {
        assert!(matches!(decode_ack(502, "<html>bad gateway</html>"), Err(ApiError::Transport(_))));
    }

    #[test]
    fn a_rejected_message_reports_why() {
        // The classic: unbalanced formatting tags. The caller retries unformatted.
        let body = r#"{"ok":false,"error_code":400,"description":"Bad Request: can't parse entities"}"#;
        match decode_ack(400, body) {
            Err(ApiError::Request { code, description }) => {
                assert_eq!(code, 400);
                assert!(description.contains("parse entities"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn garbage_never_panics_and_stays_retryable() {
        for body in ["", "not json", "{", r#"{"ok":true"#, "null", "[]"] {
            let r = decode_updates(200, body);
            assert!(r.is_err(), "{body:?} should not decode");
            assert!(!matches!(r, Err(ApiError::Unauthorized)), "garbage must not look like a bad token");
        }
    }

    #[test]
    fn an_ok_false_body_with_a_200_status_is_still_an_error() {
        // Some proxies rewrite the status; the envelope is the source of truth.
        let body = r#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#;
        assert_eq!(decode_ack(200, body), Err(ApiError::Unauthorized));
    }

    #[test]
    fn the_message_body_requests_html_and_no_link_preview() {
        let b = message_body(42, "<pre>hi</pre>");
        assert!(b.contains(r#""chat_id":42"#), "{b}");
        assert!(b.contains(r#""parse_mode":"HTML""#));
        assert!(b.contains(r#""disable_web_page_preview":true"#));
    }

    #[test]
    fn the_command_menu_body_strips_leading_slashes() {
        let b = commands_body(&[("/shot", "screenshot"), ("stop", "end the gate")]);
        assert!(b.contains(r#""command":"shot""#), "{b}");
        assert!(b.contains(r#""command":"stop""#), "{b}");
    }

    #[test]
    fn whoami_reads_the_bot_handle() {
        let b = r#"{"ok":true,"result":{"id":7,"is_bot":true,"username":"mourad_term_bot"}}"#;
        assert_eq!(decode_whoami(200, b).unwrap(), "@mourad_term_bot");
    }
}
