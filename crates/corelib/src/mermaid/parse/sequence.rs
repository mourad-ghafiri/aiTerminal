//! The `sequenceDiagram` parser: participants and the messages between them.

use super::super::lex::{self, Stmt};
use super::super::{Message, Sequence, MAX_ITEMS};

/// Message arrows, longest first so `-->>` wins over `-->`.
const ARROWS: [&str; 6] = ["-->>", "->>", "-->", "->", "--x", "-x"];

pub fn parse(stmts: &[Stmt]) -> Sequence {
    let mut ids: Vec<String> = Vec::new(); // the reference id used in messages
    let mut actors: Vec<String> = Vec::new(); // the display name (parallel to `ids`)
    let mut messages = Vec::new();
    for st in stmts {
        let line = st.text.as_str();
        if let Some(rest) = lex::strip_word(line, "participant").or_else(|| lex::strip_word(line, "actor")) {
            // `participant A as Alice` → reference id `A`, display `Alice`.
            let (id, display) = rest.split_once(" as ").map(|(i, d)| (i.trim(), d.trim())).unwrap_or((rest.trim(), rest.trim()));
            intern(id, display, &mut ids, &mut actors);
            continue;
        }
        if let Some((from, arrow, to, text)) = split_message(line) {
            let fi = intern(from, from, &mut ids, &mut actors);
            let ti = intern(to, to, &mut ids, &mut actors);
            if messages.len() < MAX_ITEMS {
                messages.push(Message { from: fi, to: ti, text: lex::label_text(text), dashed: arrow.contains("--") });
            }
        }
    }
    Sequence { actors, messages }
}

/// Intern by reference `id`, keeping its `display` name.
fn intern(id: &str, display: &str, ids: &mut Vec<String>, actors: &mut Vec<String>) -> usize {
    let id = id.trim();
    if let Some(i) = ids.iter().position(|a| a == id) {
        i
    } else if ids.len() < MAX_ITEMS {
        ids.push(id.to_string());
        actors.push(display.trim().to_string());
        ids.len() - 1
    } else {
        0
    }
}

/// Split a message line into `(from, arrow, to, text)`.
fn split_message(line: &str) -> Option<(&str, &str, &str, &str)> {
    let mut best: Option<(usize, &str)> = None;
    for a in ARROWS {
        if let Some(pos) = line.find(a) {
            // Earliest arrow wins; on a tie the longest spelling does.
            let better = match best {
                None => true,
                Some((p, w)) => pos < p || (pos == p && a.len() > w.len()),
            };
            if better {
                best = Some((pos, a));
            }
        }
    }
    let (pos, arrow) = best?;
    let from = line[..pos].trim();
    let after = &line[pos + arrow.len()..];
    let (to, text) = match after.split_once(':') {
        Some((t, msg)) => (t.trim(), msg.trim()),
        None => (after.trim(), ""),
    };
    if from.is_empty() || to.is_empty() {
        return None;
    }
    Some((from, arrow, to, text))
}

#[cfg(test)]
mod tests {
    use super::super::super::{parse as parse_any, Diagram};

    fn seq(src: &str) -> super::Sequence {
        match parse_any(src) {
            Some(Diagram::Sequence(s)) => s,
            other => panic!("expected a sequence diagram, got {other:?}"),
        }
    }

    #[test]
    fn actors_and_messages() {
        let s = seq("sequenceDiagram\n participant A as Alice\n A->>B: Hi\n B-->>A: Hello");
        assert_eq!(s.actors, vec!["Alice", "B"]);
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[0].text, "Hi");
        assert!(s.messages[1].dashed);
    }

    #[test]
    fn every_arrow_spelling_is_read() {
        let s = seq("sequenceDiagram\n A->B: a\n A-->B: b\n A->>B: c\n A-->>B: d\n A-xB: e\n A--xB: f");
        assert_eq!(s.messages.len(), 6);
        let dashed: Vec<bool> = s.messages.iter().map(|m| m.dashed).collect();
        assert_eq!(dashed, vec![false, true, false, true, false, true]);
    }

    #[test]
    fn nonsense_lines_are_skipped_not_fatal() {
        let s = seq("sequenceDiagram\n ->>\n A->>B: ok\n : stray");
        assert_eq!(s.messages.len(), 1);
    }
}
