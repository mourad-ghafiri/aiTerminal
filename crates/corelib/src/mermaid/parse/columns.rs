//! The lane languages: `timeline`, `journey` and `kanban`. Different words, one picture —
//! titled columns holding stacks of cards.

use super::super::lex::{self, Stmt};
use super::super::{Card, Columns, Lane, MAX_ITEMS};

/// `timeline`: sections hold periods, and a period holds one or more events.
pub fn timeline(_header: &str, stmts: &[Stmt]) -> Columns {
    let mut c = Columns::default();
    for st in stmts {
        let line = st.text.as_str();
        if let Some(rest) = lex::strip_word(line, "title") {
            c.title = lex::label_text(rest);
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "section") {
            push_lane(&mut c, lex::label_text(rest));
            continue;
        }
        // `2002 : LinkedIn : Friendster` — the period, then its events.
        let mut parts = line.split(':').map(str::trim);
        let Some(period) = parts.next().filter(|p| !p.is_empty()) else { continue };
        let events: Vec<&str> = parts.filter(|p| !p.is_empty()).collect();
        if c.lanes.is_empty() {
            push_lane(&mut c, String::new());
        }
        let lane = c.lanes.last_mut().expect("a lane exists");
        if events.is_empty() {
            push_card(lane, Card { text: lex::label_text(period), ..Card::default() });
        } else {
            for e in events {
                push_card(lane, Card { text: lex::label_text(e), detail: lex::label_text(period), ..Card::default() });
            }
        }
    }
    // A timeline without sections is still a set of periods: one column each reads better
    // than one tall column.
    if c.lanes.len() == 1 && c.lanes[0].title.is_empty() {
        let cards = std::mem::take(&mut c.lanes[0].cards);
        c.lanes.clear();
        for card in cards {
            c.lanes.push(Lane { title: card.detail.clone(), cards: vec![Card { detail: String::new(), ..card }] });
        }
        merge_same_titles(&mut c);
    }
    c
}

/// `journey`: sections hold tasks, each with a 1–5 score and the people involved.
pub fn journey(_header: &str, stmts: &[Stmt]) -> Columns {
    let mut c = Columns { scored: true, ..Columns::default() };
    for st in stmts {
        let line = st.text.as_str();
        if let Some(rest) = lex::strip_word(line, "title") {
            c.title = lex::label_text(rest);
            continue;
        }
        if let Some(rest) = lex::strip_word(line, "section") {
            push_lane(&mut c, lex::label_text(rest));
            continue;
        }
        // `Make tea: 5: Me, Cat`
        let mut parts = line.split(':').map(str::trim);
        let Some(task) = parts.next().filter(|p| !p.is_empty()) else { continue };
        let score = parts.next().and_then(|s| s.trim().parse::<i32>().ok());
        let who = parts.next().unwrap_or("").to_string();
        if c.lanes.is_empty() {
            push_lane(&mut c, String::new());
        }
        let lane = c.lanes.last_mut().expect("a lane exists");
        push_card(lane, Card { text: lex::label_text(task), score, detail: lex::label_text(&who) });
    }
    c
}

/// `kanban`: a column per top-level line, a card per indented line.
pub fn kanban(_header: &str, stmts: &[Stmt]) -> Columns {
    let mut c = Columns::default();
    // The first line's indentation sets what "a column" means for this board.
    let base = stmts.iter().map(|s| s.indent).min().unwrap_or(0);
    for st in stmts {
        let line = st.text.as_str();
        if let Some(rest) = lex::strip_word(line, "title") {
            c.title = lex::label_text(rest);
            continue;
        }
        // `id[Text]` — the id is a handle for metadata, the text is what shows.
        let (_, label, _) = super::flow::node_token(line);
        let text = label.unwrap_or_else(|| lex::label_text(line));
        // Card metadata (`@{ assigned: 'x' }`) is not part of the board's shape.
        if text.starts_with('@') || text.is_empty() {
            continue;
        }
        if st.indent <= base {
            push_lane(&mut c, text);
        } else {
            if c.lanes.is_empty() {
                push_lane(&mut c, String::new());
            }
            let lane = c.lanes.last_mut().expect("a lane exists");
            push_card(lane, Card { text, ..Card::default() });
        }
    }
    c
}

fn push_lane(c: &mut Columns, title: String) {
    if c.lanes.len() < MAX_ITEMS {
        c.lanes.push(Lane { title, cards: Vec::new() });
    }
}

fn push_card(lane: &mut Lane, card: Card) {
    if lane.cards.len() < MAX_ITEMS {
        lane.cards.push(card);
    }
}

/// Fold neighbouring columns that share a title (a timeline period with several events).
fn merge_same_titles(c: &mut Columns) {
    let mut out: Vec<Lane> = Vec::with_capacity(c.lanes.len());
    for lane in c.lanes.drain(..) {
        match out.last_mut() {
            Some(prev) if prev.title == lane.title => prev.cards.extend(lane.cards),
            _ => out.push(lane),
        }
    }
    c.lanes = out;
}

#[cfg(test)]
mod tests {
    use super::super::super::{parse as parse_any, Columns, Diagram};

    fn cols(src: &str) -> Columns {
        match parse_any(src) {
            Some(Diagram::Columns(c)) => c,
            other => panic!("expected a column diagram, got {other:?}"),
        }
    }

    fn titles(c: &Columns) -> Vec<&str> {
        c.lanes.iter().map(|l| l.title.as_str()).collect()
    }

    #[test]
    fn a_timeline_section_holds_its_events() {
        let c = cols("timeline\n title History\n section Social\n  2002 : LinkedIn : Friendster\n  2004 : Facebook");
        assert_eq!(c.title, "History");
        assert_eq!(titles(&c), vec!["Social"]);
        let texts: Vec<&str> = c.lanes[0].cards.iter().map(|k| k.text.as_str()).collect();
        assert_eq!(texts, vec!["LinkedIn", "Friendster", "Facebook"]);
        assert_eq!(c.lanes[0].cards[0].detail, "2002", "the period stays with its event");
    }

    #[test]
    fn a_timeline_without_sections_becomes_one_column_per_period() {
        let c = cols("timeline\n 2002 : LinkedIn : Friendster\n 2004 : Facebook");
        assert_eq!(titles(&c), vec!["2002", "2004"]);
        assert_eq!(c.lanes[0].cards.len(), 2);
    }

    #[test]
    fn a_journey_keeps_scores_and_actors() {
        let c = cols("journey\n title My day\n section Work\n  Make tea: 5: Me\n  Commute: 2: Me, Cat");
        assert!(c.scored);
        assert_eq!(c.lanes[0].cards[0].score, Some(5));
        assert_eq!(c.lanes[0].cards[1].detail, "Me, Cat");
    }

    #[test]
    fn a_kanban_reads_columns_from_indentation() {
        let c = cols("kanban\n  Todo\n    [Write docs]\n    id2[Ship it]\n  Doing\n    id3[Review]");
        assert_eq!(titles(&c), vec!["Todo", "Doing"]);
        assert_eq!(c.lanes[0].cards.iter().map(|k| k.text.as_str()).collect::<Vec<_>>(), vec!["Write docs", "Ship it"]);
        assert_eq!(c.lanes[1].cards.len(), 1);
    }

    #[test]
    fn junk_lines_never_make_empty_cards() {
        let c = cols("journey\n section S\n  : : \n  Real: 3: Me");
        assert_eq!(c.lanes[0].cards.len(), 1);
    }
}
