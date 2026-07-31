use super::*;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tt-log-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn rec(level: Level, unix_secs: i64, msg: &str) -> Record {
    Record { level, unix_secs, millis: 7, target: "t::m", msg: msg.into() }
}

#[test]
fn level_parses_orders_and_thresholds() {
    assert_eq!(Level::parse("ERROR"), Level::Error);
    assert_eq!(Level::parse("Warn"), Level::Warn);
    assert_eq!(Level::parse("off"), Level::Off);
    assert_eq!(Level::parse("nonsense"), Level::Error); // safe default
    assert!(Level::Error < Level::Warn && Level::Warn < Level::Info);
    assert_eq!(Level::Error.as_str(), "ERROR");
}

#[test]
fn record_renders_the_canonical_line() {
    let secs = 1_782_604_800;
    let ts = corelib::datetime::format(secs, "%Y-%m-%d %H:%M:%S", 0);
    let line = rec(Level::Error, secs, "boom").render(0);
    assert_eq!(line, format!("{ts}.007 ERROR t::m: boom\n"));
}

#[test]
fn sink_writes_today_and_rotates_per_day() {
    let dir = tmp("rotate");
    // Retention 0 (pruning off): the fixture days are FIXED timestamps, so any
    // real retention window would eventually prune them as the actual date
    // moves on (a time-bomb test). Pruning has its own test below.
    let mut sink = RotatingFileSink::new(dir.clone(), 0, 0);
    let day1 = 1_782_604_800;
    let day2 = day1 + 86_400; // the following day
    let d1 = corelib::datetime::format(day1, "%Y-%m-%d", 0);
    let d2 = corelib::datetime::format(day2, "%Y-%m-%d", 0);
    assert_ne!(d1, d2, "the two timestamps must fall on different days");
    sink.write(&rec(Level::Error, day1, "first"));
    sink.write(&rec(Level::Error, day1 + 5, "second"));
    sink.write(&rec(Level::Warn, day2, "next day"));
    sink.flush();
    let a = std::fs::read_to_string(dir.join(format!("{d1}.log"))).unwrap();
    let b = std::fs::read_to_string(dir.join(format!("{d2}.log"))).unwrap();
    assert!(a.contains("first") && a.contains("second") && !a.contains("next day"));
    assert!(b.contains("next day") && !b.contains("first"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prune_removes_files_older_than_retention() {
    let dir = tmp("prune");
    std::fs::create_dir_all(&dir).unwrap();
    // An ancient day file + a recent one (today).
    std::fs::write(dir.join("2000-01-01.log"), "old\n").unwrap();
    let today = corelib::datetime::format(
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
        "%Y-%m-%d",
        0,
    );
    std::fs::write(dir.join(format!("{today}.log")), "new\n").unwrap();
    // Construction prunes with a 7-day window.
    let _sink = RotatingFileSink::new(dir.clone(), 7, 0);
    assert!(!dir.join("2000-01-01.log").exists(), "ancient file pruned");
    assert!(dir.join(format!("{today}.log")).exists(), "today's file kept");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn end_to_end_init_emit_flush_filters_by_level() {
    let dir = tmp("e2e");
    // Default threshold is Error; init with Error so info! is filtered out.
    init(dir.clone(), Level::Error, 7);
    assert!(enabled(Level::Error) && !enabled(Level::Info));
    crate::error!("written {}", 1);
    crate::info!("filtered {}", 2);
    flush();
    let today = corelib::datetime::format(
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
        "%Y-%m-%d",
        crate::os::utc_offset_secs(),
    );
    let body = std::fs::read_to_string(dir.join(format!("{today}.log"))).unwrap();
    assert!(body.contains("written 1"), "error line present: {body:?}");
    assert!(!body.contains("filtered"), "info filtered at error threshold");
    let _ = std::fs::remove_dir_all(&dir);
}
