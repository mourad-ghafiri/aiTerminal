//! The Telegram gateway: long-poll in, paced messages out.

pub mod api;
pub mod curl;
pub mod mock;

pub use api::{ApiError, BotApi, FileKind, Keyboard, Kind, Update};
pub use curl::CurlBotApi;
pub use mock::MockBotApi;

/// How long the server holds a poll open with nothing to say. Long enough that an
/// idle gate is nearly free, short enough that a dead connection is noticed.
pub const POLL_TIMEOUT_S: u32 = 25;
/// Minimum spacing between messages to one chat. Telegram allows about one per
/// second per chat; a three-part reply sent instantly earns a 429.
pub const MIN_SEND_GAP_MS: u64 = 1_100;
/// Consecutive transport failures before the local pane is told the link is down.
/// One blip should not print anything; a real outage should say so exactly once.
const DOWN_AFTER: u32 = 5;
/// Backoff ceiling.
const MAX_BACKOFF_MS: u64 = 30_000;
/// Attempts to establish the starting offset before refusing to start.
const PRIME_ATTEMPTS: u32 = 3;

/// What the polling loop should do next.
#[derive(Debug, PartialEq)]
pub enum PollStep {
    /// Deliver these, then poll again immediately.
    Updates(Vec<Update>),
    /// Sleep this long, then poll again.
    Wait(u64),
    /// Tell the user the link is struggling (once), then keep retrying.
    Down(String),
    /// Unrecoverable — stop polling. The shell keeps running regardless.
    Stop(String),
}

/// Long-poll bookkeeping: which updates we have seen, and how hard to back off.
///
/// The retry policy lives here rather than in the client so it can be exercised
/// against [`MockBotApi`] without a network.
pub struct Poller {
    offset: i64,
    fails: u32,
    reported_down: bool,
    timeout_s: u32,
}

impl Default for Poller {
    fn default() -> Self {
        Self::new(POLL_TIMEOUT_S)
    }
}

impl Poller {
    pub fn new(timeout_s: u32) -> Self {
        Poller { offset: 0, fails: 0, reported_down: false, timeout_s }
    }

    pub fn offset(&self) -> i64 {
        self.offset
    }

    /// Establish the starting point by acknowledging everything already queued.
    ///
    /// **This is a safety requirement, not an optimization.** Telegram holds
    /// undelivered messages for 24 hours. Without priming, starting a gate would
    /// replay every command sent while it was off — including, eventually, one you
    /// did not want run unattended. On failure the gate refuses to start rather than
    /// guessing.
    pub fn prime(&mut self, api: &dyn BotApi, sleep: &mut dyn FnMut(u64)) -> Result<usize, ApiError> {
        let mut last_err = ApiError::Transport("no attempt made".into());
        for attempt in 0..PRIME_ATTEMPTS {
            // `offset = -1` asks for just the most recent update, whatever the backlog.
            match api.get_updates(-1, 0) {
                Ok(updates) => {
                    let discarded = updates.len();
                    if let Some(newest) = updates.iter().map(|u| u.update_id).max() {
                        self.offset = newest + 1;
                    }
                    return Ok(discarded);
                }
                Err(ApiError::Unauthorized) => return Err(ApiError::Unauthorized),
                Err(ApiError::Cancelled) => return Err(ApiError::Cancelled),
                Err(e) => {
                    last_err = e;
                    if attempt + 1 < PRIME_ATTEMPTS {
                        sleep(1_000 * (attempt as u64 + 1));
                    }
                }
            }
        }
        Err(last_err)
    }

    /// One poll cycle.
    pub fn poll(&mut self, api: &dyn BotApi) -> PollStep {
        match api.get_updates(self.offset, self.timeout_s) {
            Ok(updates) => {
                self.fails = 0;
                let recovered = std::mem::take(&mut self.reported_down);
                if let Some(newest) = updates.iter().map(|u| u.update_id).max() {
                    self.offset = newest + 1;
                }
                if updates.is_empty() && recovered {
                    return PollStep::Down("telegram: reconnected".into());
                }
                PollStep::Updates(updates)
            }
            Err(ApiError::Unauthorized) => {
                PollStep::Stop("telegram rejected the bot token — check [gates.telegram] token".into())
            }
            Err(ApiError::Cancelled) => PollStep::Stop(String::new()),
            Err(ApiError::RateLimited { retry_after }) => {
                // Deliberately does NOT advance the offset and does NOT count as a
                // failure: nothing was delivered, so those updates must come again.
                PollStep::Wait(retry_after as u64 * 1_000 + 1_000)
            }
            Err(e) => {
                self.fails += 1;
                if self.fails >= DOWN_AFTER && !self.reported_down {
                    self.reported_down = true;
                    return PollStep::Down(format!("telegram unreachable ({e}) — retrying"));
                }
                PollStep::Wait(backoff_ms(self.fails))
            }
        }
    }
}

/// Exponential backoff, capped. No jitter: there is exactly one client here, so
/// there is no thundering herd to spread out, and determinism makes it testable.
pub fn backoff_ms(fails: u32) -> u64 {
    let shift = fails.saturating_sub(1).min(20);
    1_000u64.saturating_mul(1u64 << shift).min(MAX_BACKOFF_MS)
}

/// How long to wait before the next outbound message so the per-chat rate limit is
/// respected. `last_ms` is when the previous message went out.
pub fn pace_ms(last_ms: Option<u64>, now_ms: u64) -> u64 {
    match last_ms {
        Some(last) => MIN_SEND_GAP_MS.saturating_sub(now_ms.saturating_sub(last)),
        None => 0,
    }
}

#[cfg(test)]
mod tests;
