use super::*;

#[test]
fn parses_status_and_body_from_curls_include_output() {
    let raw = "HTTP/2 200\r\ncontent-type: application/json\r\n\r\n{\"ok\":true}";
    assert_eq!(split_response(raw), (200, "{\"ok\":true}".to_string()));
}

#[test]
fn skips_an_informational_block() {
    let raw = "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 429 Too Many\r\nx: y\r\n\r\nbody";
    assert_eq!(split_response(raw), (429, "body".to_string()));
}

#[test]
fn the_long_poll_url_carries_the_offset_and_asks_for_messages_and_taps() {
    let api = CurlBotApi::with_base("123:ABC", "https://example.invalid");
    let url = api.url("getUpdates");
    assert_eq!(url, "https://example.invalid/bot123:ABC/getUpdates");
    // The bot must not be handed edits/joins it would then have to filter.
    assert!(format!("{url}?offset=5&timeout=25&allowed_updates=%5B%22message%22%2C%22callback_query%22%5D").contains("allowed_updates"));
}

#[test]
fn curl_must_outlive_the_servers_hold() {
    // If curl's own deadline were <= the poll timeout, EVERY poll would be killed
    // by our own client just before the server answered.
    let poll = 25u32;
    assert!(poll + POLL_SLACK_S > poll + 10, "not enough headroom for TLS + response");
}

#[test]
fn a_shut_down_client_refuses_to_start_new_requests() {
    let api = CurlBotApi::with_base("t", "https://example.invalid");
    api.shutdown();
    assert_eq!(api.get_updates(0, 1), Err(ApiError::Cancelled), "no process is spawned after shutdown");
    assert_eq!(api.send_message(1, "hi", None), Err(ApiError::Cancelled));
    assert_eq!(api.answer_callback("cb1"), Err(ApiError::Cancelled));
}
