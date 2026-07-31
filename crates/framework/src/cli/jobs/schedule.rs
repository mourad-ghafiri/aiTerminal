use crate::cli::jobs::args::parse_period;

/// Parse a natural delay / clock phrase out of a request and return the schedule plus the
/// request with that phrase removed — the **fallback** when the AI planner is unavailable,
/// and the reader for the explicit `--in` / `--at` / `--every` flags. Recognizes
/// "in|after <n> sec|min|hour|day(s)" (or a fused `30s`/`2min`), "at HH[:MM][am/pm]", and
/// "every <n> <unit>" / "every hour|day". No match → `(None, request)` (run now).
pub(crate) fn parse_schedule(prompt: &str, now: u64) -> (Option<crate::jobs::Schedule>, String) {
    let words: Vec<&str> = prompt.split_whitespace().collect();
    for i in 0..words.len() {
        let kw = words[i].to_ascii_lowercase();
        if kw == "every" {
            if let Some((secs, used)) = parse_period(&words[i + 1..]) {
                return (Some(crate::jobs::Schedule::Every(secs)), join_excluding(&words, i, i + 1 + used));
            }
        } else if kw == "in" || kw == "after" {
            if let Some((secs, used)) = parse_delay(&words[i + 1..]) {
                return (Some(crate::jobs::Schedule::Once(now.saturating_add(secs))), join_excluding(&words, i, i + 1 + used));
            }
        } else if kw == "at" {
            if let Some(word) = words.get(i + 1) {
                if let Some(fire) = parse_clock_at(word, now) {
                    return (Some(crate::jobs::Schedule::Once(fire)), join_excluding(&words, i, i + 2));
                }
            }
        }
    }
    (None, prompt.to_string())
}

/// Parse a relative delay from the words after `in`/`after` → `(seconds, words_consumed)`.
pub(crate) fn parse_delay(rest: &[&str]) -> Option<(u64, usize)> {
    let first = rest.first()?;
    if let Some((n, unit)) = split_num_unit(first) {
        return unit_secs(unit, n).map(|s| (s, 1));
    }
    let n: u64 = first.parse().ok()?;
    let unit = rest.get(1)?;
    unit_secs(unit, n).map(|s| (s, 2))
}

/// Split a fused `30s` / `2min` / `1h` into `(number, unit)`; `None` if not that shape.
fn split_num_unit(w: &str) -> Option<(u64, &str)> {
    let split = w.find(|c: char| !c.is_ascii_digit())?;
    if split == 0 {
        return None;
    }
    let n: u64 = w[..split].parse().ok()?;
    Some((n, &w[split..]))
}

/// Seconds for `n` of a time unit (`s/sec/min/m/hour/h/day/d`, plural OK); `None` if the
/// unit is unknown **or the span is absurd**.
///
/// The multiply is checked: `in 999999999999999999 days` overflowed `u64` and wrapped to
/// a small, entirely different time — silently in release, as a panic in debug. A span
/// nobody could mean is refused rather than turned into one they did not ask for.
pub(crate) fn unit_secs(unit: &str, n: u64) -> Option<u64> {
    let mult = match unit.to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => 86400,
        _ => return None,
    };
    // A century is past every real use and still far from the edge, so the arithmetic
    // downstream (`now + secs`) has room too.
    const MAX_SPAN_SECS: u64 = 100 * 365 * 86_400;
    n.checked_mul(mult).filter(|s| *s <= MAX_SPAN_SECS)
}

/// Parse a clock time (`17:30`, `5pm`, `9`, `9am`) → the next unix time it occurs (today,
/// or tomorrow if already past), using the local UTC offset. `None` if not a clock time.
pub(crate) fn parse_clock_at(word: &str, now: u64) -> Option<u64> {
    let w = word.to_ascii_lowercase();
    let (body, ampm) = if let Some(b) = w.strip_suffix("pm") {
        (b, Some(true))
    } else if let Some(b) = w.strip_suffix("am") {
        (b, Some(false))
    } else {
        (w.as_str(), None)
    };
    let (h_str, m_str) = body.split_once(':').unwrap_or((body, "0"));
    let mut hour: i64 = h_str.parse().ok()?;
    let min: i64 = m_str.parse().ok()?;
    if !(0..=23).contains(&hour) || !(0..=59).contains(&min) {
        return None;
    }
    match ampm {
        Some(true) if hour < 12 => hour += 12, // 5pm → 17
        Some(false) if hour == 12 => hour = 0, // 12am → 0
        _ => {}
    }
    let offset = platform::os::utc_offset_secs();
    let local_now = now as i64 + offset;
    let day_start = local_now - local_now.rem_euclid(86400);
    let mut target = day_start + hour * 3600 + min * 60;
    if target <= local_now {
        target += 86400; // already passed today → tomorrow
    }
    Some((target - offset) as u64)
}

/// Rejoin `words` skipping the half-open range `[start, end)` (the schedule phrase).
fn join_excluding(words: &[&str], start: usize, end: usize) -> String {
    words.iter().enumerate().filter(|(i, _)| *i < start || *i >= end).map(|(_, w)| *w).collect::<Vec<_>>().join(" ")
}
