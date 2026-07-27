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
mod tests {
    use super::*;

    fn no_sleep() -> impl FnMut(u64) {
        |_| {}
    }

    #[test]
    fn priming_discards_a_backlog_so_a_restart_never_replays_old_commands() {
        // The dangerous case: messages sent while the gate was off must NOT execute
        // when it comes back up.
        let api = MockBotApi::new();
        api.push_texts(7, 900, &["rm -rf build", "deploy prod"]);
        let mut p = Poller::new(25);
        let mut s = no_sleep();
        assert_eq!(p.prime(&api, &mut s).unwrap(), 2);
        assert_eq!(p.offset(), 902, "offset is past the whole backlog");

        // So the first real poll asks only for what arrives from now on.
        api.push_texts(7, 902, &["ls"]);
        match p.poll(&api) {
            PollStep::Updates(u) => assert_eq!(u.iter().filter_map(|u| u.text()).collect::<Vec<_>>(), ["ls"]),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(api.polls(), vec![(-1, 0), (902, 25)], "primed with -1, then resumed from the backlog's end");
    }

    #[test]
    fn priming_refuses_to_start_when_the_backlog_cannot_be_established() {
        let api = MockBotApi::new();
        for _ in 0..PRIME_ATTEMPTS {
            api.push(Err(ApiError::Transport("dns".into())));
        }
        let mut p = Poller::new(25);
        let mut s = no_sleep();
        assert!(p.prime(&api, &mut s).is_err(), "starting blind risks replaying stale commands");
    }

    #[test]
    fn an_update_we_ignore_still_advances_the_offset() {
        // Without this the same update is redelivered instantly on every poll — a hot
        // loop that burns the rate limit and never makes progress.
        let api = MockBotApi::new();
        api.push(Ok(vec![Update {
            update_id: 500,
            chat_id: 0,
            from_id: 0,
            from_name: String::new(),
            kind: Kind::Other,
        }]));
        let mut p = Poller::new(25);
        assert!(matches!(p.poll(&api), PollStep::Updates(_)));
        assert_eq!(p.offset(), 501, "the ignored update was still acknowledged");
    }

    #[test]
    fn a_bad_token_stops_the_poller_immediately() {
        let api = MockBotApi::new();
        api.push(Err(ApiError::Unauthorized));
        let mut p = Poller::new(25);
        match p.poll(&api) {
            PollStep::Stop(m) => assert!(m.contains("token"), "{m}"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn the_offset_advances_past_delivered_updates_only() {
        let api = MockBotApi::new();
        api.push_texts(1, 10, &["a", "b", "c"]);
        let mut p = Poller::new(25);
        assert!(matches!(p.poll(&api), PollStep::Updates(u) if u.len() == 3));
        assert_eq!(p.offset(), 13);
    }

    #[test]
    fn a_rate_limit_waits_without_losing_the_undelivered_updates() {
        let api = MockBotApi::new();
        api.push(Err(ApiError::RateLimited { retry_after: 12 }));
        let mut p = Poller::new(25);
        p.offset = 500;
        assert_eq!(p.poll(&api), PollStep::Wait(13_000));
        assert_eq!(p.offset(), 500, "the offset must not advance — nothing was received");
    }

    #[test]
    fn transient_failures_back_off_and_report_the_outage_exactly_once() {
        let api = MockBotApi::new();
        for _ in 0..12 {
            api.push(Err(ApiError::Transport("network is down".into())));
        }
        let mut p = Poller::new(25);
        let mut waits = Vec::new();
        let mut downs = 0;
        for _ in 0..12 {
            match p.poll(&api) {
                PollStep::Wait(ms) => waits.push(ms),
                PollStep::Down(_) => downs += 1,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(downs, 1, "an outage is announced once, not every retry");
        assert!(waits.windows(2).all(|w| w[1] >= w[0]), "backoff must not shrink: {waits:?}");
        assert!(waits.iter().all(|&w| w <= MAX_BACKOFF_MS), "backoff is capped: {waits:?}");
    }

    #[test]
    fn recovery_after_an_outage_is_announced() {
        let api = MockBotApi::new();
        for _ in 0..DOWN_AFTER {
            api.push(Err(ApiError::Transport("down".into())));
        }
        let mut p = Poller::new(25);
        let mut said_down = false;
        for _ in 0..DOWN_AFTER {
            said_down |= matches!(p.poll(&api), PollStep::Down(_));
        }
        assert!(said_down);
        api.push(Ok(Vec::new()));
        match p.poll(&api) {
            PollStep::Down(m) => assert!(m.contains("reconnected"), "{m}"),
            other => panic!("expected a recovery note, got {other:?}"),
        }
    }

    #[test]
    fn backoff_grows_then_flattens_at_the_cap() {
        let seq: Vec<u64> = (1..=8).map(backoff_ms).collect();
        assert_eq!(seq[0], 1_000);
        assert!(seq.windows(2).all(|w| w[1] >= w[0]), "{seq:?}");
        assert!(seq.iter().all(|&v| v <= MAX_BACKOFF_MS), "{seq:?}");
    }

    #[test]
    fn outbound_messages_are_spaced_to_stay_under_the_chat_rate_limit() {
        assert_eq!(pace_ms(None, 5_000), 0, "the first message goes immediately");
        assert_eq!(pace_ms(Some(5_000), 5_000), MIN_SEND_GAP_MS, "back to back needs the full gap");
        assert_eq!(pace_ms(Some(5_000), 5_000 + MIN_SEND_GAP_MS), 0, "enough time already passed");
        assert_eq!(pace_ms(Some(5_000), 9_999), 0);
    }
}
