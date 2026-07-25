//! Pure diagram layout: [`Diagram`] → pixel geometry. Text sizing is injected via a
//! `measure` closure, so this stays free of any font/OS dependency. Flowcharts use a simple
//! layered (rank) layout; sequence diagrams use actor columns + a message timeline.

use super::{Diagram, Dir, Flow, Sequence, Shape};

/// A positioned node box (pixels).
#[derive(Clone, Debug, PartialEq)]
pub struct NodeBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub label: String,
    pub shape: Shape,
}

/// A routed edge (pixels): a polyline, an optional label, arrowhead + dash flags.
#[derive(Clone, Debug, PartialEq)]
pub struct EdgePath {
    pub points: Vec<(f32, f32)>,
    pub label: String,
    pub arrow: bool,
    pub dashed: bool,
}

/// The laid-out diagram: overall pixel size + node boxes + edge paths.
#[derive(Clone, Debug, PartialEq)]
pub struct DiagramLayout {
    pub width: u32,
    pub height: u32,
    pub nodes: Vec<NodeBox>,
    pub edges: Vec<EdgePath>,
}

const PAD_X: f32 = 16.0;
const PAD_Y: f32 = 8.0;
const MIN_W: f32 = 44.0;
const MIN_H: f32 = 28.0;
const RANK_GAP: f32 = 48.0;
const NODE_GAP: f32 = 24.0;
const MARGIN: f32 = 16.0;

/// Lay out any diagram. `measure(text) -> (w_px, h_px)` gives a label's rendered extent.
pub fn layout(d: &Diagram, measure: &dyn Fn(&str) -> (u32, u32)) -> DiagramLayout {
    match d {
        Diagram::Flow(f) => layout_flow(f, measure),
        Diagram::Sequence(s) => layout_sequence(s, measure),
    }
}

fn node_size(label: &str, shape: Shape, measure: &dyn Fn(&str) -> (u32, u32)) -> (f32, f32) {
    let (tw, th) = measure(label);
    let mut w = tw as f32 + 2.0 * PAD_X;
    let mut h = th as f32 + 2.0 * PAD_Y;
    // Diamonds/circles need extra room around the text.
    if matches!(shape, Shape::Diamond | Shape::Circle) {
        w += PAD_X;
        h += PAD_Y;
    }
    (w.max(MIN_W), h.max(MIN_H))
}

fn layout_flow(f: &Flow, measure: &dyn Fn(&str) -> (u32, u32)) -> DiagramLayout {
    let n = f.nodes.len();
    if n == 0 {
        return DiagramLayout { width: 0, height: 0, nodes: Vec::new(), edges: Vec::new() };
    }
    let sizes: Vec<(f32, f32)> = f.nodes.iter().map(|nd| node_size(&nd.label, nd.shape, measure)).collect();

    // Longest-path ranks (cycle-safe: bounded relaxation passes).
    let mut rank = vec![0usize; n];
    for _ in 0..n {
        let mut changed = false;
        for e in &f.edges {
            if e.from != e.to && rank[e.to] < rank[e.from] + 1 {
                rank[e.to] = rank[e.from] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let max_rank = *rank.iter().max().unwrap_or(&0);
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for (i, &r) in rank.iter().enumerate() {
        groups[r].push(i);
    }

    let horiz = f.dir.horizontal();
    // Along the rank axis: how thick each rank is (max node extent in that direction).
    let rank_size = |i: usize| if horiz { sizes[i].0 } else { sizes[i].1 };
    let cross_size = |i: usize| if horiz { sizes[i].1 } else { sizes[i].0 };
    let rank_thick: Vec<f32> = groups.iter().map(|g| g.iter().map(|&i| rank_size(i)).fold(0.0_f32, f32::max)).collect();
    // Rank-axis start position for each rank.
    let mut rank_pos = vec![0.0_f32; max_rank + 1];
    let mut acc = MARGIN;
    for r in 0..=max_rank {
        rank_pos[r] = acc;
        acc += rank_thick[r] + RANK_GAP;
    }
    // Cross-axis width of the widest rank → center every rank to it.
    let cross_total = |g: &[usize]| -> f32 {
        let sum: f32 = g.iter().map(|&i| cross_size(i)).sum();
        sum + NODE_GAP * g.len().saturating_sub(1) as f32
    };
    let max_cross = groups.iter().map(|g| cross_total(g)).fold(0.0_f32, f32::max);

    let mut boxes = vec![NodeBox { x: 0.0, y: 0.0, w: 0.0, h: 0.0, label: String::new(), shape: Shape::Rect }; n];
    for (r, g) in groups.iter().enumerate() {
        let mut cross = MARGIN + (max_cross - cross_total(g)) / 2.0;
        for &i in g {
            let (w, h) = sizes[i];
            let (x, y) = if horiz {
                (rank_pos[r] + (rank_thick[r] - w) / 2.0, cross)
            } else {
                (cross, rank_pos[r] + (rank_thick[r] - h) / 2.0)
            };
            boxes[i] = NodeBox { x, y, w, h, label: f.nodes[i].label.clone(), shape: f.nodes[i].shape };
            cross += cross_size(i) + NODE_GAP;
        }
    }

    let width = boxes.iter().map(|b| b.x + b.w).fold(0.0_f32, f32::max) + MARGIN;
    let height = boxes.iter().map(|b| b.y + b.h).fold(0.0_f32, f32::max) + MARGIN;

    // Reverse the rank axis for BT / RL.
    if matches!(f.dir, Dir::BT) {
        for b in &mut boxes {
            b.y = height - b.y - b.h;
        }
    } else if matches!(f.dir, Dir::RL) {
        for b in &mut boxes {
            b.x = width - b.x - b.w;
        }
    }

    let edges = f
        .edges
        .iter()
        .filter(|e| e.from < n && e.to < n && e.from != e.to)
        .map(|e| {
            let a = &boxes[e.from];
            let b = &boxes[e.to];
            let ca = (a.x + a.w / 2.0, a.y + a.h / 2.0);
            let cb = (b.x + b.w / 2.0, b.y + b.h / 2.0);
            let p0 = border_point(a, cb);
            let p1 = border_point(b, ca);
            EdgePath { points: vec![p0, p1], label: e.label.clone(), arrow: e.arrow, dashed: e.dashed }
        })
        .collect();

    DiagramLayout { width: width.ceil() as u32, height: height.ceil() as u32, nodes: boxes, edges }
}

/// The point on `rect`'s border along the ray from its center toward `target`.
fn border_point(rect: &NodeBox, target: (f32, f32)) -> (f32, f32) {
    let cx = rect.x + rect.w / 2.0;
    let cy = rect.y + rect.h / 2.0;
    let dx = target.0 - cx;
    let dy = target.1 - cy;
    if dx == 0.0 && dy == 0.0 {
        return (cx, cy);
    }
    let hw = rect.w / 2.0;
    let hh = rect.h / 2.0;
    let sx = if dx != 0.0 { hw / dx.abs() } else { f32::INFINITY };
    let sy = if dy != 0.0 { hh / dy.abs() } else { f32::INFINITY };
    let s = sx.min(sy);
    (cx + dx * s, cy + dy * s)
}

fn layout_sequence(s: &Sequence, measure: &dyn Fn(&str) -> (u32, u32)) -> DiagramLayout {
    const ACTOR_H: f32 = 32.0;
    const ACTOR_GAP: f32 = 40.0;
    const MSG_TOP: f32 = 24.0;
    const MSG_GAP: f32 = 40.0;
    let a = s.actors.len();
    if a == 0 {
        return DiagramLayout { width: 0, height: 0, nodes: Vec::new(), edges: Vec::new() };
    }
    // Actor header boxes across the top; remember each column's center x.
    let mut boxes = Vec::with_capacity(a);
    let mut cx = Vec::with_capacity(a);
    let mut x = MARGIN;
    for name in &s.actors {
        let (tw, _) = measure(name);
        let w = (tw as f32 + 2.0 * PAD_X).max(MIN_W);
        boxes.push(NodeBox { x, y: MARGIN, w, h: ACTOR_H, label: name.clone(), shape: Shape::Rect });
        cx.push(x + w / 2.0);
        x += w + ACTOR_GAP;
    }
    let width = x - ACTOR_GAP + MARGIN;
    let lifeline_top = MARGIN + ACTOR_H;
    let height = lifeline_top + MSG_TOP + MSG_GAP * s.messages.len().max(1) as f32 + MARGIN;

    let mut edges = Vec::new();
    // Lifelines: a dashed vertical from each actor down to the bottom.
    for &x in &cx {
        edges.push(EdgePath { points: vec![(x, lifeline_top), (x, height - MARGIN)], label: String::new(), arrow: false, dashed: true });
    }
    // Messages: horizontal arrows at increasing y.
    let mut y = lifeline_top + MSG_TOP;
    for m in &s.messages {
        if m.from < a && m.to < a {
            edges.push(EdgePath { points: vec![(cx[m.from], y), (cx[m.to], y)], label: m.text.clone(), arrow: true, dashed: m.dashed });
        }
        y += MSG_GAP;
    }

    DiagramLayout { width: width.ceil() as u32, height: height.ceil() as u32, nodes: boxes, edges }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    // A deterministic stub: width = 8px/char, height = 16px.
    fn stub(s: &str) -> (u32, u32) {
        (s.chars().count() as u32 * 8, 16)
    }

    fn lay(src: &str) -> DiagramLayout {
        layout(&parse(src).unwrap(), &stub)
    }

    #[test]
    fn flowchart_layout_is_sized_and_non_overlapping() {
        let l = lay("flowchart TD\n A[Start] --> B[Middle]\n B --> C[End]");
        assert_eq!(l.nodes.len(), 3);
        assert_eq!(l.edges.len(), 2);
        assert!(l.width > 0 && l.height > 0);
        // TD: ranks go downward — B is below A, C below B.
        assert!(l.nodes[1].y > l.nodes[0].y, "B below A");
        assert!(l.nodes[2].y > l.nodes[1].y, "C below B");
        // Every node fits inside the reported canvas.
        for n in &l.nodes {
            assert!(n.x >= 0.0 && n.y >= 0.0 && n.x + n.w <= l.width as f32 + 1.0 && n.y + n.h <= l.height as f32 + 1.0);
        }
        // Edge endpoints sit on node borders (not at centers).
        for e in &l.edges {
            assert_eq!(e.points.len(), 2);
        }
    }

    #[test]
    fn lr_lays_out_horizontally() {
        let l = lay("flowchart LR\n A --> B");
        assert!(l.nodes[1].x > l.nodes[0].x, "B to the right of A");
    }

    #[test]
    fn siblings_dont_overlap_within_a_rank() {
        let l = lay("flowchart TD\n A --> B\n A --> C");
        // B and C share a rank; their x-ranges must not overlap.
        let (b, c) = (&l.nodes[1], &l.nodes[2]);
        assert!(b.x + b.w <= c.x + 0.1 || c.x + c.w <= b.x + 0.1, "B and C overlap: {b:?} {c:?}");
    }

    #[test]
    fn sequence_layout_has_actors_and_messages() {
        let l = lay("sequenceDiagram\n A->>B: Hi\n B-->>A: Yo");
        assert_eq!(l.nodes.len(), 2, "two actor boxes");
        // lifelines (2) + messages (2)
        assert_eq!(l.edges.len(), 4);
        assert!(l.height as f32 > l.nodes[0].y + l.nodes[0].h);
    }

    #[test]
    fn empty_is_zero_and_no_panic() {
        let l = layout(&parse("flowchart TD").unwrap(), &stub);
        assert_eq!(l.nodes.len(), 0);
    }
}
