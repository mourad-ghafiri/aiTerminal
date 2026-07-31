//! Who is allowed to drive this terminal.
//!
//! A chat bot accepts messages from **anyone who learns its @name**, so a chat id
//! written in a config file is an address, not a credential. The real authentication
//! is a one-time code printed in the local pane: whoever can see your screen can pair,
//! and nobody else. That is the whole security model, and it is deliberately simple
//! enough to explain in one sentence.
//!
//! Around it:
//!
//! - A gate serves **one** chat at a time. Once paired, a second chat is refused.
//! - Wrong codes are counted; after [`MAX_ATTEMPTS`] pairing is locked for the rest of
//!   the session, which bounds guessing at five tries out of a million.
//! - While unpaired the code **rotates** every [`CODE_TTL_MS`], so a gate left running
//!   overnight is not still advertising this morning's code.
//! - `allow` pre-authorizes known ids for people who would rather not pair each time.

/// How long an unpaired code stays valid before a fresh one is issued.
pub const CODE_TTL_MS: u64 = 10 * 60 * 1000;
/// Wrong codes tolerated before pairing is closed for this session.
pub const MAX_ATTEMPTS: u32 = 5;

/// The chat currently driving the terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub chat_id: i64,
    pub name: String,
}

/// The verdict on one inbound message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    /// Carry on and interpret the message.
    Allowed,
    /// This message paired the chat; announce it and do nothing else.
    JustPaired,
    /// Reply with this, then stop.
    Refused(String),
    /// Say nothing at all. Used for unknown chats, so a stranger who found the bot
    /// learns nothing — not even that the bot is live.
    Silent,
}

pub struct Auth {
    code: String,
    issued_ms: u64,
    require_pairing: bool,
    allow: Vec<String>,
    paired: Option<Peer>,
    attempts: u32,
}

impl Auth {
    /// `allow` holds pre-authorized chat ids as text.
    pub fn new(require_pairing: bool, allow: Vec<String>, now_ms: u64, code: String) -> Self {
        Auth { code, issued_ms: now_ms, require_pairing, allow, paired: None, attempts: 0 }
    }

    /// The code to display locally, formatted for reading aloud.
    pub fn display_code(&self) -> String {
        format!("{}-{}", &self.code[..3], &self.code[3..])
    }

    pub fn paired(&self) -> Option<&Peer> {
        self.paired.as_ref()
    }

    pub fn locked_out(&self) -> bool {
        self.attempts >= MAX_ATTEMPTS
    }

    /// Rotate the code if it has gone stale while nobody paired. Returns the new code
    /// to announce in the pane.
    pub fn tick(&mut self, now_ms: u64, fresh: impl FnOnce() -> String) -> Option<String> {
        if self.paired.is_some() || self.locked_out() || now_ms.saturating_sub(self.issued_ms) < CODE_TTL_MS {
            return None;
        }
        self.code = fresh();
        self.issued_ms = now_ms;
        Some(self.display_code())
    }

    /// Decide what to do with a message from `chat_id`.
    ///
    /// `pair_code` is `Some` when the message was `/pair <code>`.
    pub fn check(&mut self, chat_id: i64, name: &str, pair_code: Option<&str>) -> Access {
        // A pre-authorized id skips the handshake entirely.
        if self.allow.iter().any(|a| a == &chat_id.to_string()) {
            self.bind(chat_id, name);
            return Access::Allowed;
        }
        if !self.require_pairing {
            // Pairing off + an empty allow-list would mean "anyone with the bot name
            // owns this machine". `@gate` refuses to start in that state, so reaching
            // here means the id simply isn't on the list.
            return Access::Silent;
        }
        match &self.paired {
            Some(p) if p.chat_id == chat_id => Access::Allowed,
            // Someone else already has the shell. Say so rather than going silent —
            // this one is much more likely to be the owner's second device than an
            // attacker, and silence would look like a bug.
            Some(_) => Access::Refused("this terminal is already paired with another chat".into()),
            None => self.try_pair(chat_id, name, pair_code),
        }
    }

    fn try_pair(&mut self, chat_id: i64, name: &str, pair_code: Option<&str>) -> Access {
        if self.locked_out() {
            return Access::Silent;
        }
        let Some(given) = pair_code else {
            // Unpaired and not pairing: reveal nothing.
            return Access::Silent;
        };
        // Accept how people actually type it: `418-207`, `418 207`, `418207`.
        let given: String = given.chars().filter(char::is_ascii_digit).collect();
        if given == self.code {
            self.bind(chat_id, name);
            self.attempts = 0;
            return Access::JustPaired;
        }
        self.attempts += 1;
        if self.locked_out() {
            return Access::Refused("too many wrong codes — restart the gate to try again".into());
        }
        Access::Refused(format!("wrong code ({} of {MAX_ATTEMPTS} tries used)", self.attempts))
    }

    fn bind(&mut self, chat_id: i64, name: &str) {
        if self.paired.is_none() {
            self.paired = Some(Peer { chat_id, name: name.to_string() });
        }
    }
}

/// A fresh six-digit code from the OS entropy source, falling back to the clock only
/// if that is unavailable (never silently to a constant).
pub fn new_code() -> String {
    let mut buf = [0u8; 4];
    let n = if platform::os::random_bytes(&mut buf) {
        u32::from_le_bytes(buf)
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
            .wrapping_mul(2_654_435_761)
    };
    format!("{:06}", n % 1_000_000)
}

#[cfg(test)]
mod tests;
