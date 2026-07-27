//! The Telegram Bot API seam: the trait a gate talks to, and the pure codecs.
//!
//! Everything that can be decided from bytes alone lives here, so the whole protocol
//! — including every failure mode that matters (a bad token, a rate limit, a garbled
//! response) — is unit-tested without touching the network.

use corelib::wire::Json;

/// What an update actually carries.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// A text message the user typed.
    Text(String),
    /// A button on one of our messages was tapped. `id` must be acknowledged quickly
    /// or the sender's client shows a spinner; `message_id` is the message the button
    /// belongs to.
    Callback { id: String, data: String, message_id: i64 },
    /// Something we do not act on — a sticker, a join, a future update type.
    ///
    /// It is kept rather than dropped **on purpose**: the poller advances its offset
    /// from the updates it receives, so anything discarded here is redelivered forever
    /// in a tight loop. Carrying an inert value costs nothing and makes that whole
    /// class of bug impossible.
    Other,
}

/// One inbound update.
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub update_id: i64,
    pub chat_id: i64,
    pub from_id: i64,
    /// The sender's display name, for the local echo line.
    pub from_name: String,
    pub kind: Kind,
}

impl Update {
    /// The text of a typed message, if that is what this is.
    pub fn text(&self) -> Option<&str> {
        match &self.kind {
            Kind::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// Rows of `(label, callback data)` — an inline keyboard attached to a message.
///
/// Buttons live on the message itself, so tapping one does not add anything to the
/// conversation: the live screen stays at the bottom of the chat and keeps updating in
/// place, which is the whole point.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keyboard(pub Vec<Vec<(String, String)>>);

/// Telegram's hard limit on `callback_data`.
pub const MAX_CALLBACK_DATA: usize = 64;

impl Keyboard {
    pub fn new() -> Keyboard {
        Keyboard(Vec::new())
    }

    /// Add a row. Buttons whose data exceeds [`MAX_CALLBACK_DATA`] are dropped here
    /// rather than becoming a 400 from the API at send time.
    pub fn row<I: IntoIterator<Item = (String, String)>>(mut self, buttons: I) -> Keyboard {
        let row: Vec<(String, String)> =
            buttons.into_iter().filter(|(_, d)| !d.is_empty() && d.len() <= MAX_CALLBACK_DATA).collect();
        if !row.is_empty() {
            self.0.push(row);
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn json(&self) -> Json {
        let rows = self
            .0
            .iter()
            .map(|row| {
                Json::Arr(
                    row.iter()
                        .map(|(text, data)| {
                            Json::Obj(vec![
                                ("text".into(), Json::Str(text.clone())),
                                ("callback_data".into(), Json::Str(data.clone())),
                            ])
                        })
                        .collect(),
                )
            })
            .collect();
        Json::Obj(vec![("inline_keyboard".into(), Json::Arr(rows))])
    }
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
    /// Send an HTML-formatted message, optionally with buttons. Returns the message
    /// id, which is what makes editing it later possible.
    fn send_message(&self, chat_id: i64, html: &str, keys: Option<&Keyboard>) -> Result<i64, ApiError>;
    /// Replace the text and buttons of a message we already sent.
    fn edit_message(&self, chat_id: i64, message_id: i64, html: &str, keys: Option<&Keyboard>) -> Result<(), ApiError>;
    /// Acknowledge a button tap so the sender's client stops spinning.
    fn answer_callback(&self, id: &str) -> Result<(), ApiError>;
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

/// Decode a send response into the new message's id.
pub fn decode_sent(status: u16, body: &str) -> Result<i64, ApiError> {
    let doc = decode_envelope(status, body)?;
    doc.get("result")
        .and_then(|r| r.get("message_id"))
        .and_then(|v| v.as_f64())
        .map(|n| n as i64)
        .ok_or_else(|| ApiError::Transport("send succeeded but returned no message id".into()))
}

/// Decode an `editMessageText` response.
///
/// Editing a message to exactly what it already says is a 400 from Telegram, and it is
/// **not a failure**: the live screen simply had nothing new to show. Treating it as an
/// error would spam the pane with warnings during every quiet moment.
pub fn decode_edit(status: u16, body: &str) -> Result<(), ApiError> {
    match decode_ack(status, body) {
        Err(ApiError::Request { description, .. }) if description.contains("not modified") => Ok(()),
        other => other,
    }
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

/// Classify one update.
///
/// Anything with an `update_id` decodes — a shape we do not act on becomes
/// [`Kind::Other`] rather than vanishing, because a dropped update never advances the
/// poll offset and is therefore redelivered immediately, forever.
fn update_from(v: &Json) -> Option<Update> {
    // `update_id` and chat ids are well inside f64's exact integer range.
    let update_id = v.get("update_id").and_then(|v| v.as_f64())? as i64;
    let inert = Update { update_id, chat_id: 0, from_id: 0, from_name: String::new(), kind: Kind::Other };

    let name_of = |from: Option<&Json>| {
        from.and_then(|f| f.get("first_name").or_else(|| f.get("username")))
            .and_then(|v| v.as_str())
            .unwrap_or("someone")
            .to_string()
    };
    let id_of = |from: Option<&Json>, fallback: i64| {
        from.and_then(|f| f.get("id")).and_then(|v| v.as_f64()).map(|n| n as i64).unwrap_or(fallback)
    };

    if let Some(msg) = v.get("message") {
        let Some(chat_id) = msg.get("chat").and_then(|c| c.get("id")).and_then(|v| v.as_f64()).map(|n| n as i64)
        else {
            return Some(inert);
        };
        let Some(text) = msg.get("text").and_then(|v| v.as_str()) else {
            return Some(inert); // a sticker, a photo — nothing to run
        };
        let from = msg.get("from");
        return Some(Update {
            update_id,
            chat_id,
            from_id: id_of(from, chat_id),
            from_name: name_of(from),
            kind: Kind::Text(text.trim().to_string()),
        });
    }

    if let Some(cb) = v.get("callback_query") {
        let chat_id = cb
            .get("message")
            .and_then(|m| m.get("chat"))
            .and_then(|c| c.get("id"))
            .and_then(|v| v.as_f64())
            .map(|n| n as i64);
        let message_id =
            cb.get("message").and_then(|m| m.get("message_id")).and_then(|v| v.as_f64()).map(|n| n as i64);
        let id = cb.get("id").and_then(|v| v.as_str());
        let data = cb.get("data").and_then(|v| v.as_str());
        if let (Some(chat_id), Some(message_id), Some(id), Some(data)) = (chat_id, message_id, id, data) {
            let from = cb.get("from");
            return Some(Update {
                update_id,
                chat_id,
                from_id: id_of(from, chat_id),
                from_name: name_of(from),
                kind: Kind::Callback { id: id.to_string(), data: data.to_string(), message_id },
            });
        }
        return Some(inert);
    }

    Some(inert)
}

/// The JSON body for `sendMessage`.
pub fn message_body(chat_id: i64, html: &str, keys: Option<&Keyboard>) -> String {
    let mut fields = vec![
        ("chat_id".to_string(), Json::Num(chat_id as f64)),
        ("text".to_string(), Json::Str(html.to_string())),
        ("parse_mode".to_string(), Json::Str("HTML".into())),
        // Link previews turn a path or URL in command output into a giant card.
        ("disable_web_page_preview".to_string(), Json::Bool(true)),
    ];
    if let Some(k) = keys.filter(|k| !k.is_empty()) {
        fields.push(("reply_markup".to_string(), k.json()));
    }
    Json::Obj(fields).to_string()
}

/// The JSON body for `editMessageText`.
pub fn edit_body(chat_id: i64, message_id: i64, html: &str, keys: Option<&Keyboard>) -> String {
    let mut fields = vec![
        ("chat_id".to_string(), Json::Num(chat_id as f64)),
        ("message_id".to_string(), Json::Num(message_id as f64)),
        ("text".to_string(), Json::Str(html.to_string())),
        ("parse_mode".to_string(), Json::Str("HTML".into())),
        ("disable_web_page_preview".to_string(), Json::Bool(true)),
    ];
    // Always send the markup, even when empty: omitting it would LEAVE the previous
    // buttons in place, so stale choices would linger after the question changed.
    fields.push(("reply_markup".to_string(), keys.map(|k| k.json()).unwrap_or_else(|| Keyboard::new().json())));
    Json::Obj(fields).to_string()
}

/// The JSON body for `answerCallbackQuery`.
pub fn answer_body(id: &str) -> String {
    Json::Obj(vec![("callback_query_id".into(), Json::Str(id.to_string()))]).to_string()
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
                kind: Kind::Text("git status".into()),
            }]
        );
        assert_eq!(u[0].text(), Some("git status"));
    }

    #[test]
    fn a_button_tap_decodes_with_everything_needed_to_answer_and_edit() {
        let body = r#"{"ok":true,"result":[{"update_id":42,"callback_query":{
            "id":"399","from":{"id":7,"first_name":"Mourad"},"data":"k:1",
            "message":{"message_id":1234,"chat":{"id":51234903}}}}]}"#;
        let u = decode_updates(200, body).unwrap();
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].chat_id, 51234903);
        assert_eq!(u[0].from_id, 7);
        assert_eq!(u[0].kind, Kind::Callback { id: "399".into(), data: "k:1".into(), message_id: 1234 });
        assert_eq!(u[0].text(), None);
    }

    #[test]
    fn an_update_we_do_not_act_on_is_still_returned() {
        // THE offset bug: `poll` advances from the ids it receives, so a dropped update
        // is redelivered immediately, forever, in a tight loop. Everything decodes.
        let body = r#"{"ok":true,"result":[
            {"update_id":1,"edited_message":{"chat":{"id":1},"text":"x"}},
            {"update_id":2,"message":{"chat":{"id":1},"sticker":{}}},
            {"update_id":3,"my_chat_member":{}},
            {"update_id":4,"message":{"chat":{"id":9},"from":{"id":9,"first_name":"A"},"text":"ls"}}]}"#;
        let u = decode_updates(200, body).unwrap();
        assert_eq!(u.len(), 4, "every update must count toward the offset");
        assert_eq!(u.iter().map(|x| x.update_id).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert!(matches!(u[0].kind, Kind::Other));
        assert!(matches!(u[1].kind, Kind::Other));
        assert!(matches!(u[2].kind, Kind::Other));
        assert_eq!(u[3].text(), Some("ls"));
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
    fn editing_a_message_to_what_it_already_says_is_not_a_failure() {
        // The live screen edits on a timer; a quiet moment must not print a warning.
        let body = r#"{"ok":false,"error_code":400,"description":"Bad Request: message is not modified"}"#;
        assert_eq!(decode_edit(400, body), Ok(()));
        // A real rejection still surfaces.
        let bad = r#"{"ok":false,"error_code":400,"description":"Bad Request: can't parse entities"}"#;
        assert!(matches!(decode_edit(400, bad), Err(ApiError::Request { .. })));
    }

    #[test]
    fn a_send_hands_back_the_id_needed_to_edit_it() {
        let body = r#"{"ok":true,"result":{"message_id":1234,"chat":{"id":7},"text":"hi"}}"#;
        assert_eq!(decode_sent(200, body).unwrap(), 1234);
        // A success with no id is a transport oddity, not a silent zero.
        assert!(decode_sent(200, r#"{"ok":true,"result":{}}"#).is_err());
    }

    #[test]
    fn a_keyboard_becomes_nested_inline_rows() {
        let kb = Keyboard::new()
            .row([("1 · Yes".to_string(), "k:1".to_string()), ("2 · No".to_string(), "k:2".to_string())])
            .row([("⏎".to_string(), "k:enter".to_string())]);
        let b = message_body(7, "hi", Some(&kb));
        assert!(b.contains(r#""inline_keyboard":[["#), "{b}");
        assert!(b.contains(r#""callback_data":"k:1""#), "{b}");
        assert!(b.contains(r#""text":"⏎""#), "{b}");
    }

    #[test]
    fn a_button_whose_data_exceeds_the_api_limit_is_dropped_at_build_time() {
        // Better than a 400 at send time that loses the whole message.
        let kb = Keyboard::new().row([("ok".to_string(), "x".repeat(MAX_CALLBACK_DATA + 1))]);
        assert!(kb.is_empty(), "an over-long callback payload must not reach the API");
        let fine = Keyboard::new().row([("ok".to_string(), "x".repeat(MAX_CALLBACK_DATA))]);
        assert!(!fine.is_empty());
    }

    #[test]
    fn an_edit_always_sends_its_markup_so_stale_buttons_cannot_linger() {
        // Omitting reply_markup LEAVES the old keyboard in place — the previous
        // question's answers would stay tappable under a new screen.
        let b = edit_body(7, 99, "<pre>x</pre>", None);
        assert!(b.contains(r#""reply_markup":{"inline_keyboard":[]}"#), "{b}");
        assert!(b.contains(r#""message_id":99"#), "{b}");
    }

    #[test]
    fn the_message_body_requests_html_and_no_link_preview() {
        let b = message_body(42, "<pre>hi</pre>", None);
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
