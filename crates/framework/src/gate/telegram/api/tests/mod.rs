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
