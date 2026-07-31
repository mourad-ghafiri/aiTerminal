use super::super::super::{parse as parse_any, Chart, ChartKind, Diagram};

fn chart(src: &str) -> Chart {
    match parse_any(src) {
        Some(Diagram::Chart(c)) => c,
        other => panic!("expected a chart, got {other:?}"),
    }
}

#[test]
fn a_pie_reads_its_slices() {
    let c = chart("pie title Pets\n \"Dogs\" : 386\n \"Cats\" : 85");
    assert_eq!(c.kind, ChartKind::Pie);
    assert_eq!(c.title, "Pets");
    assert_eq!(c.categories, vec!["Dogs", "Cats"]);
    assert_eq!(c.series[0].values, vec![386.0, 85.0]);
}

#[test]
fn an_xychart_reads_axes_bars_and_lines() {
    let c = chart("xychart-beta\n title \"Sales\"\n x-axis [jan, feb, mar]\n y-axis \"Revenue\" 0 --> 100\n bar [5, 10, 15]\n line [3, 8, 12]");
    assert_eq!(c.title, "Sales");
    assert_eq!(c.categories, vec!["jan", "feb", "mar"]);
    assert_eq!(c.y_title, "Revenue");
    assert_eq!(c.series.len(), 2);
    assert!(!c.series[0].line && c.series[1].line);
}

#[test]
fn a_quadrant_reads_captions_and_points() {
    let c = chart("quadrantChart\n title Reach\n x-axis Low --> High\n quadrant-1 Expand\n quadrant-3 Drop\n Campaign A: [0.3, 0.6]");
    assert_eq!(c.quadrants[0], "Expand");
    assert_eq!(c.quadrants[2], "Drop");
    assert_eq!(c.points.len(), 1);
    assert_eq!((c.points[0].x, c.points[0].y), (0.3, 0.6));
}

#[test]
fn gantt_dates_durations_and_dependencies() {
    let c = chart("gantt\n title Plan\n dateFormat YYYY-MM-DD\n section Design\n Draft :a1, 2024-01-01, 10d\n Review :after a1, 5d");
    assert_eq!(c.tasks.len(), 2);
    assert_eq!(c.tasks[0].section, "Design");
    assert_eq!(c.tasks[0].end - c.tasks[0].start, 10 * 86_400);
    assert_eq!(c.tasks[1].start, c.tasks[0].end, "`after` starts where the other ended");
    assert_eq!(c.tasks[1].end - c.tasks[1].start, 5 * 86_400);
}

#[test]
fn gantt_states_and_milestones() {
    let c = chart("gantt\n section S\n Done thing :done, d1, 2024-01-01, 3d\n Now :active, crit, 2d\n Ship :milestone, 2024-02-01, 0d");
    assert!(c.tasks[0].done);
    assert!(c.tasks[1].active && c.tasks[1].critical);
    assert!(c.tasks[2].milestone);
}

#[test]
fn a_sankey_reads_its_flows() {
    let c = chart("sankey-beta\n Coal,Electricity,25\n Gas,Electricity,15");
    assert_eq!(c.flows.len(), 2);
    assert_eq!(c.flows[0], ("Coal".into(), "Electricity".into(), 25.0));
}

#[test]
fn a_radar_reads_axes_and_curves() {
    let c = chart("radar-beta\n axis a[\"Speed\"], b[\"Power\"]\n curve me[\"Me\"]{10, 20}");
    assert_eq!(c.categories, vec!["Speed", "Power"]);
    assert_eq!(c.series[0].name, "Me");
    assert_eq!(c.series[0].values, vec![10.0, 20.0]);
}

#[test]
fn a_treemap_keeps_its_sections() {
    let c = chart("treemap-beta\n \"Section 1\"\n   \"Leaf 1.1\": 12\n   \"Leaf 1.2\": 24");
    assert_eq!(c.categories, vec!["Section 1 · Leaf 1.1", "Section 1 · Leaf 1.2"]);
    assert_eq!(c.series[0].values, vec![12.0, 24.0]);
}

#[test]
fn a_packet_reads_its_fields() {
    let c = chart("packet-beta\n title TCP\n 0-15: \"Source Port\"\n 16-31: \"Destination Port\"");
    assert_eq!(c.title, "TCP");
    assert_eq!(c.rows.len(), 2);
    assert_eq!(c.rows[0], ("0-15".into(), "Source Port".into()));
}

#[test]
fn junk_never_becomes_data() {
    let c = chart("pie\n not a slice\n \"Real\" : 5");
    assert_eq!(c.categories, vec!["Real"]);
}
