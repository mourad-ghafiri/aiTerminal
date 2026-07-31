use super::*;

fn auth() -> Auth {
    Auth::new(true, Vec::new(), 0, "418207".into())
}

#[test]
fn nothing_is_accepted_before_pairing() {
    let mut a = auth();
    assert_eq!(a.check(7, "Stranger", None), Access::Silent, "an unpaired chat learns nothing");
    assert!(a.paired().is_none());
}

#[test]
fn the_right_code_pairs_the_chat_and_then_it_may_act() {
    let mut a = auth();
    assert_eq!(a.check(7, "Mourad", Some("418207")), Access::JustPaired);
    assert_eq!(a.paired(), Some(&Peer { chat_id: 7, name: "Mourad".into() }));
    assert_eq!(a.check(7, "Mourad", None), Access::Allowed);
}

#[test]
fn the_code_is_accepted_however_it_is_typed() {
    for typed in ["418207", "418-207", "418 207", " 418-207 "] {
        let mut a = auth();
        assert_eq!(a.check(7, "M", Some(typed)), Access::JustPaired, "{typed}");
    }
}

#[test]
fn guessing_is_bounded_and_then_closed_for_the_session() {
    let mut a = auth();
    for i in 1..MAX_ATTEMPTS {
        match a.check(7, "Guesser", Some("000000")) {
            Access::Refused(m) => assert!(m.contains(&i.to_string()), "{m}"),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(matches!(a.check(7, "Guesser", Some("000000")), Access::Refused(_)));
    assert!(a.locked_out());
    // Even the CORRECT code is refused once locked out — otherwise the limit
    // would only slow an attacker down rather than stop them.
    assert_eq!(a.check(7, "Guesser", Some("418207")), Access::Silent);
    assert!(a.paired().is_none());
}

#[test]
fn a_second_chat_cannot_take_over_a_paired_terminal() {
    let mut a = auth();
    a.check(7, "Owner", Some("418207"));
    match a.check(99, "Interloper", Some("418207")) {
        Access::Refused(m) => assert!(m.contains("already paired"), "{m}"),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(a.paired().unwrap().chat_id, 7);
}

#[test]
fn a_pre_authorized_chat_skips_the_handshake() {
    let mut a = Auth::new(true, vec!["51234903".into()], 0, "418207".into());
    assert_eq!(a.check(51234903, "Mourad", None), Access::Allowed);
    assert_eq!(a.paired().unwrap().chat_id, 51234903);
    // …and it is still one chat at a time.
    assert!(matches!(a.check(7, "Other", Some("418207")), Access::Refused(_)));
}

#[test]
fn with_pairing_off_only_the_allow_list_is_honoured() {
    let mut a = Auth::new(false, vec!["5".into()], 0, "418207".into());
    assert_eq!(a.check(5, "Listed", None), Access::Allowed);
    assert_eq!(a.check(6, "Unlisted", Some("418207")), Access::Silent, "the code is not a bypass");
}

#[test]
fn an_unpaired_code_rotates_so_a_gate_left_running_stops_advertising_yesterdays() {
    let mut a = auth();
    assert_eq!(a.tick(CODE_TTL_MS - 1, || "999999".into()), None, "not stale yet");
    assert_eq!(a.tick(CODE_TTL_MS, || "999999".into()), Some("999-999".into()));
    assert_eq!(a.check(7, "M", Some("418207")), Access::Refused("wrong code (1 of 5 tries used)".into()));
    assert_eq!(a.check(7, "M", Some("999999")), Access::JustPaired);
}

#[test]
fn a_paired_gate_stops_rotating_codes() {
    let mut a = auth();
    a.check(7, "M", Some("418207"));
    assert_eq!(a.tick(CODE_TTL_MS * 10, || "999999".into()), None);
}

#[test]
fn the_displayed_code_is_grouped_for_reading() {
    assert_eq!(auth().display_code(), "418-207");
}

#[test]
fn generated_codes_are_six_digits_and_vary() {
    let codes: std::collections::HashSet<String> = (0..64).map(|_| new_code()).collect();
    for c in &codes {
        assert_eq!(c.len(), 6, "{c}");
        assert!(c.chars().all(|ch| ch.is_ascii_digit()), "{c}");
    }
    assert!(codes.len() > 40, "codes must not be predictable (got {} distinct of 64)", codes.len());
}
