//! The data languages: `pie`, `xychart`, `quadrantChart`, `gantt`, `sankey`, `radar`,
//! `treemap`, `packet` and `info`.
//!
//! They are small grammars with one thing in common — the picture is a set of numbers —
//! so they share the [`Chart`] model and one layout.

use super::super::lex::{self, Stmt};
use super::super::{Chart, ChartKind, Point, Series, Task, MAX_ITEMS};

/// `pie showData\n title Pets\n "Dogs" : 386`
pub fn pie(header: &str, stmts: &[Stmt]) -> Chart {
    let mut c = Chart::new(ChartKind::Pie);
    let mut values = Vec::new();
    if let Some(rest) = lex::strip_word(header, "pie") {
        // `pie title Pets` puts the title on the header line.
        if let Some(t) = lex::strip_word(rest, "title") {
            c.title = lex::label_text(t);
        }
    }
    for st in stmts {
        let line = st.text.as_str();
        if let Some(rest) = lex::strip_word(line, "title") {
            c.title = lex::label_text(rest);
            continue;
        }
        if let Some((name, value)) = line.rsplit_once(':') {
            if let Some(v) = number(value) {
                if c.categories.len() < MAX_ITEMS {
                    c.categories.push(lex::label_text(name));
                    values.push(v);
                }
            }
        }
    }
    c.series.push(Series { name: String::new(), line: false, values });
    c
}

/// `xychart-beta\n title "Sales"\n x-axis [jan, feb]\n y-axis "Revenue" 0 --> 100\n bar [5, 10]\n line [3, 8]`
pub fn xy(_header: &str, stmts: &[Stmt]) -> Chart {
    let mut c = Chart::new(ChartKind::Xy);
    for st in stmts {
        let line = st.text.as_str();
        if let Some(rest) = lex::strip_word(line, "title") {
            c.title = lex::label_text(rest);
            continue;
        }
        if let Some(rest) = line.strip_prefix("x-axis") {
            c.categories = bracket_list(rest);
            if c.categories.is_empty() {
                c.x_title = lex::label_text(rest);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("y-axis") {
            // `y-axis "Revenue" 0 --> 100` — only the name survives; the range is implied
            // by the data we draw.
            c.y_title = quoted(rest).unwrap_or_else(|| lex::label_text(rest.split("-->").next().unwrap_or(rest)));
            continue;
        }
        for (word, is_line) in [("bar", false), ("line", true)] {
            if let Some(rest) = line.strip_prefix(word) {
                let values: Vec<f64> = bracket_list(rest).iter().filter_map(|v| number(v)).collect();
                if !values.is_empty() && c.series.len() < MAX_ITEMS {
                    c.series.push(Series { name: String::new(), line: is_line, values });
                }
                break;
            }
        }
    }
    c
}

/// `quadrantChart\n title Reach\n x-axis Low --> High\n quadrant-1 We should expand\n Campaign A: [0.3, 0.6]`
pub fn quadrant(_header: &str, stmts: &[Stmt]) -> Chart {
    let mut c = Chart::new(ChartKind::Quadrant);
    for st in stmts {
        let line = st.text.as_str();
        if let Some(rest) = lex::strip_word(line, "title") {
            c.title = lex::label_text(rest);
            continue;
        }
        if let Some(rest) = line.strip_prefix("x-axis") {
            c.x_title = lex::label_text(rest);
            continue;
        }
        if let Some(rest) = line.strip_prefix("y-axis") {
            c.y_title = lex::label_text(rest);
            continue;
        }
        if let Some(rest) = line.strip_prefix("quadrant-") {
            let (n, label) = rest.split_at(1);
            if let Ok(i) = n.parse::<usize>() {
                if (1..=4).contains(&i) {
                    c.quadrants[i - 1] = lex::label_text(label);
                }
            }
            continue;
        }
        // `Campaign A: [0.3, 0.6]`
        if let Some((name, coords)) = line.split_once(':') {
            let nums: Vec<f64> = bracket_list(coords).iter().filter_map(|v| number(v)).collect();
            if nums.len() == 2 && c.points.len() < MAX_ITEMS {
                c.points.push(Point { name: lex::label_text(name), x: nums[0], y: nums[1] });
            }
        }
    }
    c
}

/// `gantt\n dateFormat YYYY-MM-DD\n section Design\n Draft :a1, 2024-01-01, 30d`
pub fn gantt(_header: &str, stmts: &[Stmt]) -> Chart {
    let mut c = Chart::new(ChartKind::Gantt);
    let mut section = String::new();
    let mut format: Option<String> = None;
    // Where an `after <id>` task starts, and the previous task's end for bare durations.
    let mut ends: Vec<(String, i64)> = Vec::new();
    let mut cursor: i64 = 0;

    for st in stmts {
        let line = st.text.as_str();
        let word = lex::first_word(line);
        match word.as_str() {
            "title" => {
                c.title = lex::label_text(lex::strip_word(line, "title").unwrap_or(""));
                continue;
            }
            "dateformat" => {
                format = lex::strip_word(line, "dateFormat").map(|f| f.trim().to_string());
                continue;
            }
            "section" => {
                section = lex::label_text(lex::strip_word(line, "section").unwrap_or(""));
                continue;
            }
            // Axis formatting, exclusions and weekend rules change the ticks, not the bars.
            "axisformat" | "excludes" | "includes" | "todaymarker" | "tickinterval" | "weekday" | "weekend" => continue,
            _ => {}
        }
        let Some((name, rest)) = line.split_once(':') else { continue };
        let fields: Vec<&str> = rest.split(',').map(str::trim).collect();
        let mut task = Task {
            section: section.clone(),
            name: lex::label_text(name),
            start: cursor,
            end: cursor,
            milestone: false,
            done: false,
            active: false,
            critical: false,
        };
        // Read every field first, then decide the dates: a duration means nothing until
        // we know which start it applies to, and the fields can arrive in any order.
        let mut id = String::new();
        let (mut start, mut end, mut len) = (None, None, None);
        for f in &fields {
            let lower = f.to_ascii_lowercase();
            match lower.as_str() {
                "done" => task.done = true,
                "active" => task.active = true,
                "crit" => task.critical = true,
                "milestone" => task.milestone = true,
                _ => {
                    if let Some(after) = lower.strip_prefix("after ") {
                        start = after.split_whitespace().filter_map(|dep| ends.iter().find(|(i, _)| i == dep).map(|(_, e)| *e)).max().or(Some(cursor));
                    } else if let Some(d) = duration(f) {
                        // `10d` is unambiguous; a lenient date parser would happily read a
                        // number out of it, so durations are tested first.
                        len = Some(d);
                    } else if let Some(secs) = date(f, format.as_deref()) {
                        if start.is_none() {
                            start = Some(secs);
                        } else {
                            end = Some(secs);
                        }
                    } else if id.is_empty() && !f.is_empty() && !f.contains(' ') {
                        id = f.to_string();
                    }
                }
            }
        }
        task.start = start.unwrap_or(cursor);
        task.end = match (end, len) {
            (Some(e), _) => e,
            (None, Some(l)) => task.start + l,
            // A task with no duration is a moment: a milestone, or a one-day bar.
            (None, None) => task.start + if task.milestone { 0 } else { DAY },
        };
        cursor = task.end;
        if !id.is_empty() {
            ends.push((id, task.end));
        }
        if c.tasks.len() < MAX_ITEMS {
            c.tasks.push(task);
        }
    }
    c
}

const DAY: i64 = 86_400;

/// `sankey-beta` — comma-separated `source,target,value` rows.
pub fn sankey(_header: &str, stmts: &[Stmt]) -> Chart {
    let mut c = Chart::new(ChartKind::Sankey);
    for st in stmts {
        let cols: Vec<&str> = st.text.split(',').map(str::trim).collect();
        if cols.len() < 3 {
            continue;
        }
        if let Some(v) = number(cols[2]) {
            if c.flows.len() < MAX_ITEMS {
                c.flows.push((lex::label_text(cols[0]), lex::label_text(cols[1]), v));
            }
        }
    }
    c
}

/// `radar-beta\n axis a["Alpha"], b["Beta"]\n curve me["Me"]{10, 20}`
pub fn radar(_header: &str, stmts: &[Stmt]) -> Chart {
    let mut c = Chart::new(ChartKind::Radar);
    for st in stmts {
        let line = st.text.as_str();
        if let Some(rest) = lex::strip_word(line, "title") {
            c.title = lex::label_text(rest);
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "axis") {
            for part in rest.split(',') {
                let (_, label) = super::common::id_and_label(part.trim());
                if !label.is_empty() && c.categories.len() < MAX_ITEMS {
                    c.categories.push(label);
                }
            }
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "curve") {
            let (name, values) = match rest.split_once('{') {
                Some((head, tail)) => (super::common::id_and_label(head.trim()).1, tail.trim_end_matches('}')),
                None => (String::new(), rest),
            };
            let values: Vec<f64> = values.split(',').filter_map(|v| number(v)).collect();
            if !values.is_empty() && c.series.len() < MAX_ITEMS {
                c.series.push(Series { name, line: true, values });
            }
        }
    }
    c
}

/// `treemap-beta` — indented `"Name": value` rows, deepest first in the drawing.
pub fn treemap(_header: &str, stmts: &[Stmt]) -> Chart {
    let mut c = Chart::new(ChartKind::Treemap);
    let mut values = Vec::new();
    let mut section = String::new();
    for st in stmts {
        let line = st.text.as_str();
        if let Some(rest) = lex::strip_word(line, "title") {
            c.title = lex::label_text(rest);
            continue;
        }
        match line.rsplit_once(':') {
            Some((name, value)) if number(value).is_some() => {
                let label = lex::label_text(name);
                let full = if section.is_empty() { label } else { format!("{section} · {label}") };
                if c.categories.len() < MAX_ITEMS {
                    c.categories.push(full);
                    values.push(number(value).unwrap_or(0.0));
                }
            }
            // A row with no value is a heading for the rows under it.
            _ => section = lex::label_text(line.trim_end_matches(':')),
        }
    }
    c.series.push(Series { name: String::new(), line: false, values });
    c
}

/// `packet-beta` — `0-15: "Source Port"` rows.
pub fn packet(_header: &str, stmts: &[Stmt]) -> Chart {
    let mut c = Chart::new(ChartKind::Packet);
    for st in stmts {
        let line = st.text.as_str();
        if let Some(rest) = lex::strip_word(line, "title") {
            c.title = lex::label_text(rest);
            continue;
        }
        if let Some((range, name)) = line.split_once(':') {
            let range = range.trim();
            if !range.is_empty() && c.rows.len() < MAX_ITEMS {
                c.rows.push((range.to_string(), lex::label_text(name)));
            }
        }
    }
    c
}

/// `info` — mermaid's smallest diagram: a card that says what it is.
pub fn info(_header: &str, stmts: &[Stmt]) -> Chart {
    let mut c = Chart::new(ChartKind::Info);
    c.title = "info".to_string();
    for st in stmts {
        if !st.text.is_empty() && c.rows.len() < 16 {
            c.rows.push((String::new(), lex::label_text(&st.text)));
        }
    }
    c
}

/// The first `"quoted"` run in `s`.
fn quoted(s: &str) -> Option<String> {
    let open = s.find('"')?;
    let rest = &s[open + 1..];
    let end = rest.find('"')?;
    Some(lex::label_text(&rest[..end]))
}

/// `[a, b, c]` → the trimmed, unquoted items. Empty when there is no bracketed list.
fn bracket_list(s: &str) -> Vec<String> {
    let (Some(open), Some(close)) = (s.find('['), s.rfind(']')) else { return Vec::new() };
    if close <= open {
        return Vec::new();
    }
    s[open + 1..close].split(',').map(|p| lex::label_text(p)).filter(|p| !p.is_empty()).collect()
}

/// The first number in `s` (`386`, `1.5`, `"42"`).
fn number(s: &str) -> Option<f64> {
    let t = s.trim().trim_matches('"').trim();
    t.parse::<f64>().ok()
}

/// A gantt date in the diagram's own format, or one of the ISO spellings.
fn date(s: &str, format: Option<&str>) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() || t.chars().next()?.is_alphabetic() || !t.contains(['-', '/', ':', '.']) {
        return None;
    }
    // Mermaid spells its formats `YYYY-MM-DD`; the datetime reader speaks strftime.
    let fmt = format.map(strftime_of);
    crate::datetime::parse(t, fmt.as_deref(), 0).or_else(|| crate::datetime::parse(t, None, 0))
}

/// `YYYY-MM-DD` → `%Y-%m-%d`, so mermaid's `dateFormat` reaches the datetime reader in
/// the spelling it understands.
fn strftime_of(fmt: &str) -> String {
    let mut out = fmt.to_string();
    for (from, to) in [("YYYY", "%Y"), ("YY", "%y"), ("MM", "%m"), ("DD", "%d"), ("HH", "%H"), ("mm", "%M"), ("ss", "%S")] {
        out = out.replace(from, to);
    }
    out
}

/// `30d` / `2w` / `12h` / `1m` → seconds.
fn duration(s: &str) -> Option<i64> {
    let t = s.trim();
    let (num, unit) = t.split_at(t.find(|c: char| c.is_alphabetic())?);
    let n: f64 = num.trim().parse().ok()?;
    let secs = match unit.trim().to_ascii_lowercase().as_str() {
        "s" => 1.0,
        "m" | "min" => 60.0,
        "h" => 3600.0,
        "d" => DAY as f64,
        "w" => 7.0 * DAY as f64,
        "mo" => 30.0 * DAY as f64,
        "y" => 365.0 * DAY as f64,
        _ => return None,
    };
    Some((n * secs) as i64)
}

#[cfg(test)]
mod tests;
