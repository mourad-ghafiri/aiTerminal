//! [`Scene`] → Unicode box art, for every terminal that can't draw pixels (a pipe, tmux,
//! CI, another emulator) and for tests, which can then assert the *picture* instead of a
//! geometry number.
//!
//! Lines live in a direction mask rather than as characters, so a message crossing a
//! lifeline, an edge meeting a node border and two edges meeting at a bend all resolve to
//! the right junction glyph (`├ ┬ ┼ …`) without any special-casing at the call sites.
//! Characters written on top (arrowheads, labels, slanted corners) win over the mask.

use super::scene::{Anchor, Cap, Item, Scene, Shape, Stroke};
use crate::unicode::str_width;

const UP: u8 = 1;
const RIGHT: u8 = 2;
const DOWN: u8 = 4;
const LEFT: u8 = 8;

/// Never draw taller than this, whatever the source claims.
const MAX_ROWS: usize = 400;

/// Draw `scene` as character rows, at most `max_cols` wide. `None` when it is empty or
/// too wide to draw honestly in cells — the caller falls back to showing the source.
pub fn render(scene: &Scene, max_cols: usize) -> Option<Vec<String>> {
    let w = scene.width as usize;
    let h = scene.height as usize;
    if w == 0 || h == 0 || w > max_cols.max(1) || h > MAX_ROWS {
        return None;
    }
    let mut c = Canvas::new(w, h);
    // Frames, then lines, then boxes, then text — the same back-to-front order the GPU
    // renderer uses, so both rasterizers agree on what covers what.
    for it in &scene.items {
        if let Item::Group { rect, title, .. } = it {
            c.group(cells(rect), title);
        }
    }
    let mut edge_labels = Vec::new();
    for it in &scene.items {
        match it {
            Item::Path { points, stroke, tail, head, label, .. } => {
                if let Some(spot) = c.path(points, *stroke, *tail, *head, label) {
                    edge_labels.push((label.clone(), spot));
                }
            }
            Item::Rule { a, b, .. } => c.hline(a.0.round() as isize, b.0.round() as isize, a.1.round() as isize),
            _ => {}
        }
    }
    for it in &scene.items {
        if let Item::Shape { kind, rect, label, .. } = it {
            c.node(*kind, cells(rect), label);
        }
    }
    // Edge labels last: they hunt for a free cell, and only the finished picture knows
    // which cells those are.
    for (label, (x, y, horizontal)) in edge_labels {
        c.place_label(&label, x, y, horizontal);
    }
    for it in &scene.items {
        match it {
            Item::Label { text, x, y, anchor, .. } => c.anchored(text, x.round() as isize, y.round() as isize, *anchor),
            // A wedge has no honest character-cell form; chart types substitute their own
            // text shape before reaching here.
            Item::Wedge { .. } => {}
            _ => {}
        }
    }
    Some(c.rows())
}

/// A coordinate as a cell index. Layout centers land on half-cells (a three-row box has
/// its center at `1.5`), and rounding those *up* would put an arrowhead one row below the
/// line it belongs to — so exact halves settle downward.
fn cell(v: f32) -> isize {
    (v - 0.25).round() as isize
}

/// A rect in whole cells: inclusive `(x0, y0, x1, y1)`.
fn cells(r: &crate::types::Rect) -> (isize, isize, isize, isize) {
    let x0 = r.x.round() as isize;
    let y0 = r.y.round() as isize;
    let x1 = (r.right().round() as isize - 1).max(x0 + 1);
    let y1 = (r.bottom().round() as isize - 1).max(y0 + 1);
    (x0, y0, x1, y1)
}

/// Corner/side overrides for a node outline. `'\0'` means "let the line mask decide".
struct Glyphs {
    tl: char,
    tr: char,
    bl: char,
    br: char,
    left: char,
    right: char,
    top: char,
    bottom: char,
    mid_left: char,
    mid_right: char,
    /// Inner verticals one cell in from each side (subroutine, double circle).
    inner: bool,
}

impl Glyphs {
    const fn plain() -> Self {
        Glyphs { tl: '\0', tr: '\0', bl: '\0', br: '\0', left: '\0', right: '\0', top: '\0', bottom: '\0', mid_left: '\0', mid_right: '\0', inner: false }
    }
    const fn round() -> Self {
        Glyphs { tl: '╭', tr: '╮', bl: '╰', br: '╯', ..Glyphs::plain() }
    }
}

fn glyphs(shape: Shape) -> Glyphs {
    match shape {
        Shape::Rect | Shape::Actor => Glyphs::plain(),
        Shape::Round | Shape::Stadium => Glyphs::round(),
        Shape::Circle => Glyphs { left: '(', right: ')', ..Glyphs::round() },
        Shape::DoubleCircle => Glyphs { left: '(', right: ')', inner: true, ..Glyphs::round() },
        Shape::Subroutine => Glyphs { inner: true, ..Glyphs::plain() },
        Shape::Cylinder => Glyphs { top: '═', bottom: '═', ..Glyphs::round() },
        Shape::Diamond => Glyphs { tl: '╱', tr: '╲', bl: '╲', br: '╱', ..Glyphs::plain() },
        Shape::Hexagon => Glyphs { tl: '╱', tr: '╲', bl: '╲', br: '╱', mid_left: '<', mid_right: '>', ..Glyphs::plain() },
        Shape::Asymmetric => Glyphs { mid_left: '>', ..Glyphs::plain() },
        Shape::Parallelogram => Glyphs { tl: '╱', tr: '╱', bl: '╱', br: '╱', ..Glyphs::plain() },
        Shape::ParallelogramAlt => Glyphs { tl: '╲', tr: '╲', bl: '╲', br: '╲', ..Glyphs::plain() },
        Shape::Trapezoid => Glyphs { tl: '╱', tr: '╲', ..Glyphs::plain() },
        Shape::TrapezoidAlt => Glyphs { bl: '╲', br: '╱', ..Glyphs::plain() },
        Shape::Note => Glyphs { top: '┄', bottom: '┄', left: '┆', right: '┆', ..Glyphs::plain() },
    }
}

struct Canvas {
    w: usize,
    h: usize,
    /// Line directions per cell.
    mask: Vec<u8>,
    /// Characters that override the mask (`'\0'` = none).
    over: Vec<char>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Canvas { w, h, mask: vec![0; w * h], over: vec!['\0'; w * h] }
    }

    fn idx(&self, x: isize, y: isize) -> Option<usize> {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return None;
        }
        Some(y as usize * self.w + x as usize)
    }

    fn add(&mut self, x: isize, y: isize, bits: u8) {
        if let Some(i) = self.idx(x, y) {
            self.mask[i] |= bits;
            // A solid line crossing a dashed one wins the cell, so it reads as continuous
            // rather than pockmarked.
            if matches!(self.over[i], '╌' | '╎') {
                self.over[i] = '\0';
            }
        }
    }

    /// Write a character over whatever is there.
    fn put(&mut self, x: isize, y: isize, ch: char) {
        if ch == '\0' {
            return;
        }
        if let Some(i) = self.idx(x, y) {
            self.over[i] = ch;
        }
    }

    /// Write a character only where nothing has been drawn yet.
    fn put_free(&mut self, x: isize, y: isize, ch: char) -> bool {
        match self.idx(x, y) {
            Some(i) if self.over[i] == '\0' && self.mask[i] == 0 => {
                self.over[i] = ch;
                true
            }
            _ => false,
        }
    }

    fn hline(&mut self, x0: isize, x1: isize, y: isize) {
        let (a, b) = (x0.min(x1), x0.max(x1));
        for x in a..=b {
            let mut bits = 0;
            if x > a {
                bits |= LEFT;
            }
            if x < b {
                bits |= RIGHT;
            }
            self.add(x, y, if a == b { LEFT | RIGHT } else { bits });
        }
    }

    fn vline(&mut self, y0: isize, y1: isize, x: isize) {
        let (a, b) = (y0.min(y1), y0.max(y1));
        for y in a..=b {
            let mut bits = 0;
            if y > a {
                bits |= UP;
            }
            if y < b {
                bits |= DOWN;
            }
            self.add(x, y, if a == b { UP | DOWN } else { bits });
        }
    }

    /// A dashed run, drawn as characters so it never merges into a solid junction.
    fn dashed_v(&mut self, y0: isize, y1: isize, x: isize) {
        for y in y0.min(y1)..=y0.max(y1) {
            self.put_free(x, y, '╎');
        }
    }

    fn dashed_h(&mut self, x0: isize, x1: isize, y: isize) {
        for x in x0.min(x1)..=x0.max(x1) {
            self.put_free(x, y, '╌');
        }
    }

    fn text(&mut self, x: isize, y: isize, s: &str) {
        for (i, ch) in s.chars().enumerate() {
            self.put(x + i as isize, y, ch);
        }
    }

    fn anchored(&mut self, s: &str, x: isize, y: isize, anchor: Anchor) {
        for (row, line) in s.split('\n').enumerate() {
            let w = str_width(line) as isize;
            let sx = match anchor {
                Anchor::Start => x,
                Anchor::Middle => x - w / 2,
                Anchor::End => x - w,
            };
            self.text(sx, y + row as isize, line);
        }
    }

    /// A node outline plus its centered label.
    fn node(&mut self, shape: Shape, (x0, y0, x1, y1): (isize, isize, isize, isize), label: &str) {
        if shape == Shape::Actor {
            return self.actor((x0, y0, x1, y1), label);
        }
        let g = glyphs(shape);
        self.hline(x0, x1, y0);
        self.hline(x0, x1, y1);
        self.vline(y0, y1, x0);
        self.vline(y0, y1, x1);
        for x in x0 + 1..x1 {
            self.put(x, y0, g.top);
            self.put(x, y1, g.bottom);
        }
        for y in y0 + 1..y1 {
            self.put(x0, y, g.left);
            self.put(x1, y, g.right);
        }
        self.put(x0, y0, g.tl);
        self.put(x1, y0, g.tr);
        self.put(x0, y1, g.bl);
        self.put(x1, y1, g.br);
        let mid = (y0 + y1) / 2;
        self.put(x0, mid, g.mid_left);
        self.put(x1, mid, g.mid_right);
        if g.inner && x1 - x0 > 3 {
            self.vline(y0 + 1, y1 - 1, x0 + 1);
            self.vline(y0 + 1, y1 - 1, x1 - 1);
        }
        self.centered_label(label, x0 + 1, x1 - 1, y0 + 1, y1 - 1);
    }

    /// A sequence actor: a stick figure with its name underneath.
    fn actor(&mut self, (x0, y0, x1, y1): (isize, isize, isize, isize), name: &str) {
        let cx = (x0 + x1) / 2;
        self.put(cx, y0, '○');
        if y1 - y0 >= 2 {
            self.text(cx - 1, y0 + 1, "╱│╲");
        }
        self.centered_label(name, x0, x1, y1, y1);
    }

    fn centered_label(&mut self, label: &str, x0: isize, x1: isize, y0: isize, y1: isize) {
        if label.is_empty() {
            return;
        }
        let lines: Vec<&str> = label.split('\n').collect();
        let inner_h = (y1 - y0 + 1).max(1);
        let top = y0 + (inner_h - lines.len() as isize).max(0) / 2;
        for (i, line) in lines.iter().enumerate() {
            let avail = (x1 - x0 + 1).max(0) as usize;
            let clipped = clip(line, avail);
            let w = str_width(&clipped) as isize;
            let x = x0 + ((x1 - x0 + 1) - w).max(0) / 2;
            self.text(x, top + i as isize, &clipped);
        }
    }

    /// A titled frame (subgraph, sequence block).
    fn group(&mut self, (x0, y0, x1, y1): (isize, isize, isize, isize), title: &str) {
        for x in x0..=x1 {
            self.put_free(x, y0, '┄');
            self.put_free(x, y1, '┄');
        }
        for y in y0..=y1 {
            self.put_free(x0, y, '┆');
            self.put_free(x1, y, '┆');
        }
        self.put(x0, y0, '┌');
        self.put(x1, y0, '┐');
        self.put(x0, y1, '└');
        self.put(x1, y1, '┘');
        if !title.is_empty() {
            let t = clip(title, (x1 - x0).max(0) as usize);
            self.text(x0 + 2, y0, &format!(" {t} "));
        }
    }

    /// A polyline, routed orthogonally (a diagonal has no honest cell form), with its end
    /// caps. Returns where its label wants to go, for the later free-cell pass.
    fn path(&mut self, points: &[(f32, f32)], stroke: Stroke, tail: Cap, head: Cap, label: &str) -> Option<(isize, isize, bool)> {
        if points.len() < 2 {
            return None;
        }
        let pts: Vec<(isize, isize)> = points.iter().map(|&(x, y)| (cell(x), cell(y))).collect();
        let dashed = matches!(stroke, Stroke::Dashed | Stroke::Dotted);
        let mut label_at: Option<(isize, isize, bool)> = None; // (x, y-of-the-line, horizontal)
        // The first and last runs actually drawn — the caps follow the ink, so a bend that
        // collapses to nothing in cells can't leave an arrow pointing the wrong way.
        let mut first_run: Option<((isize, isize), (isize, isize))> = None;
        let mut last_run: Option<((isize, isize), (isize, isize))> = None;
        for w in pts.windows(2) {
            for (p, q) in manhattan(w[0], w[1]) {
                if p != q {
                    first_run.get_or_insert((p, q));
                    last_run = Some((p, q));
                }
                if p.0 == q.0 {
                    if dashed {
                        self.dashed_v(p.1, q.1, p.0);
                    } else {
                        self.vline(p.1, q.1, p.0);
                    }
                    label_at.get_or_insert((p.0, (p.1 + q.1) / 2, false));
                } else {
                    if dashed {
                        self.dashed_h(p.0, q.0, p.1);
                    } else {
                        self.hline(p.0, q.0, p.1);
                    }
                    // A horizontal run reads better as the label's home than a vertical one.
                    label_at = Some(((p.0 + q.0) / 2, p.1, true));
                }
            }
        }
        if let Some((p, q)) = last_run {
            self.cap(head, step_back(q, p), p);
        }
        if let Some((p, q)) = first_run {
            self.cap(tail, step_back(p, q), q);
        }
        (!label.is_empty()).then_some(label_at).flatten()
    }

    /// Put an edge label as close to `(x, y)` as a free run of cells allows, so it never
    /// lands on a node it doesn't belong to. Falls back to the line itself, padded.
    fn place_label(&mut self, label: &str, x: isize, y: isize, horizontal: bool) {
        let text = clip(&label.replace('\n', " "), self.w.saturating_sub(2));
        let w = str_width(&text) as isize;
        // Above the line first (mermaid's own placement), then below, then further out —
        // and at each row, slide along the line looking for a run that is actually free.
        let rows: [isize; 4] = if horizontal { [y - 1, y + 1, y - 2, y + 2] } else { [y, y - 1, y + 1, y - 2] };
        let slides: [isize; 5] = [0, -2, 2, -4, 4];
        let mut candidates = Vec::with_capacity(rows.len() * slides.len());
        for row in rows {
            for slide in slides {
                candidates.push(if horizontal { (x - w / 2 + slide, row) } else { (x + 1 + slide.abs(), row) });
            }
        }
        for (sx, sy) in candidates {
            if (0..w).all(|i| matches!(self.idx(sx + i, sy), Some(k) if self.over[k] == '\0' && self.mask[k] == 0)) {
                return self.text(sx, sy, &text);
            }
        }
        // Nowhere is free: sit on the line, cleared to a space on each side so it reads.
        let (sx, sy) = if horizontal { (x - w / 2 - 1, y) } else { (x + 1, y) };
        self.text(sx, sy, &format!(" {text} "));
    }

    fn cap(&mut self, cap: Cap, at: (isize, isize), from: (isize, isize)) {
        let ch = match cap {
            Cap::None => return,
            Cap::Cross => '✕',
            Cap::Circle => '●',
            Cap::Diamond => '◇',
            Cap::FilledDiamond => '◆',
            Cap::Tick => '┃',
            Cap::CrowFoot => '≺',
            Cap::Arrow | Cap::Open | Cap::Triangle => {
                let (dx, dy) = (at.0 - from.0, at.1 - from.1);
                if dx.abs() >= dy.abs() {
                    if dx >= 0 {
                        '▶'
                    } else {
                        '◀'
                    }
                } else if dy >= 0 {
                    '▼'
                } else {
                    '▲'
                }
            }
        };
        self.put(at.0, at.1, ch);
    }

    fn rows(self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.h);
        for y in 0..self.h {
            let mut line = String::with_capacity(self.w);
            for x in 0..self.w {
                let i = y * self.w + x;
                line.push(if self.over[i] != '\0' { self.over[i] } else { glyph(self.mask[i]) });
            }
            out.push(line.trim_end().to_string());
        }
        out
    }
}

/// A direction mask as a box-drawing character.
fn glyph(mask: u8) -> char {
    if mask == 0 {
        return ' ';
    }
    // Purely vertical / purely horizontal runs, including their end cells.
    if mask & (LEFT | RIGHT) == 0 {
        return '│';
    }
    if mask & (UP | DOWN) == 0 {
        return '─';
    }
    match (mask & UP != 0, mask & RIGHT != 0, mask & DOWN != 0, mask & LEFT != 0) {
        (true, true, false, false) => '└',
        (true, false, false, true) => '┘',
        (false, true, true, false) => '┌',
        (false, false, true, true) => '┐',
        (true, true, true, false) => '├',
        (true, false, true, true) => '┤',
        (false, true, true, true) => '┬',
        (true, true, false, true) => '┴',
        _ => '┼',
    }
}

/// One cell back from `at` toward `from` — where an arrowhead sits, so the node's own
/// border survives underneath it.
fn step_back(at: (isize, isize), from: (isize, isize)) -> (isize, isize) {
    let (dx, dy) = (at.0 - from.0, at.1 - from.1);
    if dx.abs() >= dy.abs() {
        (at.0 - dx.signum(), at.1)
    } else {
        (at.0, at.1 - dy.signum())
    }
}

/// Split a segment into axis-aligned runs: straight when it already is, otherwise a Z
/// bend that turns on the dominant axis first.
fn manhattan(a: (isize, isize), b: (isize, isize)) -> Vec<((isize, isize), (isize, isize))> {
    if a.0 == b.0 || a.1 == b.1 {
        return vec![(a, b)];
    }
    if (b.1 - a.1).abs() >= (b.0 - a.0).abs() {
        let my = (a.1 + b.1) / 2;
        vec![(a, (a.0, my)), ((a.0, my), (b.0, my)), ((b.0, my), b)]
    } else {
        let mx = (a.0 + b.0) / 2;
        vec![(a, (mx, a.1)), ((mx, a.1), (mx, b.1)), ((mx, b.1), b)]
    }
}

/// Truncate to `max` columns, marking the cut with `…`.
fn clip(s: &str, max: usize) -> String {
    if str_width(s) <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".repeat(max);
    }
    let mut out = String::new();
    for ch in s.chars() {
        if str_width(&out) + 1 >= max {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::super::{layout, parse};
    use super::*;

    fn art(src: &str, cols: usize) -> Vec<String> {
        let d = parse(src).expect("parses");
        let scene = layout(&d, &|s: &str| (str_width(s) as u32, 1));
        render(&scene, cols).expect("draws")
    }

    fn joined(src: &str, cols: usize) -> String {
        art(src, cols).join("\n")
    }

    #[test]
    fn a_flowchart_draws_boxes_labels_and_an_arrow() {
        let out = joined("flowchart TD\n A[Start] --> B[End]", 80);
        assert!(out.contains("Start"), "{out}");
        assert!(out.contains("End"), "{out}");
        assert!(out.contains('┌') && out.contains('┘'), "square node corners:\n{out}");
        assert!(out.contains('▼'), "a downward arrowhead:\n{out}");
    }

    #[test]
    fn shapes_read_differently() {
        assert!(joined("flowchart TD\n A(round)", 40).contains('╭'), "rounded corners");
        assert!(joined("flowchart TD\n A{choice}", 40).contains('╱'), "diamond corners");
        assert!(joined("flowchart TD\n A((circle))", 40).contains('('), "circle sides");
    }

    #[test]
    fn an_edge_label_is_drawn_beside_the_line() {
        let out = joined("flowchart LR\n A -->|yes| B", 80);
        assert!(out.contains("yes"), "{out}");
    }

    #[test]
    fn a_sequence_draws_lifelines_and_messages() {
        let out = joined("sequenceDiagram\n A->>B: Hi\n B-->>A: Yo", 80);
        assert!(out.contains('╎'), "dashed lifelines:\n{out}");
        assert!(out.contains("Hi") && out.contains("Yo"), "{out}");
        assert!(out.contains('▶') || out.contains('◀'), "message arrowheads:\n{out}");
    }

    #[test]
    fn too_wide_is_refused_rather_than_mangled() {
        let d = parse("flowchart LR\n A --> B --> C --> D").unwrap();
        let scene = layout(&d, &|s: &str| (str_width(s) as u32, 1));
        assert!(render(&scene, 8).is_none(), "a diagram wider than the pane is refused");
        assert!(render(&scene, 200).is_some());
    }

    #[test]
    fn rows_have_no_trailing_padding() {
        for line in art("flowchart TD\n A --> B", 60) {
            assert_eq!(line.trim_end(), line, "row is padded: {line:?}");
        }
    }

    #[test]
    fn long_labels_are_clipped_not_overflowed() {
        let scene = layout(&parse("flowchart TD\n A[hello]").unwrap(), &|s: &str| (str_width(s) as u32, 1));
        let rows = render(&scene, 200).unwrap();
        let widest = rows.iter().map(|r| str_width(r)).max().unwrap_or(0);
        assert!(widest <= scene.width as usize, "{widest} > {}", scene.width);
    }

    #[test]
    fn glyph_table_covers_every_junction() {
        assert_eq!(glyph(UP | DOWN | LEFT | RIGHT), '┼');
        assert_eq!(glyph(UP | RIGHT), '└');
        assert_eq!(glyph(DOWN | LEFT), '┐');
        assert_eq!(glyph(0), ' ');
    }
}
