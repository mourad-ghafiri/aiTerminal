//! What a tool call looks like while it happens.
//!
//! The trace used to print the model's raw argument JSON, truncated at 72 characters:
//!
//! ```text
//! ⚙ fs.read {"path":"crates/framework/src/cli/runner.rs","max":20 · 9ms · 6.1KB
//! ⚙ sys.run {"cmd":"cargo test --workspace --all-features 2>&1 | · 2.1s · 1.4KB
//! ```
//!
//! Which is the wire format, not the event. It is unreadable at a glance, it truncates
//! mid-token so the one thing you wanted — *which file* — is usually the part that got
//! cut, and it says nothing about what came back.
//!
//! ```text
//! ⚙ fs.read    crates/framework/src/cli/runner.rs · 9ms · 6.1KB
//! ⚙ sys.run    cargo test --workspace --all-features · 2.1s · 48 lines
//! ⚙ fs.edit    src/cli.rs · 12ms · 1 replaced
//! ⚙ fs.list    crates/framework/src · 2ms · 14 entries
//! ⚙ web.search "LLM memory architectures" · 480ms · 5 results
//! ```
//!
//! **Nothing here knows a tool by name.** A table of per-tool formatters would be a
//! second registry to keep in step with [`crate::caps`], and it would be wrong the day
//! somebody adds a tool or an MCP server exposes one. What it knows is that arguments
//! have *names*, and that some names identify a call while others configure it: `path`
//! and `cmd` and `url` say what is being acted on, `max` and `all` and `scope` say how.
//! The same is true of the result: a JSON array is a list of things and a multi-line
//! string is output you would scroll, so both halves are read off shape.

use corelib::wire::Json;

/// Argument names that identify a call rather than configure it, best first.
///
/// The same vocabulary [`crate::caps::arg`] already accepts as synonyms, which is not a
/// coincidence: the names a weak model reaches for are the names worth showing back.
const SUBJECT: [&str; 14] = [
    "path", "file", "cmd", "command", "url", "query", "pattern", "glob", "src", "name", "key", "id", "text", "content",
];

/// How much of a subject is worth showing before it stops being a glance.
const SUBJECT_MAX: usize = 56;

/// One tool call, as a line a person can read.
pub(crate) fn call(name: &str, args: &[(String, String)]) -> String {
    match subject(args) {
        Some(s) => format!("{name} {s}"),
        None => name.to_string(),
    }
}

/// Argument names whose value is free text somebody typed, rather than a name of
/// something. Only these are quoted: `cargo test` and `LLM memory architectures` are
/// indistinguishable as strings, so the value cannot be the thing that decides — the
/// KEY is what knows whether it holds a phrase or an identifier.
const PHRASE: [&str; 3] = ["query", "search", "text"];

/// The argument that says what this call is *about*.
fn subject(args: &[(String, String)]) -> Option<String> {
    let pick = SUBJECT
        .iter()
        .find_map(|want| args.iter().find(|(k, _)| k.eq_ignore_ascii_case(want)))
        // Nothing recognised: the first argument that carries anything. A tool nobody
        // here has heard of still says more with its first value than with none.
        .or_else(|| args.iter().find(|(_, v)| !v.trim().is_empty()))?;
    let value = tidy(&pick.1);
    if value.is_empty() {
        return None;
    }
    let quoted = PHRASE.iter().any(|k| pick.0.eq_ignore_ascii_case(k));
    Some(if quoted { format!("\"{value}\"") } else { value })
}

/// A value on one line, bounded.
fn tidy(raw: &str) -> String {
    // The first line only: a `content` argument is a whole file, and its first line is
    // the useful summary of it. Middle-elided rather than truncated, because a path's
    // last component is the part you were looking for and a plain cut always takes it.
    let one = raw.trim().lines().next().unwrap_or("").trim();
    elide(one, SUBJECT_MAX)
}

/// Shorten to `max`, keeping both ends — `crates/…/cli/runner.rs`, never `crates/frame`.
fn elide(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let (head, tail) = (keep / 3, keep - keep / 3);
    let a: String = chars[..head].iter().collect();
    let b: String = chars[chars.len() - tail..].iter().collect();
    format!("{a}\u{2026}{b}")
}

/// What came back, read off the JSON's shape rather than off the tool's name.
///
/// The shapes named here are the ones the shipped tools actually return — checked against
/// `caps`, not imagined. An earlier draft reported `exit 0` for a command, which reads
/// well and is a lie: `sys.run` hands back the combined output as a string and throws the
/// status away. A trace that invents a fact is worse than one that reports less.
pub(crate) fn result(v: &Json) -> String {
    match v {
        // A list is a list of things, whatever the things are.
        Json::Arr(items) => plural(items.len(), "result"),
        Json::Obj(fields) => {
            let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v);
            if let Some(Json::Arr(items)) = get("entries") {
                return plural(items.len(), "entry");
            }
            if let Some(Json::Num(n)) = get("replaced") {
                return format!("{} replaced", *n as i64);
            }
            crate::cli::format::human_bytes(super::run::json_text(v).len())
        }
        // Output somebody would scroll: how many lines of it, which is the question you
        // ask of a command's output. A one-liner is a value, not a listing, so that stays
        // a size.
        Json::Str(s) if s.contains('\n') => plural(s.lines().count(), "line"),
        Json::Str(s) => crate::cli::format::human_bytes(s.len()),
        Json::Bool(b) => b.to_string(),
        other => crate::cli::format::human_bytes(super::run::json_text(other).len()),
    }
}

/// How long it took, in the unit a person would have used. `2100ms` is a number you
/// have to convert before you can react to it; `2.1s` is one you already understand.
pub(crate) fn took(ms: u128) -> String {
    match ms {
        0..=999 => format!("{ms}ms"),
        _ => format!("{:.1}s", ms as f64 / 1000.0),
    }
}

/// `1 entry` / `2 entries`. English is not `word + "s"`, and "1 entries" is the kind of
/// detail that makes a tool feel unfinished.
fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        return format!("{n} {word}");
    }
    match word.strip_suffix('y') {
        Some(stem) => format!("{n} {stem}ies"),
        None => format!("{n} {word}s"),
    }
}

#[cfg(test)]
mod tests;
