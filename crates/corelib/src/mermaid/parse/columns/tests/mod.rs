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
