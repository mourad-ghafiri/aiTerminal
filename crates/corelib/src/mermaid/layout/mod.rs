//! Pure diagram layout: [`Diagram`] → [`Scene`].
//!
//! Text sizing is injected via a `measure` closure, so this stays free of any font/OS
//! dependency — and every spacing constant is expressed in **em units** derived from that
//! closure rather than hardcoded pixels. That is what lets one layout serve two very
//! different rasterizers: the GPU renderer measures in pixels (`8×16`-ish), the text
//! renderer measures in character cells (`1×1`), and both get proportionate geometry.

mod chart;
mod columns;
mod flow;
mod graph;
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

    /// True when the host measures in character cells rather than pixels — the signal
    /// that sub-cell geometry (a pie's wedges, a radar's polygon) has to become something
    /// a character grid can actually show.
    pub fn cells(&self) -> bool {
        self.ew <= 2.0 && self.eh <= 2.0
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
        Diagram::Graph(g) => graph::layout(g, &m, measure),
        Diagram::Columns(c) => columns::layout(c, &m, measure),
        Diagram::Chart(c) => chart::layout(c, &m, measure),
    }
}

#[cfg(test)]
mod tests;
