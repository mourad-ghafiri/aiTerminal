use super::super::super::{layout as lay, parse, Item, Scene};

fn px(s: &str) -> (u32, u32) {
    (s.chars().count() as u32 * 8, 16)
}
fn cells(s: &str) -> (u32, u32) {
    (s.chars().count() as u32, 1)
}

fn scene(src: &str, measure: &dyn Fn(&str) -> (u32, u32)) -> Scene {
    lay(&parse(src).unwrap(), measure)
}

fn labels(s: &Scene) -> Vec<String> {
    s.items
        .iter()
        .filter_map(|i| match i {
            Item::Label { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_pie_draws_wedges_in_pixels_and_bars_in_cells() {
    let src = "pie title Pets\n \"Dogs\" : 75\n \"Cats\" : 25";
    let p = scene(src, &px);
    assert_eq!(p.items.iter().filter(|i| matches!(i, Item::Wedge { .. })).count(), 2);
    let c = scene(src, &cells);
    assert_eq!(c.items.iter().filter(|i| matches!(i, Item::Wedge { .. })).count(), 0, "no sub-cell geometry in cells");
    assert!(labels(&c).iter().any(|l| l.contains("Dogs") && l.contains("75%")), "{:?}", labels(&c));
}

#[test]
fn an_xychart_draws_a_bar_per_value_and_labels_the_axis() {
    let s = scene("xychart-beta\n x-axis [a, b]\n bar [10, 20]", &px);
    assert_eq!(s.shapes().count(), 2);
    assert!(labels(&s).contains(&"a".to_string()));
    // The taller value gets the taller bar.
    let hs: Vec<f32> = s.shapes().map(|(r, _, _)| r.h).collect();
    assert!(hs[1] > hs[0]);
}

#[test]
fn a_gantt_scales_bars_to_the_plan() {
    let s = scene("gantt\n dateFormat YYYY-MM-DD\n section S\n A :a1, 2024-01-01, 10d\n B :after a1, 10d", &px);
    let ws: Vec<f32> = s.shapes().map(|(r, _, _)| r.w).collect();
    assert_eq!(ws.len(), 2);
    assert!((ws[0] - ws[1]).abs() < 1.0, "equal durations draw equal bars: {ws:?}");
    assert!(labels(&s).iter().any(|l| l.starts_with("2024-01-01")), "the span is stamped: {:?}", labels(&s));
}

#[test]
fn a_quadrant_places_points_by_value() {
    let s = scene("quadrantChart\n quadrant-1 Good\n A: [0.9, 0.9]\n B: [0.1, 0.1]", &px);
    let dots: Vec<(f32, f32)> = s
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Label { text, x, y, .. } if text == "●" => Some((*x, *y)),
            _ => None,
        })
        .collect();
    assert_eq!(dots.len(), 2);
    assert!(dots[0].0 > dots[1].0 && dots[0].1 < dots[1].1, "0.9,0.9 is up and to the right: {dots:?}");
}

#[test]
fn a_sankey_links_both_columns() {
    let s = scene("sankey-beta\n Coal,Power,25\n Gas,Power,15", &px);
    assert_eq!(s.node_labels(), vec!["Coal", "Gas", "Power"]);
    assert_eq!(s.paths().count(), 2);
}

#[test]
fn a_radar_is_a_ring_in_pixels_and_bars_in_cells() {
    let src = "radar-beta\n axis a[\"Speed\"], b[\"Power\"], c[\"Range\"]\n curve me[\"Me\"]{10, 20, 30}";
    let p = scene(src, &px);
    let ring = p.paths().any(|i| matches!(i, Item::Path { points, .. } if points.len() > 3));
    assert!(ring, "the curve closes into a ring");
    let c = scene(src, &cells);
    assert!(c.shapes().count() >= 3, "one bar per axis in cells");
}

#[test]
fn a_packet_lists_its_fields() {
    let s = scene("packet-beta\n title TCP\n 0-15: \"Source Port\"", &px);
    assert!(labels(&s).contains(&"Source Port".to_string()));
    assert!(labels(&s).contains(&"0-15".to_string()));
}

#[test]
fn empty_data_is_an_empty_scene_not_a_panic() {
    for src in ["pie", "xychart-beta", "gantt", "sankey-beta", "radar-beta", "quadrantChart"] {
        let _ = scene(src, &px);
    }
}
