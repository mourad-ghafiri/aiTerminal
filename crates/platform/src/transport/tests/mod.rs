use super::*;

fn drain(h: StreamHandle) -> Vec<Chunk> {
    h.rx.into_iter().collect()
}

#[test]
fn cancel_token_clones_share_one_flag() {
    let a = CancelToken::new();
    let b = a.clone();
    assert!(!a.is_cancelled() && !b.is_cancelled());
    a.cancel();
    assert!(a.is_cancelled() && b.is_cancelled(), "a clone observes the same cancellation");
}

#[test]
fn decoder_frames_data_on_blank_line() {
    let mut d = SseDecoder::new();
    assert_eq!(d.push_line("data: hello"), Ok(None)); // no dispatch yet
    assert_eq!(d.push_line(""), Ok(Some("hello".to_string())));
}

#[test]
fn decoder_joins_multiline_data_and_skips_comments() {
    let mut d = SseDecoder::new();
    assert_eq!(d.push_line(": keep-alive"), Ok(None));
    assert_eq!(d.push_line("data: line1"), Ok(None));
    assert_eq!(d.push_line("data: line2"), Ok(None));
    assert_eq!(d.push_line(""), Ok(Some("line1\nline2".to_string())));
}

#[test]
fn mock_replays_payloads_then_done() {
    let sse = "data: {\"a\":1}\n\ndata: {\"b\":2}\n\n";
    let chunks = drain(MockTransport::from_fixture(sse).stream("", &[], "", &CancelToken::new()));
    assert_eq!(
        chunks,
        vec![
            Chunk::Data("{\"a\":1}".to_string()),
            Chunk::Data("{\"b\":2}".to_string()),
            Chunk::Done,
        ]
    );
}

#[test]
fn empty_stream_reports_error() {
    let chunks = drain(MockTransport::from_fixture("").stream("", &[], "", &CancelToken::new()));
    assert_eq!(chunks, vec![Chunk::Error("empty response from server".to_string())]);
}

#[test]
fn scripted_advances_per_call_and_repeats_last() {
    let t = ScriptedTransport::new(vec!["data: a\n\n".into(), "data: b\n\n".into()]);
    let c = CancelToken::new();
    assert_eq!(drain(t.stream("", &[], "", &c)), vec![Chunk::Data("a".into()), Chunk::Done]);
    assert_eq!(drain(t.stream("", &[], "", &c)), vec![Chunk::Data("b".into()), Chunk::Done]);
    assert_eq!(drain(t.stream("", &[], "", &c)), vec![Chunk::Data("b".into()), Chunk::Done]);
}

#[test]
fn pump_aborts_a_huge_unterminated_line_without_buffering_it() {
    // A hostile/broken server body: 100 MB with NO newline. The pump must
    // error out fast at its line cap — with a tiny test cap here — instead of
    // growing the line buffer to gigabytes (the old OOM).
    struct Endless(u64);
    impl std::io::Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = (self.0.min(buf.len() as u64)) as usize;
            buf[..n].fill(b'a');
            self.0 -= n as u64;
            Ok(n)
        }
    }
    let (tx, rx) = channel();
    let t = std::time::Instant::now();
    let pumped = pump_sse(std::io::BufReader::new(Endless(100 << 20)), &tx, 4096);
    assert!(matches!(pumped.end, PumpEnd::Overflow(_)));
    assert!(t.elapsed() < std::time::Duration::from_secs(2), "took {:?}", t.elapsed());
    drop(tx);
    assert!(rx.into_iter().next().is_none(), "no data chunk from a poisoned stream");
}

#[test]
fn pump_retains_only_a_bounded_sniff_head() {
    // A large but WELL-FORMED stream: the pump forwards every payload but
    // retains at most ERROR_SNIFF_CAP bytes of raw stream for error sniffing
    // (it used to keep the entire response in memory a second time).
    let mut sse = String::new();
    for i in 0..2000 {
        sse.push_str(&format!("data: payload-{i}-{}\n\n", "x".repeat(100)));
    }
    let (tx, rx) = channel();
    let pumped = pump_sse(std::io::Cursor::new(sse), &tx, MAX_SSE_LINE);
    assert!(matches!(pumped.end, PumpEnd::Eof));
    assert!(pumped.saw_any);
    assert!(pumped.sniff.len() <= ERROR_SNIFF_CAP, "sniff head is capped: {}", pumped.sniff.len());
    drop(tx);
    assert_eq!(rx.into_iter().filter(|c| matches!(c, Chunk::Data(_))).count(), 2000);
}

#[test]
fn decoder_caps_an_event_that_never_dispatches() {
    // `data:` lines forever without a blank line: the decoder must refuse at
    // its event cap rather than buffer the lot.
    let mut d = SseDecoder::new();
    let line = format!("data: {}", "y".repeat(1024 * 1024));
    let mut result = Ok(None);
    for _ in 0..16 {
        result = d.push_line(&line);
        if result.is_err() {
            break;
        }
    }
    assert!(result.is_err(), "the event cap must trip within {} bytes", MAX_SSE_EVENT);
    // The decoder stays usable for a fresh, sane event afterwards.
    assert_eq!(d.push_line("data: ok"), Ok(None));
    assert_eq!(d.push_line(""), Ok(Some("ok".to_string())));
}
