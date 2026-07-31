use super::*;

#[test]
fn output_is_capped_and_the_child_still_finishes() {
    // 10 MB of output against a 4 KiB cap: memory stays at the cap, the child
    // runs to completion (the pipe keeps draining), nothing times out.
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "yes | head -c 10000000"]);
    let t = Instant::now();
    let r = run_bounded(cmd, Duration::from_secs(30), 4096).unwrap();
    assert!(r.truncated);
    assert_eq!(r.stdout.len(), 4096, "exactly the cap is kept");
    assert!(!r.timed_out);
    assert!(matches!(r.status, Some(s) if s.success()));
    assert!(t.elapsed() < Duration::from_secs(10), "took {:?}", t.elapsed());
}

#[test]
fn the_deadline_kills_the_child() {
    // A sleeping child must be killed AND reaped at the deadline — the whole
    // call returns promptly with `timed_out` (the zombie-leak regression).
    let mut cmd = Command::new("sleep");
    cmd.arg("30");
    let t = Instant::now();
    let r = run_bounded(cmd, Duration::from_millis(200), 4096).unwrap();
    assert!(r.timed_out);
    assert!(r.status.is_none());
    assert!(t.elapsed() < Duration::from_secs(2), "took {:?}", t.elapsed());
}

#[test]
fn read_tail_keeps_exactly_the_end() {
    let data: Vec<u8> = (0..10_000_000u32).map(|i| (i % 64) as u8 + 0x20).collect();
    let tail = read_tail(std::io::Cursor::new(data.clone()), 4000);
    assert_eq!(tail.len(), 4000);
    assert_eq!(tail.as_bytes(), &data[data.len() - 4000..]);
    // Shorter-than-keep input comes back whole.
    assert_eq!(read_tail(std::io::Cursor::new(b"abc".to_vec()), 4000), "abc");
}
