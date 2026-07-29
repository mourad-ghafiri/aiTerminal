//! The render-agnostic display list every diagram lays out into.
//!
//! A [`Scene`] is pure geometry + *roles* — never colors. The GPU renderer maps a
//! [`Role`] onto the active theme, and the text renderer maps it onto box-drawing
//! glyphs, so one layout serves both and a theme switch restyles every diagram for
//! free. Charts index a categorical palette through [`Role::Slot`].

use crate::types::Rect;

/// A node/box outline. The bracket style in the source picks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Rect,             // [ ]
    Round,            // ( )
    Stadium,          // ([ ])
    Subroutine,       // [[ ]]
    Cylinder,         // [( )]
    Circle,           // (( ))
    DoubleCircle,     // ((( )))
    Asymmetric,       // > ]
    Diamond,          // { }
    Hexagon,          // {{ }}
    Parallelogram,    // [/ /]
    ParallelogramAlt, // [\ \]
    Trapezoid,        // [/ \]
    TrapezoidAlt,     // [\ /]
    /// A sticky note (sequence notes, flowchart annotations).
    Note,
    /// A sequence-diagram actor drawn as a stick figure rather than a box.
    Actor,
}

impl Shape {
    /// True when the outline is rounded on its left/right ends.
    pub fn is_pill(self) -> bool {
        matches!(self, Shape::Round | Shape::Stadium | Shape::Circle | Shape::DoubleCircle)
    }
}

/// How a line is stroked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stroke {
    Solid,
    Dashed,
    Dotted,
    Thick,
}

/// What sits at the end of a line. Covers flowchart arrows, UML relations and ER
/// cardinality in one vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cap {
    None,
    /// A filled triangle — the ordinary `-->` arrow.
    Arrow,
    /// An open "V" — mermaid's `->>` message head.
    Open,
    /// `--x`
    Cross,
    /// `--o`, and UML aggregation's hollow end when drawn small.
    Circle,
    /// A hollow triangle — UML inheritance/realization.
    Triangle,
    /// A hollow diamond — UML aggregation.
    Diamond,
    /// A filled diamond — UML composition.
    FilledDiamond,
    /// ER "many" — the crow's foot.
    CrowFoot,
    /// ER "one" — a single tick across the line.
    Tick,
}

/// Text alignment relative to its anchor point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

/// The relative size a label is drawn at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextSize {
    Title,
    Normal,
    Small,
}

/// What an item *means*, which is how it gets its color. `Slot(n)` is a categorical
/// series index (pie slices, gantt sections, journey scores) resolved by the renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Node,
    Edge,
    Label,
    Muted,
    Accent,
    Slot(u8),
}

/// One drawable.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// A node outline with an optional centered label.
    Shape { kind: Shape, rect: Rect, label: String, role: Role },
    /// A titled frame around other items (subgraph, sequence `box`, `loop`/`alt`, C4 boundary).
    Group { rect: Rect, title: String, role: Role },
    /// A polyline with end caps and an optional mid-line label.
    Path { points: Vec<(f32, f32)>, stroke: Stroke, tail: Cap, head: Cap, label: String, role: Role },
    /// A pie/radar wedge, angles in radians clockwise from 12 o'clock.
    Wedge { cx: f32, cy: f32, r: f32, a0: f32, a1: f32, slot: u8 },
    /// Free-standing text (titles, axis ticks, class members, legends).
    Label { text: String, x: f32, y: f32, anchor: Anchor, size: TextSize, role: Role },
    /// A thin divider (class compartments, chart axes).
    Rule { a: (f32, f32), b: (f32, f32), role: Role },
}

/// A laid-out diagram: an overall extent plus the items to draw, back to front.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub width: u32,
    pub height: u32,
    pub items: Vec<Item>,
}

impl Scene {
    /// The labels of the diagram's *nodes*, in layout order — what a test asserts to prove
    /// a diagram was understood, independent of pixels. Decoration that happens to be a
    /// shape (a note, an activation bar) carries another role and is left out.
    pub fn node_labels(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Shape { label, role: Role::Node, .. } if !label.is_empty() => Some(label.clone()),
                _ => None,
            })
            .collect()
    }

    /// Every shape rectangle, in layout order.
    pub fn shapes(&self) -> impl Iterator<Item = (&Rect, &str, Shape)> {
        self.items.iter().filter_map(|i| match i {
            Item::Shape { rect, label, kind, .. } => Some((rect, label.as_str(), *kind)),
            _ => None,
        })
    }

    /// Every path polyline, in layout order.
    pub fn paths(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| matches!(i, Item::Path { .. }))
    }

    /// Grow the recorded extent to contain `rect` (plus `margin` on the far edges).
    pub fn fit(&mut self, rect: Rect, margin: f32) {
        self.width = self.width.max((rect.right() + margin).ceil().max(0.0) as u32);
        self.height = self.height.max((rect.bottom() + margin).ceil().max(0.0) as u32);
    }
}

/// A [`Scene`] under construction. Keeps layout code declarative — push shapes and
/// paths, and the extent tracks itself.
#[derive(Debug, Default)]
pub struct Builder {
    items: Vec<Item>,
    w: f32,
    h: f32,
    margin: f32,
}

impl Builder {
    /// A builder whose extent always leaves `margin` beyond the furthest item.
    pub fn new(margin: f32) -> Self {
        Builder { items: Vec::new(), w: 0.0, h: 0.0, margin }
    }

    fn grow(&mut self, x: f32, y: f32) {
        self.w = self.w.max(x);
        self.h = self.h.max(y);
    }

    pub fn shape(&mut self, kind: Shape, rect: Rect, label: impl Into<String>, role: Role) {
        self.grow(rect.right(), rect.bottom());
        self.items.push(Item::Shape { kind, rect, label: label.into(), role });
    }

    pub fn group(&mut self, rect: Rect, title: impl Into<String>, role: Role) {
        self.grow(rect.right(), rect.bottom());
        // Frames go behind everything drawn so far so their fill can't hide nodes.
        self.items.insert(0, Item::Group { rect, title: title.into(), role });
    }

    pub fn path(&mut self, points: Vec<(f32, f32)>, stroke: Stroke, tail: Cap, head: Cap, label: impl Into<String>, role: Role) {
        for &(x, y) in &points {
            self.grow(x, y);
        }
        self.items.push(Item::Path { points, stroke, tail, head, label: label.into(), role });
    }

    /// The common case: a two-point line with an arrowhead at `b`.
    pub fn arrow(&mut self, a: (f32, f32), b: (f32, f32), stroke: Stroke, label: impl Into<String>) {
        self.path(vec![a, b], stroke, Cap::None, Cap::Arrow, label, Role::Edge);
    }

    pub fn wedge(&mut self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, slot: u8) {
        self.grow(cx + r, cy + r);
        self.items.push(Item::Wedge { cx, cy, r, a0, a1, slot });
    }

    pub fn label(&mut self, text: impl Into<String>, x: f32, y: f32, anchor: Anchor, size: TextSize, role: Role) {
        self.grow(x, y);
        self.items.push(Item::Label { text: text.into(), x, y, anchor, size, role });
    }

    pub fn rule(&mut self, a: (f32, f32), b: (f32, f32), role: Role) {
        self.grow(a.0.max(b.0), a.1.max(b.1));
        self.items.push(Item::Rule { a, b, role });
    }

    /// Finish, sizing the scene to the furthest item plus the margin.
    pub fn build(self) -> Scene {
        if self.items.is_empty() {
            return Scene::default();
        }
        Scene {
            width: (self.w + self.margin).ceil().max(0.0) as u32,
            height: (self.h + self.margin).ceil().max(0.0) as u32,
            items: self.items,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_sizes_to_the_furthest_item_plus_margin() {
        let mut b = Builder::new(10.0);
        b.shape(Shape::Rect, Rect::new(0.0, 0.0, 40.0, 20.0), "A", Role::Node);
        b.shape(Shape::Rect, Rect::new(60.0, 30.0, 40.0, 20.0), "B", Role::Node);
        let s = b.build();
        assert_eq!((s.width, s.height), (110, 60));
        assert_eq!(s.node_labels(), vec!["A", "B"]);
    }

    #[test]
    fn an_empty_builder_is_a_zero_scene() {
        assert_eq!(Builder::new(8.0).build(), Scene::default());
    }

    #[test]
    fn groups_sit_behind_the_items_they_frame() {
        let mut b = Builder::new(4.0);
        b.shape(Shape::Rect, Rect::new(10.0, 10.0, 20.0, 10.0), "inner", Role::Node);
        b.group(Rect::new(0.0, 0.0, 40.0, 30.0), "frame", Role::Muted);
        let s = b.build();
        assert!(matches!(s.items[0], Item::Group { .. }), "the frame draws first");
    }

    #[test]
    fn paths_extend_the_extent() {
        let mut b = Builder::new(0.0);
        b.arrow((0.0, 0.0), (50.0, 25.0), Stroke::Solid, "");
        let s = b.build();
        assert_eq!((s.width, s.height), (50, 25));
        assert_eq!(s.paths().count(), 1);
    }
}
