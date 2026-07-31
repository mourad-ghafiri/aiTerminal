use super::parse_response;

#[test]
fn parses_status_headers_body() {
    let raw = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-A: b\r\n\r\n{\"ok\":true}";
    let (status, headers, body) = parse_response(raw);
    assert_eq!(status, 200);
    assert_eq!(body, "{\"ok\":true}");
    assert!(headers.iter().any(|(k, v)| k == "Content-Type" && v == "application/json"));
}

#[test]
fn skips_100_continue_block() {
    let raw = "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 201 Created\r\nLocation: /x\r\n\r\nbody";
    let (status, _h, body) = parse_response(raw);
    assert_eq!(status, 201);
    assert_eq!(body, "body");
}
