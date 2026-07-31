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
mod tests;
