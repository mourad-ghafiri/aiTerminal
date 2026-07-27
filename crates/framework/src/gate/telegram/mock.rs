//! An in-memory [`BotApi`] — scripted responses in, a record of everything sent out.
//!
//! Compiled always (not `#[cfg(test)]`), mirroring `platform::transport::MockTransport`,
//! so tests in any module can build a gate that talks to nothing.

use std::sync::Mutex;

use super::api::{ApiError, BotApi, FileKind, Keyboard, Kind, Update};

/// Something the gate sent.
#[derive(Clone, Debug, PartialEq)]
pub enum Sent {
    Message { chat_id: i64, html: String, keys: Option<Keyboard> },
    Edit { chat_id: i64, message_id: i64, html: String, keys: Option<Keyboard> },
    Answered(String),
    File { chat_id: i64, kind: FileKind, name: String, bytes: usize, caption: Option<String> },
    Commands(Vec<String>),
}

/// A scriptable bot backend.
pub struct MockBotApi {
    /// Queued `get_updates` results, consumed in order; an exhausted queue returns
    /// an empty poll forever (the real API's idle behaviour).
    replies: Mutex<std::collections::VecDeque<Result<Vec<Update>, ApiError>>>,
    pub sent: Mutex<Vec<Sent>>,
    /// Every `(offset, timeout)` the poller asked for.
    pub polls: Mutex<Vec<(i64, u32)>>,
    stopped: std::sync::atomic::AtomicBool,
    /// Ids handed back by `send_message`, so a test can assert an edit targeted the
    /// message that was actually sent.
    next_id: std::sync::atomic::AtomicI64,
}

impl Default for MockBotApi {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBotApi {
    pub fn new() -> Self {
        MockBotApi {
            replies: Mutex::new(std::collections::VecDeque::new()),
            sent: Mutex::new(Vec::new()),
            polls: Mutex::new(Vec::new()),
            stopped: std::sync::atomic::AtomicBool::new(false),
            next_id: std::sync::atomic::AtomicI64::new(1000),
        }
    }

    /// Queue one `get_updates` outcome.
    pub fn push(&self, reply: Result<Vec<Update>, ApiError>) -> &Self {
        self.replies.lock().unwrap_or_else(|e| e.into_inner()).push_back(reply);
        self
    }

    /// Queue a batch of text messages from one chat.
    pub fn push_texts(&self, chat_id: i64, first_update_id: i64, texts: &[&str]) -> &Self {
        let updates = texts
            .iter()
            .enumerate()
            .map(|(i, t)| Update {
                update_id: first_update_id + i as i64,
                chat_id,
                from_id: chat_id,
                from_name: "Tester".into(),
                kind: Kind::Text((*t).to_string()),
            })
            .collect();
        self.push(Ok(updates))
    }

    /// Queue a batch of button taps from one chat.
    pub fn push_taps(&self, chat_id: i64, first_update_id: i64, data: &[&str]) -> &Self {
        let updates = data
            .iter()
            .enumerate()
            .map(|(i, d)| Update {
                update_id: first_update_id + i as i64,
                chat_id,
                from_id: chat_id,
                from_name: "Tester".into(),
                kind: Kind::Callback { id: format!("cb{i}"), data: (*d).to_string(), message_id: 1000 },
            })
            .collect();
        self.push(Ok(updates))
    }

    pub fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The plain text of every message sent, for readable assertions.
    pub fn messages(&self) -> Vec<String> {
        self.sent()
            .into_iter()
            .filter_map(|s| match s {
                Sent::Message { html, .. } | Sent::Edit { html, .. } => Some(html),
                _ => None,
            })
            .collect()
    }

    pub fn polls(&self) -> Vec<(i64, u32)> {
        self.polls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl BotApi for MockBotApi {
    fn get_updates(&self, offset: i64, timeout_s: u32) -> Result<Vec<Update>, ApiError> {
        if self.stopped.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(ApiError::Cancelled);
        }
        self.polls.lock().unwrap_or_else(|e| e.into_inner()).push((offset, timeout_s));
        self.replies.lock().unwrap_or_else(|e| e.into_inner()).pop_front().unwrap_or(Ok(Vec::new()))
    }

    fn send_message(&self, chat_id: i64, html: &str, keys: Option<&Keyboard>) -> Result<i64, ApiError> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).push(Sent::Message {
            chat_id,
            html: html.to_string(),
            keys: keys.cloned(),
        });
        Ok(self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    fn edit_message(&self, chat_id: i64, message_id: i64, html: &str, keys: Option<&Keyboard>) -> Result<(), ApiError> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).push(Sent::Edit {
            chat_id,
            message_id,
            html: html.to_string(),
            keys: keys.cloned(),
        });
        Ok(())
    }

    fn answer_callback(&self, id: &str) -> Result<(), ApiError> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).push(Sent::Answered(id.to_string()));
        Ok(())
    }

    fn send_file(
        &self,
        chat_id: i64,
        kind: FileKind,
        name: &str,
        _mime: &str,
        bytes: &[u8],
        caption: Option<&str>,
    ) -> Result<(), ApiError> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).push(Sent::File {
            chat_id,
            kind,
            name: name.to_string(),
            bytes: bytes.len(),
            caption: caption.map(str::to_string),
        });
        Ok(())
    }

    fn set_commands(&self, commands: &[(&str, &str)]) -> Result<(), ApiError> {
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Sent::Commands(commands.iter().map(|(c, _)| c.to_string()).collect()));
        Ok(())
    }

    fn whoami(&self) -> Result<String, ApiError> {
        Ok("@mock_bot".into())
    }

    fn shutdown(&self) {
        self.stopped.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
