//! Pure diagram layout: [`Diagram`] → [`Scene`].
//!
//! Text sizing is injected via a `measure` closure, so this stays free of any font/OS
//! dependency — and every spacing constant is expressed in **em units** derived from that
//! closure rather than hardcoded pixels. That is what lets one layout serve two very
//! different rasterizers: the GPU renderer measures in pixels (`8×16`-ish), the text
//! renderer measures in character cells (`1×1`), and both get proportionate geometry.

mod flow;
pub(crate) mod layered;
mod sequence;

use super::scene::{Scene, Shape};
use super::Diagram;

/// A text-measuring closure: `measure(text) -> (width, height)` in the host's units.
pub(crate) type Measure<'a> = &'a dyn Fn(&str) -> (u32, u32);

/// Spacing for one layout pass, in the units the `measure` closure reports.
pub(crate) struct Metrics {
    /// Width of one em, height of one line.
    pub ew: f32,
    pub eh: f32,
    pub pad_x: f32,
    pub pad_y: f32,
    pub rank_gap: f32,
    pub node_gap: f32,
    pub margin: f32,
    pub min_w: f32,
    pub min_h: f32,
}

impl Metrics {
    pub fn new(measure: Measure) -> Self {
        let (ew, eh) = measure("M");
        let (ew, eh) = (ew.max(1) as f32, eh.max(1) as f32);
        Metrics {
            ew,
            eh,
            // `max(_, 1.0)` keeps a border's worth of room in cell units, where half an em
            // would round away to nothing.
            pad_x: (2.0 * ew).max(1.0),
            pad_y: (0.5 * eh).max(1.0),
            rank_gap: 3.0 * eh,
            node_gap: 3.0 * ew,
            margin: eh,
            min_w: 5.0 * ew,
            min_h: 2.0 * eh,
        }
    }

    /// The extent of a (possibly multi-line) label.
    pub fn text_size(&self, label: &str, measure: Measure) -> (f32, f32) {
        let mut w = 0.0_f32;
        let mut lines = 0;
        for line in label.split('\n') {
            w = w.max(measure(line).0 as f32);
            lines += 1;
        }
        (w, lines.max(1) as f32 * self.eh)
    }

    /// The box a label needs in the given shape.
    pub fn node_size(&self, label: &str, shape: Shape, measure: Measure) -> (f32, f32) {
        let (tw, th) = self.text_size(label, measure);
        let mut w = tw + 2.0 * self.pad_x;
        let mut h = th + 2.0 * self.pad_y;
        // Diamonds, hexagons and circles waste their corners, so the text needs more room.
        // The vertical bonus is floored, so in character cells — where half a line rounds
        // to nothing — a circle stays three rows rather than growing a blank one.
        if matches!(shape, Shape::Diamond | Shape::Hexagon | Shape::Circle | Shape::DoubleCircle) {
            w += self.pad_x;
            h += (0.5 * self.eh).floor();
        }
        (w.max(self.min_w), h.max(self.min_h))
    }
}

/// Lay out any diagram. `measure(text) -> (w, h)` gives a label's rendered extent.
pub fn layout(d: &Diagram, measure: Measure) -> Scene {
    let m = Metrics::new(measure);
    match d {
        Diagram::Flow(f) => flow::layout(f, &m, measure),
        Diagram::Sequence(s) => sequence::layout(s, &m, measure),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{layout as lay_any, parse, Item, Scene, Stroke};
    use crate::types::Rect;

    // A deterministic pixel stub: 8px per column, 16px per line.
    fn stub(s: &str) -> (u32, u32) {
        (s.chars().count() as u32 * 8, 16)
    }
    // The text renderer's view: one cell per column, one per line.
    fn cells(s: &str) -> (u32, u32) {
        (s.chars().count() as u32, 1)
    }

    fn lay(src: &str) -> Scene {
        lay_any(&parse(src).unwrap(), &stub)
    }

    fn boxes(s: &Scene) -> Vec<Rect> {
        s.shapes().map(|(r, _, _)| *r).collect()
    }

    #[test]
    fn flowchart_layout_is_sized_and_non_overlapping() {
        let l = lay("flowchart TD\n A[Start] --> B[Middle]\n B --> C[End]");
        let b = boxes(&l);
        assert_eq!(b.len(), 3);
        assert_eq!(l.paths().count(), 2);
        assert!(b[1].y > b[0].y, "B below A");
        assert!(b[2].y > b[1].y, "C below B");
        for r in &b {
            assert!(r.x >= 0.0 && r.y >= 0.0 && r.right() <= l.width as f32 + 1.0 && r.bottom() <= l.height as f32 + 1.0);
        }
    }

    #[test]
    fn lr_lays_out_horizontally() {
        let b = boxes(&lay("flowchart LR\n A --> B"));
        assert!(b[1].x > b[0].x, "B to the right of A");
    }

    #[test]
    fn siblings_dont_overlap_within_a_rank() {
        let b = boxes(&lay("flowchart TD\n A --> B\n A --> C"));
        assert!(b[1].right() <= b[2].x + 0.1 || b[2].right() <= b[1].x + 0.1, "B and C overlap: {:?} {:?}", b[1], b[2]);
    }

    #[test]
    fn a_dashed_edge_keeps_its_stroke() {
        let l = lay("flowchart TD\n A -.-> B");
        let Item::Path { stroke, .. } = l.paths().next().unwrap() else { unreachable!() };
        assert_eq!(*stroke, Stroke::Dashed);
    }

    #[test]
    fn sequence_layout_has_actors_lifelines_and_messages() {
        let l = lay("sequenceDiagram\n A->>B: Hi\n B-->>A: Yo");
        assert_eq!(l.node_labels(), vec!["A", "B"]);
        assert_eq!(l.paths().count(), 4, "two lifelines + two messages");
    }

    #[test]
    fn multiline_labels_make_taller_boxes() {
        let one = boxes(&lay("flowchart TD\n A[one]"))[0];
        let two = boxes(&lay("flowchart TD\n A[\"one<br/>two\"]"))[0];
        assert!(two.h > one.h, "a two-line label is taller: {two:?} vs {one:?}");
    }

    #[test]
    fn metrics_scale_with_the_measure_unit() {
        let px = lay("flowchart TD\n A --> B");
        let cell = lay_any(&parse("flowchart TD\n A --> B").unwrap(), &cells);
        assert!(cell.width < px.width / 4, "cell {} vs px {}", cell.width, px.width);
        assert!(cell.height >= 6, "still tall enough for two bordered boxes: {}", cell.height);
    }

    #[test]
    fn empty_is_zero_and_no_panic() {
        assert_eq!(lay_any(&parse("flowchart TD").unwrap(), &stub), Scene::default());
    }
}
