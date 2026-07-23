//! `datetime` — pure, dependency-free civil-date math + a `strftime`-lite formatter
//! and parser, exposed by the `time.*` native family. Timezone-free at the core: every
//! function takes an explicit `offset_secs` (the caller passes the OS local offset), so
//! this stays in the OS-free Core layer. Civil↔days uses Howard Hinnant's algorithm.

const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// A broken-down local date-time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sunday … 6 = Saturday.
    pub weekday: u32,
}

/// Days since 1970-01-01 for a civil date (Hinnant). Valid for any proleptic Gregorian date.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as i64 + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Civil date `(year, month, day)` from days since 1970-01-01 (the inverse).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Break a unix timestamp into a local [`DateTime`] given the local UTC offset.
pub fn from_unix(secs: i64, offset_secs: i64) -> DateTime {
    let t = secs + offset_secs;
    let days = t.div_euclid(86_400);
    let tod = t.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    DateTime {
        year,
        month,
        day,
        hour: (tod / 3600) as u32,
        minute: (tod % 3600 / 60) as u32,
        second: (tod % 60) as u32,
        weekday: ((days.rem_euclid(7) + 4) % 7) as u32, // 1970-01-01 was Thursday (4)
    }
}

/// A unix timestamp from local civil parts + the local UTC offset.
pub fn to_unix(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32, offset_secs: i64) -> i64 {
    days_from_civil(year, month.clamp(1, 12), day.clamp(1, 31)) * 86_400
        + hour as i64 * 3600
        + minute as i64 * 60
        + second as i64
        - offset_secs
}

/// `strftime`-lite. Supported: `%Y %y %m %d %H %M %S %I %p %B %b %A %a %j %%`.
pub fn format(secs: i64, fmt: &str, offset_secs: i64) -> String {
    let dt = from_unix(secs, offset_secs);
    let mut out = String::with_capacity(fmt.len());
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&dt.year.to_string()),
            Some('y') => out.push_str(&format!("{:02}", dt.year.rem_euclid(100))),
            Some('m') => out.push_str(&format!("{:02}", dt.month)),
            Some('d') => out.push_str(&format!("{:02}", dt.day)),
            Some('H') => out.push_str(&format!("{:02}", dt.hour)),
            Some('M') => out.push_str(&format!("{:02}", dt.minute)),
            Some('S') => out.push_str(&format!("{:02}", dt.second)),
            Some('I') => out.push_str(&format!("{:02}", { let h = dt.hour % 12; if h == 0 { 12 } else { h } })),
            Some('p') => out.push_str(if dt.hour < 12 { "AM" } else { "PM" }),
            Some('B') => out.push_str(MONTHS[(dt.month.clamp(1, 12) - 1) as usize]),
            Some('b') => out.push_str(&MONTHS[(dt.month.clamp(1, 12) - 1) as usize][..3]),
            Some('A') => out.push_str(WEEKDAYS[(dt.weekday % 7) as usize]),
            Some('a') => out.push_str(&WEEKDAYS[(dt.weekday % 7) as usize][..3]),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Parse a timestamp from `text`. With a `fmt` it reads the `%Y/%m/%d/%H/%M/%S` tokens
/// in order; without one it auto-detects ISO `YYYY-MM-DD[ T]HH:MM[:SS]`. Returns the
/// unix seconds in the given local offset, or `None` on a mismatch.
pub fn parse(text: &str, fmt: Option<&str>, offset_secs: i64) -> Option<i64> {
    let nums: Vec<i64> = split_numbers(text);
    let order: Vec<char> = match fmt {
        Some(f) => fmt_order(f),
        None => vec!['Y', 'm', 'd', 'H', 'M', 'S'],
    };
    if nums.is_empty() {
        return None;
    }
    let (mut y, mut mo, mut d, mut h, mut mi, mut s) = (1970i64, 1u32, 1u32, 0u32, 0u32, 0u32);
    for (i, tok) in order.iter().enumerate() {
        let Some(&v) = nums.get(i) else { break };
        match tok {
            'Y' => y = v,
            'y' => y = 2000 + v,
            'm' => mo = v as u32,
            'd' => d = v as u32,
            'H' => h = v as u32,
            'M' => mi = v as u32,
            'S' => s = v as u32,
            _ => {}
        }
    }
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    Some(to_unix(y, mo, d, h, mi, s, offset_secs))
}

/// A human "relative" string, e.g. `3 minutes ago` / `in 2 hours` / `just now`.
pub fn relative(secs: i64, now: i64) -> String {
    let diff = now - secs;
    let (ago, d) = (diff >= 0, diff.unsigned_abs());
    let (n, unit) = if d < 45 {
        return "just now".into();
    } else if d < 5400 {
        ((d + 30) / 60, "minute")
    } else if d < 86_400 {
        ((d + 1800) / 3600, "hour")
    } else if d < 2_592_000 {
        ((d + 43200) / 86_400, "day")
    } else if d < 31_536_000 {
        ((d + 1_296_000) / 2_592_000, "month")
    } else {
        ((d + 15_768_000) / 31_536_000, "year")
    };
    let plural = if n == 1 { "" } else { "s" };
    if ago {
        format!("{n} {unit}{plural} ago")
    } else {
        format!("in {n} {unit}{plural}")
    }
}

/// Runs of digits in `text`, as integers (the simple parser's tokenizer).
fn split_numbers(text: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            if let Ok(n) = cur.parse() {
                out.push(n);
            }
            cur.clear();
        }
    }
    if let Ok(n) = cur.parse() {
        out.push(n);
    }
    out
}

/// The `%X` field letters of a format string, in order (the parse field map).
fn fmt_order(fmt: &str) -> Vec<char> {
    let mut order = Vec::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some(&n) = chars.peek() {
                if matches!(n, 'Y' | 'y' | 'm' | 'd' | 'H' | 'M' | 'S') {
                    order.push(n);
                }
                chars.next();
            }
        }
    }
    order
}

#[cfg(test)]
mod tests {
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
}
