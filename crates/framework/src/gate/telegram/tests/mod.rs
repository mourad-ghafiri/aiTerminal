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
