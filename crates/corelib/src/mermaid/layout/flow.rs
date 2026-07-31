//! Flowchart layout: the layered engine plus subgraph frames.

use super::super::scene::{Builder, Role, Scene};
use super::super::Flow;
use super::layered::{self, Graph};
use super::{Measure, Metrics};
use crate::types::Rect;

pub(crate) fn layout(f: &Flow, m: &Metrics, measure: Measure) -> Scene {
    let n = f.nodes.len();
    if n == 0 {
        return Scene::default();
    }
    let mut g = Graph::new(f.nodes.iter().map(|nd| m.node_size(&nd.label, nd.shape, measure)).collect());
    g.group = f.nodes.iter().map(|nd| nd.group).collect();
    g.edges = f.edges.iter().map(|e| (e.from, e.to, e.min_len)).collect();

    let rank = layered::ranks(&g);
    let ranks = layered::order(&g, &rank);
    let mut boxes = layered::place(&g, &ranks, f.dir, m);

    // Subgraph frames need room for their own border and title, so the nodes inside them
    // move over to make it — outermost first, since a nested frame shifts its parent too.
    let depth = group_depths(f);
    let mut order: Vec<usize> = (0..f.groups.len()).collect();
    order.sort_by_key(|&i| depth[i]);
    let mut frames: Vec<Option<Rect>> = vec![None; f.groups.len()];
    for &gi in order.iter().rev() {
        let members: Vec<Rect> = (0..n).filter(|&i| in_group(f, i, gi)).map(|i| boxes[i]).collect();
        let inner: Vec<Rect> = frames.iter().enumerate().filter(|(j, _)| f.groups[*j].parent == Some(gi)).filter_map(|(_, r)| *r).collect();
        let all: Vec<Rect> = members.into_iter().chain(inner).collect();
        // The frame reserves a line above its members for the title, and never sits
        // narrower than the title it has to show.
        let title_w = m.text_size(&f.groups[gi].title, measure).0 + 4.0 * m.ew;
        frames[gi] = layered::bounds(&all, m.pad_x).map(|r| Rect::new(r.x, r.y - m.eh, r.w.max(title_w), r.h + m.eh));
    }

    let mut sb = Builder::new(m.margin);
    for e in &f.edges {
        if e.from >= n || e.to >= n {
            continue;
        }
        let points = layered::route(&boxes[e.from], &boxes[e.to], f.dir, m);
        sb.path(points, e.stroke, e.tail, e.head, e.label.clone(), Role::Edge);
    }
    for (i, nd) in f.nodes.iter().enumerate() {
        sb.shape(nd.shape, boxes[i], nd.label.clone(), Role::Node);
    }
    // Deepest frame first: `Builder::group` puts each new frame behind the last, so the
    // outermost ends up furthest back.
    for &gi in &order {
        if let Some(r) = frames[gi] {
            sb.group(r, f.groups[gi].title.clone(), Role::Muted);
        }
    }
    let mut scene = sb.build();
    // A mirrored direction can push boxes into the margin; keep the reported extent honest.
    for b in &mut boxes {
        scene.fit(*b, m.margin);
    }
    scene
}

/// How deeply each subgraph is nested (0 = top level).
fn group_depths(f: &Flow) -> Vec<usize> {
    let mut depth = vec![0usize; f.groups.len()];
    for i in 0..f.groups.len() {
        let mut d = 0;
        let mut cur = f.groups[i].parent;
        // Bounded by the group count, so a malformed parent chain cannot spin.
        while let Some(p) = cur {
            d += 1;
            if d > f.groups.len() {
                break;
            }
            cur = f.groups[p].parent;
        }
        depth[i] = d;
    }
    depth
}

/// Is node `i` inside subgraph `gi`, directly or through nesting?
fn in_group(f: &Flow, i: usize, gi: usize) -> bool {
    let mut cur = f.nodes[i].group;
    let mut guard = 0;
    while let Some(g) = cur {
        if g == gi {
            return true;
        }
        guard += 1;
        if guard > f.groups.len() {
            return false;
        }
        cur = f.groups[g].parent;
    }
    false
}

#[cfg(test)]
mod tests;
