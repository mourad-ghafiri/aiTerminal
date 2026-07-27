//! The seam every feature plugs into, and the helpers each world shares.
//!
//! A feature is a [`World`]: it owns its own verbs and knows nothing about discovery,
//! parsing or reporting. Adding one is a folder, a file, and a line in the registry.
//!
//! Verbs are matched by name at runtime rather than through one shared enum, because the
//! vocabulary is *open across* features but closed *within* each one. Every world keeps
//! its own exhaustive match and returns [`unknown_verb`] for anything else — so a typo in
//! a scenario fails the suite instead of passing silently.

use corelib::wire::Toml;

/// One feature's scenario vocabulary.
pub trait World {
    /// Run one step. The error is shown verbatim in the failure report, so it should read
    /// as a bug report: what was expected, and what actually happened.
    fn apply(&mut self, step: &Toml) -> Result<(), String>;
}

/// Builds a world from a scenario's `[setup]` table.
pub type Factory = fn(&Toml) -> Result<Box<dyn World>, String>;

// ── reading a step ───────────────────────────────────────────────────────────

/// Escapes TOML cannot carry. `corelib`'s parser handles `\n \t \r \" \\` and nothing
/// else, so control bytes get readable names rather than being unwritable.
pub fn unescape(s: &str) -> String {
    s.replace("<ESC>", "\u{1b}").replace("<BEL>", "\u{7}").replace("<CR>", "\r").replace("<LF>", "\n")
}

/// A string argument, with escapes resolved.
pub fn text(step: &Toml, key: &str) -> Option<String> {
    step.get(key).and_then(|v| v.as_str()).map(unescape)
}

/// A list-of-strings argument, with escapes resolved.
pub fn list(step: &Toml, key: &str) -> Option<Vec<String>> {
    step.get(key).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str()).map(unescape).collect())
}

pub fn int(step: &Toml, key: &str) -> Option<i64> {
    step.get(key).and_then(|v| v.as_int())
}

pub fn flag(step: &Toml, key: &str) -> Option<bool> {
    step.get(key).and_then(|v| v.as_bool())
}

/// The keys a step carries — for the label and the unknown-verb error.
pub fn keys(step: &Toml) -> Vec<String> {
    step.as_table().map(|kv| kv.iter().map(|(k, _)| k.clone()).collect()).unwrap_or_default()
}

/// What a world returns when it does not recognize a step.
pub fn unknown_verb(step: &Toml) -> String {
    format!("no known verb in this step (keys: {})", keys(step).join(", "))
}

/// A short label for the failure report: the verb and its argument, clipped.
pub fn label(step: &Toml) -> String {
    let Some(pairs) = step.as_table() else { return "(not a table)".into() };
    pairs
        .iter()
        .map(|(k, v)| match v {
            Toml::Str(s) => format!("{k} {:?}", clip(s)),
            Toml::Int(n) => format!("{k} {n}"),
            Toml::Bool(b) => format!("{k} {b}"),
            Toml::Array(a) => format!("{k} [{} items]", a.len()),
            _ => k.clone(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Make control bytes visible, and keep a failure message to one line.
pub fn clip(s: &str) -> String {
    let shown: String = s.chars().take(48).collect();
    shown.replace('\u{1b}', "<ESC>").replace('\r', "<CR>").replace('\n', "<LF>")
}

/// Render a value for a failure message, with escapes made visible.
pub fn show(s: &str) -> String {
    let clipped: String = s.chars().take(400).collect();
    format!("{:?}", clipped.replace('\u{1b}', "<ESC>"))
}

// ── the assertions every world reaches for ───────────────────────────────────

/// Every fragment must appear in `got`.
pub fn expect_contains(got: &str, want: &[String], what: &str) -> Result<(), String> {
    for w in want {
        if !got.contains(w.as_str()) {
            return Err(format!("expected {w:?} in {what}; got {}", show(got)));
        }
    }
    Ok(())
}

/// …and its negation, which is usually the one that matters.
pub fn expect_missing(got: &str, bad: &[String], what: &str) -> Result<(), String> {
    for b in bad {
        if got.contains(b.as_str()) {
            return Err(format!("{b:?} must NOT appear in {what}; got {}", show(got)));
        }
    }
    Ok(())
}

/// An exact match, reported readably.
pub fn expect_eq(got: &str, want: &str, what: &str) -> Result<(), String> {
    if got == want {
        return Ok(());
    }
    Err(format!("{what} was {} — expected {}", show(got), show(want)))
}

/// A line-by-line match, naming the first row that differs.
pub fn expect_lines(got: &[String], want: &[String], what: &str) -> Result<(), String> {
    for (i, w) in want.iter().enumerate() {
        match got.get(i) {
            Some(g) if g == w => {}
            Some(g) => return Err(format!("{what} line {i} was {} — expected {}", show(g), show(w))),
            None => return Err(format!("{what} has only {} line(s); expected {w:?} at line {i}", got.len())),
        }
    }
    if got.len() > want.len() {
        return Err(format!("{what} has {} extra line(s), starting {}", got.len() - want.len(), show(&got[want.len()])));
    }
    Ok(())
}
