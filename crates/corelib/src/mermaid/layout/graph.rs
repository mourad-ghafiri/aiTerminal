//! Layout for every box-and-arrow language other than the flowchart: class, state, ER,
//! requirement, C4, architecture, block and mindmap.
//!
//! They differ only in what a box *contains* — a class has compartments, an ER entity has
//! attribute rows, a C4 element has a description — so one layout serves all of them: the
//! layered engine positions the boxes, and each box draws its name over a rule over its
//! rows.

use super::super::scene::{Anchor, Builder, Role, Scene, TextSize};
use super::super::{GraphDiagram, GraphKind};
use super::layered::{self, Graph};
use super::{Measure, Metrics};
use crate::types::Rect;

pub(crate) fn layout(d: &GraphDiagram, m: &Metrics, measure: Measure) -> Scene {
    let n = d.nodes.len();
    if n == 0 {
        return Scene::default();
    }
    let sizes: Vec<(f32, f32)> = d.nodes.iter().map(|nd| node_size(d, nd, m, measure)).collect();
    let mut g = Graph::new(sizes);
    g.group = d.nodes.iter().map(|nd| nd.group).collect();
    g.edges = d.edges.iter().map(|e| (e.from, e.to, e.min_len)).collect();
    // In a git graph a group *is* a lane: every commit on a branch belongs on that
    // branch's row, however far along the history it sits.
    if d.kind == GraphKind::Git {
        g.lane = g.group.clone();
    }

    let rank = layered::ranks(&g);
    let ranks = layered::order(&g, &rank);
    let boxes = layered::place(&g, &ranks, d.dir, m);
    let title_h = if d.title.is_empty() { 0.0 } else { 2.0 * m.eh };

    let mut sb = Builder::new(m.margin);
    for e in &d.edges {
        if e.from >= n || e.to >= n {
            continue;
        }
        let points: Vec<(f32, f32)> = layered::route(&boxes[e.from], &boxes[e.to], d.dir, m).into_iter().map(|(x, y)| (x, y + title_h)).collect();
        sb.path(points, e.stroke, e.tail, e.head, e.label.clone(), Role::Edge);
    }
    for (i, nd) in d.nodes.iter().enumerate() {
        let r = Rect::new(boxes[i].x, boxes[i].y + title_h, boxes[i].w, boxes[i].h);
        if nd.rows.is_empty() {
            sb.shape(nd.shape, r, nd.label.clone(), Role::Node);
            continue;
        }
        // A compartment box: the name on top, a rule under it, then one line per row. The
        // rule stops inside the border on both sides, so it reads as a divider rather than
        // as a line leaving the box.
        sb.shape(nd.shape, r, String::new(), Role::Node);
        let mut y = r.y + m.pad_y;
        sb.label(nd.label.clone(), r.x + r.w / 2.0, y, Anchor::Middle, TextSize::Normal, Role::Label);
        y += m.eh;
        sb.rule((r.x, y), (r.right() - 1.0, y), Role::Muted);
        y += m.pad_y;
        for row in &nd.rows {
            sb.label(row.clone(), r.x + m.pad_x * 0.5, y, Anchor::Start, TextSize::Small, Role::Label);
            y += m.eh;
        }
    }
    // Frames (namespaces, composite states, boundaries) wrap their members.
    let mut order: Vec<usize> = (0..d.groups.len()).collect();
    order.sort_by_key(|&i| depth_of(d, i));
    for &gi in &order {
        let members: Vec<Rect> = (0..n)
            .filter(|&i| in_group(d, i, gi))
            .map(|i| Rect::new(boxes[i].x, boxes[i].y + title_h, boxes[i].w, boxes[i].h))
            .collect();
        let title_w = m.text_size(&d.groups[gi].title, measure).0 + 4.0 * m.ew;
        let Some(r) = layered::bounds(&members, m.pad_x) else { continue };
        if d.kind == GraphKind::Git {
            // A branch is already legible as its own row of commits; a frame around it
            // would only add ink, so it just gets its name.
            sb.label(d.groups[gi].title.clone(), r.x, r.y - m.eh, Anchor::Start, TextSize::Small, Role::Muted);
        } else {
            sb.group(Rect::new(r.x, r.y - m.eh, r.w.max(title_w), r.h + m.eh), d.groups[gi].title.clone(), Role::Muted);
        }
    }
    if !d.title.is_empty() {
        let w = boxes.iter().map(|b| b.right()).fold(0.0_f32, f32::max);
        sb.label(d.title.clone(), w / 2.0, m.margin * 0.5, Anchor::Middle, TextSize::Title, Role::Label);
    }
    sb.build()
}

/// A box big enough for its name and every compartment row.
fn node_size(d: &GraphDiagram, nd: &super::super::GNode, m: &Metrics, measure: Measure) -> (f32, f32) {
    let (mut w, mut h) = m.node_size(&nd.label, nd.shape, measure);
    if nd.rows.is_empty() {
        return (w, h);
    }
    for row in &nd.rows {
        w = w.max(m.text_size(row, measure).0 + 2.0 * m.pad_x);
    }
    h += nd.rows.len() as f32 * m.eh + m.pad_y;
    // ER entities list attributes in two columns of text, so they need a little more room.
    if d.kind == GraphKind::Er {
        w += m.pad_x;
    }
    (w, h)
}

fn depth_of(d: &GraphDiagram, mut gi: usize) -> usize {
    let mut n = 0;
    while let Some(p) = d.groups[gi].parent {
        n += 1;
        if n > d.groups.len() {
            break;
        }
        gi = p;
    }
    n
}

fn in_group(d: &GraphDiagram, i: usize, gi: usize) -> bool {
    let mut cur = d.nodes[i].group;
    let mut guard = 0;
    while let Some(g) = cur {
        if g == gi {
            return true;
        }
        guard += 1;
        if guard > d.groups.len() {
            return false;
        }
        cur = d.groups[g].parent;
    }
    false
}
