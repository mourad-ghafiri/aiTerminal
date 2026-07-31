use super::*;

#[test]
fn civil_round_trips_known_dates() {
    // 2026-06-22 00:00:00 UTC
    let secs = to_unix(2026, 6, 22, 0, 0, 0, 0);
    let dt = from_unix(secs, 0);
    assert_eq!((dt.year, dt.month, dt.day), (2026, 6, 22));
    // 1970-01-01 was a Thursday (weekday 4)
    assert_eq!(from_unix(0, 0).weekday, 4);
    // epoch
    assert_eq!(to_unix(1970, 1, 1, 0, 0, 0, 0), 0);
}

#[test]
fn format_strftime_lite() {
    let secs = to_unix(2026, 6, 22, 14, 5, 9, 0);
    assert_eq!(format(secs, "%Y-%m-%d %H:%M:%S", 0), "2026-06-22 14:05:09");
    assert_eq!(format(secs, "%b %d, %Y %I:%M %p", 0), "Jun 22, 2026 02:05 PM");
    assert_eq!(format(secs, "100%%", 0), "100%");
}

#[test]
fn parse_iso_and_format() {
    let secs = parse("2026-06-22 14:05:09", None, 0).unwrap();
    assert_eq!(format(secs, "%Y-%m-%d %H:%M:%S", 0), "2026-06-22 14:05:09");
    assert_eq!(parse("2026-06-22", None, 0).unwrap(), to_unix(2026, 6, 22, 0, 0, 0, 0));
    assert!(parse("not a date", None, 0).is_none());
}

#[test]
fn offset_shifts_local_time() {
    // +2h offset: 12:00 UTC reads as 14:00 local.
    let utc_noon = to_unix(2026, 1, 1, 12, 0, 0, 0);
    assert_eq!(from_unix(utc_noon, 7200).hour, 14);
}

#[test]
fn relative_phrases() {
    assert_eq!(relative(1000, 1010), "just now");
    assert_eq!(relative(1000, 1000 + 180), "3 minutes ago");
    assert_eq!(relative(1000, 1000 + 7200), "2 hours ago");
    assert_eq!(relative(1000 + 86400 * 2, 1000), "in 2 days");
}

#[test]
fn durations_read_the_way_people_write_them() {
    assert_eq!(duration("30m"), Some(1800));
    assert_eq!(duration("90s"), Some(90));
    assert_eq!(duration("2h"), Some(7200));
    assert_eq!(duration("7d"), Some(604_800));
    assert_eq!(duration("1h30m"), Some(5400));
    assert_eq!(duration("1h 30m"), Some(5400), "spaces are noise");
    assert_eq!(duration("1800"), Some(1800), "a bare number is seconds");
    assert_eq!(duration("2H"), Some(7200), "case is noise");
    // A misspelled bound must be refusable, never silently a default.
    for bad in ["", "  ", "30x", "abc", "1h30", "m", "-5"] {
        assert_eq!(duration(bad), None, "{bad:?} must not parse");
    }
}
