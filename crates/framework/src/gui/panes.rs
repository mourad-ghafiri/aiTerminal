//! The multiplexer: a binary tree of split panes, and a list of such trees as
//! tabs. Generic over the per-pane content `T` (the app stores a terminal session
//! there). Pure logic — `layout` returns rectangles; the app renders them.
//!
//! Tree mutations are by-value transforms (consume a node, return a new one), so
//! there is no `unsafe` and no placeholder juggling.

use corelib::types::Rect;
use corelib::wire::Toml;

/// Split orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Children side by side (vertical divider): left | right.
    Horizontal,
    /// Children stacked (horizontal divider): top / bottom.
    Vertical,
}

/// A geometric focus-movement direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Stable identifier for a pane (leaf), unique within a [`PaneTree`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaneId(pub u64);

/// Gutter (px) between split panes.
pub const GUTTER: f32 = 6.0;

enum Node<T> {
    Leaf { id: PaneId, content: T },
    Split { axis: Axis, ratio: f32, first: Box<Node<T>>, second: Box<Node<T>> },
}

/// A binary tree of split panes with a focused leaf and optional zoom. `root` is
/// an `Option` only so it can be `.take()`n for by-value transforms; it is always
/// `Some` between calls.
pub struct PaneTree<T> {
    root: Option<Node<T>>,
    focus: PaneId,
    next_id: u64,
    zoom: Option<PaneId>,
}

impl<T> PaneTree<T> {
    pub fn new(content: T) -> Self {
        let id = PaneId(0);
        PaneTree { root: Some(Node::Leaf { id, content }), focus: id, next_id: 1, zoom: None }
    }

    fn root_ref(&self) -> &Node<T> {
        self.root.as_ref().expect("root present")
    }
    fn root_mut(&mut self) -> &mut Node<T> {
        self.root.as_mut().expect("root present")
    }

    fn alloc(&mut self) -> PaneId {
        let id = PaneId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn focused(&self) -> PaneId {
        self.focus
    }
    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        collect_ids(self.root_ref(), &mut v);
        v
    }
    pub fn get(&self, id: PaneId) -> Option<&T> {
        find(self.root_ref(), id)
    }
    pub fn get_mut(&mut self, id: PaneId) -> Option<&mut T> {
        find_mut(self.root_mut(), id)
    }
    pub fn focused_content(&self) -> Option<&T> {
        self.get(self.focus)
    }
    pub fn focused_content_mut(&mut self) -> Option<&mut T> {
        let f = self.focus;
        self.get_mut(f)
    }

    pub fn focus(&mut self, id: PaneId) -> bool {
        if find(self.root_ref(), id).is_some() {
            self.focus = id;
            true
        } else {
            false
        }
    }

    /// Split the focused pane along `axis`, inserting `content` as a new focused
    /// pane. Returns the new pane id.
    pub fn split(&mut self, axis: Axis, content: T) -> PaneId {
        let new_id = self.alloc();
        let target = self.focus;
        let root = self.root.take().expect("root present");
        let (new_root, leftover) = split_in(root, target, axis, new_id, content);
        // leftover is None when the target existed (always, here).
        debug_assert!(leftover.is_none());
        self.root = Some(new_root);
        self.focus = new_id;
        self.zoom = None;
        new_id
    }

    /// Close the focused pane. Returns its id (so the app drops its session), or
    /// `None` if it was the last pane in the tree.
    pub fn close_focused(&mut self) -> Option<PaneId> {
        if matches!(self.root, Some(Node::Leaf { .. })) {
            return None;
        }
        let target = self.focus;
        let root = self.root.take().expect("root present");
        let (new_root, found) = remove_leaf(root, target);
        self.root = Some(new_root.expect("a non-leaf root cannot fully collapse"));
        debug_assert!(found);
        if self.zoom == Some(target) {
            self.zoom = None;
        }
        self.focus = first_leaf(self.root_ref());
        Some(target)
    }

    pub fn toggle_zoom(&mut self) {
        self.zoom = match self.zoom {
            Some(_) => None,
            None => Some(self.focus),
        };
    }

    /// Each pane's rectangle within `area` (focused pane fills `area` when zoomed).
    pub fn layout(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        if let Some(z) = self.zoom {
            out.push((z, area));
        } else {
            layout_node(self.root_ref(), area, &mut out);
        }
        out
    }

    /// Move focus to the nearest pane in `dir` (using `area` for geometry).
    pub fn focus_dir(&mut self, dir: Dir, area: Rect) -> bool {
        let rects = self.layout(area);
        let Some((_, cur)) = rects.iter().find(|(id, _)| *id == self.focus).copied() else {
            return false;
        };
        let (cx, cy) = (cur.x + cur.w * 0.5, cur.y + cur.h * 0.5);
        let mut best: Option<(PaneId, f32)> = None;
        for (id, r) in &rects {
            if *id == self.focus {
                continue;
            }
            let (rx, ry) = (r.x + r.w * 0.5, r.y + r.h * 0.5);
            let ok = match dir {
                Dir::Left => rx < cx,
                Dir::Right => rx > cx,
                Dir::Up => ry < cy,
                Dir::Down => ry > cy,
            };
            if !ok {
                continue;
            }
            let dist = (rx - cx).powi(2) + (ry - cy).powi(2);
            if best.map(|(_, d)| dist < d).unwrap_or(true) {
                best = Some((*id, dist));
            }
        }
        match best {
            Some((id, _)) => {
                self.focus = id;
                true
            }
            None => false,
        }
    }

    /// Cycle focus to the next pane (in id order) — the `focus_next` action.
    pub fn focus_next(&mut self) {
        let ids = self.pane_ids();
        if let Some(pos) = ids.iter().position(|id| *id == self.focus) {
            self.focus = ids[(pos + 1) % ids.len()];
        }
    }

    /// Serialize the split structure to TOML, mapping each leaf's content through `f`. The
    /// split topology (axis + ratio), the focused leaf, and the zoom are recorded by
    /// depth-first leaf index — stable across a restore (which re-allocates ids). Pure data,
    /// so the persistence layer stays free of any per-content knowledge.
    pub fn snapshot(&self, f: &impl Fn(&T) -> Toml) -> Toml {
        let mut order: Vec<PaneId> = Vec::new();
        let root = node_snapshot(self.root_ref(), f, &mut order);
        let idx = |id: PaneId| order.iter().position(|p| *p == id).unwrap_or(0) as i64;
        let mut kvs = vec![("focus".to_string(), Toml::Int(idx(self.focus))), ("root".to_string(), root)];
        if let Some(z) = self.zoom {
            kvs.push(("zoom".to_string(), Toml::Int(idx(z))));
        }
        Toml::Table(kvs)
    }

    /// Rebuild a tree from [`snapshot`](Self::snapshot) output, mapping each saved leaf back
    /// to content through `g`. `None` if the document is malformed or any leaf fails to
    /// rebuild (so the caller can fall back). Re-allocates ids in the same depth-first order,
    /// then restores focus + zoom by their recorded leaf index.
    pub fn restore(toml: &Toml, g: &mut impl FnMut(&Toml) -> Option<T>) -> Option<PaneTree<T>> {
        let root_t = toml.get("root")?;
        let mut order: Vec<PaneId> = Vec::new();
        let mut next_id: u64 = 0;
        let root = node_restore(root_t, g, &mut next_id, &mut order)?;
        if order.is_empty() {
            return None;
        }
        let pick = |key: &str| {
            toml.get(key).and_then(|v| v.as_int()).and_then(|i| order.get(i.max(0) as usize).copied())
        };
        let focus = pick("focus").unwrap_or(order[0]);
        let zoom = pick("zoom");
        Some(PaneTree { root: Some(root), focus, next_id, zoom })
    }
}

fn node_snapshot<T>(node: &Node<T>, f: &impl Fn(&T) -> Toml, order: &mut Vec<PaneId>) -> Toml {
    match node {
        Node::Leaf { id, content } => {
            order.push(*id);
            Toml::Table(vec![("leaf".to_string(), f(content))])
        }
        Node::Split { axis, ratio, first, second } => Toml::Table(vec![
            ("axis".to_string(), Toml::Str(if *axis == Axis::Horizontal { "h" } else { "v" }.to_string())),
            ("ratio".to_string(), Toml::Float(*ratio as f64)),
            ("first".to_string(), node_snapshot(first, f, order)),
            ("second".to_string(), node_snapshot(second, f, order)),
        ]),
    }
}

fn node_restore<T>(
    toml: &Toml,
    g: &mut impl FnMut(&Toml) -> Option<T>,
    next_id: &mut u64,
    order: &mut Vec<PaneId>,
) -> Option<Node<T>> {
    if let Some(leaf) = toml.get("leaf") {
        let content = g(leaf)?;
        let id = PaneId(*next_id);
        *next_id += 1;
        order.push(id);
        return Some(Node::Leaf { id, content });
    }
    let axis = if toml.get("axis").and_then(|v| v.as_str()) == Some("v") { Axis::Vertical } else { Axis::Horizontal };
    // Clamp the split ratio: a corrupt/hand-edited workspace.toml (`ratio = 5.0`, `-1`, NaN)
    // would otherwise collapse one pane to zero width. `as f32` of NaN stays NaN → clamp with
    // an explicit finite fallback.
    let ratio = toml
        .get("ratio")
        .and_then(|v| v.as_num())
        .map(|r| r as f32)
        .filter(|r| r.is_finite())
        .unwrap_or(0.5)
        .clamp(0.05, 0.95);
    let first = Box::new(node_restore(toml.get("first")?, g, next_id, order)?);
    let second = Box::new(node_restore(toml.get("second")?, g, next_id, order)?);
    Some(Node::Split { axis, ratio, first, second })
}

fn split_in<T>(
    node: Node<T>,
    target: PaneId,
    new_axis: Axis,
    new_id: PaneId,
    content: T,
) -> (Node<T>, Option<T>) {
    match node {
        Node::Leaf { id, content: c } if id == target => {
            let n = Node::Split {
                axis: new_axis,
                ratio: 0.5,
                first: Box::new(Node::Leaf { id, content: c }),
                second: Box::new(Node::Leaf { id: new_id, content }),
            };
            (n, None)
        }
        leaf @ Node::Leaf { .. } => (leaf, Some(content)),
        Node::Split { axis, ratio, first, second } => {
            let (f, left) = split_in(*first, target, new_axis, new_id, content);
            match left {
                None => (Node::Split { axis, ratio, first: Box::new(f), second }, None),
                Some(content) => {
                    let (s, left2) = split_in(*second, target, new_axis, new_id, content);
                    (Node::Split { axis, ratio, first: Box::new(f), second: Box::new(s) }, left2)
                }
            }
        }
    }
}

fn remove_leaf<T>(node: Node<T>, target: PaneId) -> (Option<Node<T>>, bool) {
    match node {
        Node::Leaf { id, .. } if id == target => (None, true),
        leaf @ Node::Leaf { .. } => (Some(leaf), false),
        Node::Split { axis, ratio, first, second } => {
            let (f_opt, f_found) = remove_leaf(*first, target);
            if f_found {
                let new = match f_opt {
                    None => *second,
                    Some(f) => Node::Split { axis, ratio, first: Box::new(f), second },
                };
                return (Some(new), true);
            }
            let first_back = f_opt.expect("not-found returns original");
            let (s_opt, s_found) = remove_leaf(*second, target);
            if s_found {
                let new = match s_opt {
                    None => first_back,
                    Some(s) => {
                        Node::Split { axis, ratio, first: Box::new(first_back), second: Box::new(s) }
                    }
                };
                return (Some(new), true);
            }
            let second_back = s_opt.expect("not-found returns original");
            (
                Some(Node::Split {
                    axis,
                    ratio,
                    first: Box::new(first_back),
                    second: Box::new(second_back),
                }),
                false,
            )
        }
    }
}

fn collect_ids<T>(node: &Node<T>, out: &mut Vec<PaneId>) {
    match node {
        Node::Leaf { id, .. } => out.push(*id),
        Node::Split { first, second, .. } => {
            collect_ids(first, out);
            collect_ids(second, out);
        }
    }
}

fn first_leaf<T>(node: &Node<T>) -> PaneId {
    match node {
        Node::Leaf { id, .. } => *id,
        Node::Split { first, .. } => first_leaf(first),
    }
}

fn find<T>(node: &Node<T>, target: PaneId) -> Option<&T> {
    match node {
        Node::Leaf { id, content } if *id == target => Some(content),
        Node::Leaf { .. } => None,
        Node::Split { first, second, .. } => find(first, target).or_else(|| find(second, target)),
    }
}

fn find_mut<T>(node: &mut Node<T>, target: PaneId) -> Option<&mut T> {
    match node {
        Node::Leaf { id, content } if *id == target => Some(content),
        Node::Leaf { .. } => None,
        Node::Split { first, second, .. } => {
            if let Some(c) = find_mut(first, target) {
                Some(c)
            } else {
                find_mut(second, target)
            }
        }
    }
}

fn layout_node<T>(node: &Node<T>, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
    match node {
        Node::Leaf { id, .. } => out.push((*id, area)),
        Node::Split { axis, ratio, first, second } => {
            let (a, b) = split_rect(area, *axis, *ratio);
            layout_node(first, a, out);
            layout_node(second, b, out);
        }
    }
}

fn split_rect(area: Rect, axis: Axis, ratio: f32) -> (Rect, Rect) {
    match axis {
        Axis::Horizontal => {
            let fw = ((area.w - GUTTER) * ratio).max(0.0);
            let a = Rect::new(area.x, area.y, fw, area.h);
            let b = Rect::new(area.x + fw + GUTTER, area.y, (area.w - fw - GUTTER).max(0.0), area.h);
            (a, b)
        }
        Axis::Vertical => {
            let fh = ((area.h - GUTTER) * ratio).max(0.0);
            let a = Rect::new(area.x, area.y, area.w, fh);
            let b = Rect::new(area.x, area.y + fh + GUTTER, area.w, (area.h - fh - GUTTER).max(0.0));
            (a, b)
        }
    }
}

/// A list of [`PaneTree`]s presented as tabs.
pub struct Tabs<T> {
    tabs: Vec<PaneTree<T>>,
    active: usize,
}

impl<T> Tabs<T> {
    pub fn new(content: T) -> Self {
        Tabs { tabs: vec![PaneTree::new(content)], active: 0 }
    }
    pub fn len(&self) -> usize {
        self.tabs.len()
    }
    pub fn active_index(&self) -> usize {
        self.active
    }
    pub fn active(&self) -> &PaneTree<T> {
        &self.tabs[self.active]
    }
    pub fn active_mut(&mut self) -> &mut PaneTree<T> {
        &mut self.tabs[self.active]
    }
    pub fn iter(&self) -> impl Iterator<Item = &PaneTree<T>> {
        self.tabs.iter()
    }

    pub fn new_tab(&mut self, content: T) {
        self.tabs.push(PaneTree::new(content));
        self.active = self.tabs.len() - 1;
    }
    /// Close the active tab; returns it (so the app drops its sessions). Refuses
    /// to close the last tab.
    pub fn close_tab(&mut self) -> Option<PaneTree<T>> {
        if self.tabs.len() <= 1 {
            return None;
        }
        let removed = self.tabs.remove(self.active);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        Some(removed)
    }
    pub fn next_tab(&mut self) {
        self.active = (self.active + 1) % self.tabs.len();
    }
    pub fn prev_tab(&mut self) {
        self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
    }
    pub fn goto(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.active = i;
        }
    }

    /// Move the tab at `from` to the final index `to`, keeping it focused (the active tab
    /// follows it). `to` is the destination position in the resulting order. No-op when
    /// either index is out of range or they're equal — so a drag that lands in place is free.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
        self.active = to;
    }

    /// Serialize all tabs (each a [`PaneTree::snapshot`]) + the active index to TOML.
    pub fn snapshot(&self, f: &impl Fn(&T) -> Toml) -> Toml {
        Toml::Table(vec![
            ("active".to_string(), Toml::Int(self.active as i64)),
            ("tab".to_string(), Toml::Array(self.tabs.iter().map(|t| t.snapshot(f)).collect())),
        ])
    }

    /// Rebuild a full tab set from [`snapshot`](Self::snapshot) output. A tab whose tree
    /// fails to restore is dropped; `None` only if no tab survives (so the caller can fall
    /// back to a fresh workspace).
    pub fn restore(toml: &Toml, g: &mut impl FnMut(&Toml) -> Option<T>) -> Option<Tabs<T>> {
        let tabs: Vec<PaneTree<T>> =
            toml.get("tab")?.as_array()?.iter().filter_map(|t| PaneTree::restore(t, g)).collect();
        if tabs.is_empty() {
            return None;
        }
        let active = toml.get("active").and_then(|v| v.as_int()).unwrap_or(0).max(0) as usize;
        let active = active.min(tabs.len() - 1);
        Some(Tabs { tabs, active })
    }
}

#[cfg(test)]
mod tests;
