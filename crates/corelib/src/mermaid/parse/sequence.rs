//! The `sequenceDiagram` parser: participants, messages, notes, activations and the
//! framed blocks (`loop` / `alt` / `opt` / `par` / `critical` / `break` / `rect`).
//!
//! Everything lands on one ordered [`Event`] timeline, so the layout can walk the diagram
//! exactly once, in source order, and never has to correlate two parallel lists.

use super::super::lex::{self, Stmt};
use super::super::{Actor, Cap, Event, Message, NotePos, Sequence, Stroke, MAX_ITEMS};

/// Message arrows, longest first so `-->>` wins over `-->` and `<<-->>` over `<<->>`.
const ARROWS: [&str; 10] = ["<<-->>", "<<->>", "-->>", "--)", "-->", "--x", "->>", "-)", "->", "-x"];

/// Keywords that open a framed region and are closed by `end`.
const BLOCKS: [&str; 7] = ["loop", "alt", "opt", "par", "critical", "break", "rect"];

pub fn parse(stmts: &[Stmt]) -> Sequence {
    let mut s = Sequence::default();
    let mut ids: Vec<String> = Vec::new();
    // What each open `end` closes: `true` for a participant `box`, `false` for a block.
    let mut open: Vec<bool> = Vec::new();
    let mut current_box: Option<usize> = None;

    for st in stmts {
        let line = st.text.as_str();
        let word = lex::first_word(line);

        if word == "autonumber" {
            s.autonumber = true;
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "title") {
            s.title = lex::label_text(rest);
            continue;
        }
        // Accessibility text and the interaction directives carry no picture.
        if matches!(word.as_str(), "accTitle" | "accDescr" | "acctitle" | "accdescr" | "link" | "links" | "properties" | "style") {
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "box") {
            if s.boxes.len() < MAX_ITEMS {
                s.boxes.push(box_title(rest));
                current_box = Some(s.boxes.len() - 1);
                open.push(true);
            }
            continue;
        }
        if line.eq_ignore_ascii_case("end") {
            match open.pop() {
                Some(true) => current_box = None,
                Some(false) => s.events.push(Event::BlockEnd),
                None => {}
            }
            continue;
        }
        if let Some(kind) = BLOCKS.iter().find(|k| lex::starts_with_word(line, k)) {
            let label = lex::strip_word(line, kind).map(lex::label_text).unwrap_or_default();
            s.events.push(Event::BlockStart { kind: (*kind).to_string(), label: block_label(kind, &label) });
            open.push(false);
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "else").or_else(|| lex::strip_word(line, "and")).or_else(|| lex::strip_word(line, "option")) {
            s.events.push(Event::BlockElse { label: lex::label_text(rest) });
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "activate") {
            if let Some(i) = find(&ids, rest.trim()) {
                s.events.push(Event::Activate(i));
            }
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "deactivate") {
            if let Some(i) = find(&ids, rest.trim()) {
                s.events.push(Event::Deactivate(i));
            }
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "destroy") {
            if let Some(i) = find(&ids, rest.trim()) {
                s.events.push(Event::Destroy(i));
            }
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "note").or_else(|| lex::strip_word(line, "Note")) {
            if let Some(ev) = note(rest, &mut s, &mut ids, current_box) {
                s.events.push(ev);
            }
            continue;
        }
        // `create participant C` declares a participant that appears mid-diagram; the
        // picture is the same, so only the declaration matters.
        let decl = lex::strip_word(line, "create").unwrap_or(line);
        if let Some(rest) = lex::strip_word(decl, "participant").or_else(|| lex::strip_word(decl, "actor")) {
            let stick = lex::starts_with_word(decl, "actor");
            let (id, name) = rest.split_once(" as ").map(|(i, d)| (i.trim(), d.trim())).unwrap_or((rest.trim(), rest.trim()));
            intern(id, name, stick, current_box, &mut s, &mut ids);
            continue;
        }
        if let Some(m) = message(line, &mut s, &mut ids, current_box) {
            if s.events.len() < MAX_ITEMS {
                s.events.push(Event::Message(m));
            }
        }
    }
    // An unclosed block would otherwise swallow the rest of the diagram's frame.
    for did_box in open.into_iter().rev() {
        if !did_box {
            s.events.push(Event::BlockEnd);
        }
    }
    s
}

/// `box Aqua Left Side` — an optional leading color word, then the title.
fn box_title(rest: &str) -> String {
    let t = rest.trim();
    let first = t.split_whitespace().next().unwrap_or("");
    let is_color = first.starts_with('#') || first.starts_with("rgb") || matches!(first.to_ascii_lowercase().as_str(), "transparent" | "aqua" | "red" | "green" | "blue" | "yellow" | "grey" | "gray" | "white" | "black");
    let title = if is_color { t[first.len()..].trim() } else { t };
    lex::label_text(title)
}

/// The caption a frame shows: `loop every minute` → `loop every minute`, `rect rgb(0,0,0)`
/// → `rect` (a color is not a caption).
fn block_label(kind: &str, label: &str) -> String {
    if kind == "rect" || label.is_empty() {
        return kind.to_string();
    }
    format!("{kind} {label}")
}

fn find(ids: &[String], id: &str) -> Option<usize> {
    ids.iter().position(|a| a == id.trim())
}

/// Intern by reference id, keeping the display name of the first declaration.
fn intern(id: &str, name: &str, stick: bool, bx: Option<usize>, s: &mut Sequence, ids: &mut Vec<String>) -> usize {
    let id = id.trim();
    if let Some(i) = find(ids, id) {
        return i;
    }
    if ids.len() >= MAX_ITEMS {
        return 0;
    }
    ids.push(id.to_string());
    s.actors.push(Actor { id: id.to_string(), name: lex::label_text(name), stick, bx });
    ids.len() - 1
}

/// `Note over A,B: text` / `Note left of A: text`.
fn note(rest: &str, s: &mut Sequence, ids: &mut Vec<String>, bx: Option<usize>) -> Option<Event> {
    let (pos, after) = if let Some(r) = lex::strip_word(rest, "over") {
        (NotePos::Over, r)
    } else if let Some(r) = rest.strip_prefix("left of").or_else(|| rest.strip_prefix("Left of")) {
        (NotePos::LeftOf, r)
    } else if let Some(r) = rest.strip_prefix("right of").or_else(|| rest.strip_prefix("Right of")) {
        (NotePos::RightOf, r)
    } else {
        return None;
    };
    let (who, text) = after.split_once(':')?;
    let mut names = who.split(',').map(str::trim).filter(|n| !n.is_empty());
    let first = names.next()?;
    let from = intern(first, first, false, bx, s, ids);
    let to = match names.next() {
        Some(second) => intern(second, second, false, bx, s, ids),
        None => from,
    };
    Some(Event::Note { pos, from, to, text: lex::label_text(text) })
}

/// `A->>+B: text` — the arrow, the activation suffixes, and the text.
fn message(line: &str, s: &mut Sequence, ids: &mut Vec<String>, bx: Option<usize>) -> Option<Message> {
    let (pos, arrow) = ARROWS
        .iter()
        .filter_map(|a| line.find(a).map(|p| (p, *a)))
        .min_by(|x, y| x.0.cmp(&y.0).then(y.1.len().cmp(&x.1.len())))?;
    let from_raw = line[..pos].trim();
    let after = &line[pos + arrow.len()..];
    let (to_raw, text) = match after.split_once(':') {
        Some((t, msg)) => (t.trim(), msg.trim()),
        None => (after.trim(), ""),
    };
    if from_raw.is_empty() || to_raw.is_empty() {
        return None;
    }
    // `+` / `-` on the target activate or deactivate a lifeline.
    let (activate, deactivate, to_id) = match to_raw.chars().next() {
        Some('+') => (true, false, to_raw[1..].trim()),
        Some('-') => (false, true, to_raw[1..].trim()),
        _ => (false, false, to_raw),
    };
    let from = intern(from_raw, from_raw, false, bx, s, ids);
    let to = intern(to_id, to_id, false, bx, s, ids);
    Some(Message {
        from,
        to,
        text: lex::label_text(text),
        stroke: if arrow.contains("--") { Stroke::Dashed } else { Stroke::Solid },
        head: match arrow {
            a if a.ends_with('x') => Cap::Cross,
            a if a.ends_with(')') => Cap::Circle, // async: an open arrow
            a if a.ends_with(">>") => Cap::Arrow,
            _ => Cap::Open,
        },
        activate,
        deactivate,
    })
}

#[cfg(test)]
mod tests;
