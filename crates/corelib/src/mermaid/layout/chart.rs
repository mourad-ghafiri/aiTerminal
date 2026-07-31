//! Chart layout — pie, xychart, quadrant, gantt, sankey, radar, treemap and packet.
//!
//! Two of these shapes have no honest form in character cells: a pie's wedges and a
//! radar's polygon are sub-cell geometry, and a Manhattan-ized polygon is a lie. So when
//! the host measures in cells ([`Metrics::cells`]) those two lay out as labelled bars
//! instead — the same numbers, drawn with the ink actually available.

use super::super::scene::{Anchor, Builder, Cap, Role, Scene, Shape, Stroke, TextSize};
use super::super::{Chart, ChartKind};
use super::{Measure, Metrics};
use crate::types::Rect;

pub(crate) fn layout(c: &Chart, m: &Metrics, measure: Measure) -> Scene {
    match c.kind {
        ChartKind::Pie | ChartKind::Treemap => pie(c, m, measure),
        ChartKind::Xy => xy(c, m, measure),
        ChartKind::Radar => radar(c, m, measure),
        ChartKind::Quadrant => quadrant(c, m, measure),
        ChartKind::Gantt => gantt(c, m, measure),
        ChartKind::Sankey => sankey(c, m, measure),
        ChartKind::Packet | ChartKind::Info => rows(c, m, measure),
    }
}

/// A title above the plot, and the y it leaves free.
fn title(sb: &mut Builder, c: &Chart, m: &Metrics, width: f32) -> f32 {
    if c.title.is_empty() {
        return m.margin;
    }
    sb.label(c.title.clone(), width / 2.0, m.margin * 0.5, Anchor::Middle, TextSize::Title, Role::Label);
    m.margin + 2.0 * m.eh
}

/// Pie (and treemap, which asks the same question) — wedges with a legend where there are
/// pixels, proportional bars where there are only cells.
fn pie(c: &Chart, m: &Metrics, measure: Measure) -> Scene {
    let values = c.series.first().map(|s| s.values.clone()).unwrap_or_default();
    let total: f64 = values.iter().sum();
    if values.is_empty() || total <= 0.0 {
        return Scene::default();
    }
    let legend: Vec<String> = c
        .categories
        .iter()
        .zip(&values)
        .map(|(name, v)| format!("{name}  {}  ({:.0}%)", trim_number(*v), v / total * 100.0))
        .collect();
    let legend_w = legend.iter().map(|l| m.text_size(l, measure).0).fold(0.0_f32, f32::max);

    if m.cells() {
        // Cells: a bar per slice, scaled to the widest label we can afford.
        let mut sb = Builder::new(m.margin);
        let width = legend_w + 24.0 * m.ew;
        let mut y = title(&mut sb, c, m, width);
        let bar_max = 20.0 * m.ew;
        for (i, (label, v)) in legend.iter().zip(&values).enumerate() {
            let len = ((v / total) as f32 * bar_max).max(m.ew);
            sb.label(label.clone(), m.margin, y, Anchor::Start, TextSize::Normal, Role::Label);
            sb.shape(Shape::Bar, Rect::new(m.margin + legend_w + m.ew, y, len, m.eh), String::new(), Role::Slot(i as u8));
            y += m.eh;
        }
        let mut scene = sb.build();
        scene.width = scene.width.max((m.margin + legend_w + m.ew + bar_max + m.margin) as u32);
        return scene;
    }

    let r = 8.0 * m.eh;
    let mut sb = Builder::new(m.margin);
    let width = 2.0 * r + 4.0 * m.ew + legend_w + 2.0 * m.margin;
    let top = title(&mut sb, c, m, width);
    let (cx, cy) = (m.margin + r, top + r);
    // Slices start at twelve o'clock, the way every pie chart is read.
    let mut angle = -std::f32::consts::FRAC_PI_2;
    for (i, v) in values.iter().enumerate() {
        let sweep = (v / total) as f32 * std::f32::consts::TAU;
        sb.wedge(cx, cy, r, angle, angle + sweep, i as u8);
        angle += sweep;
    }
    let mut y = top;
    for (i, label) in legend.iter().enumerate() {
        let swatch = Rect::new(cx + r + 2.0 * m.ew, y, m.ew, m.eh);
        sb.shape(Shape::Rect, swatch, String::new(), Role::Slot(i as u8));
        sb.label(label.clone(), swatch.right() + m.ew, y, Anchor::Start, TextSize::Small, Role::Label);
        y += 1.5 * m.eh;
    }
    let mut scene = sb.build();
    scene.width = scene.width.max(width as u32);
    scene.height = scene.height.max((cy + r + m.margin) as u32);
    scene
}

/// An xy chart: a category axis along the bottom, one bar (or line marker) per value.
fn xy(c: &Chart, m: &Metrics, measure: Measure) -> Scene {
    let max = c.max_value();
    if c.series.is_empty() || max <= 0.0 {
        return Scene::default();
    }
    let plot_h = 10.0 * m.eh;
    let col_w = c
        .categories
        .iter()
        .map(|t| m.text_size(t, measure).0)
        .fold(4.0 * m.ew, f32::max)
        + 2.0 * m.ew;
    let count = c.series.iter().map(|s| s.values.len()).max().unwrap_or(0);
    let mut sb = Builder::new(m.margin);
    let width = m.margin * 2.0 + col_w * count as f32;
    let top = title(&mut sb, c, m, width);
    let base = top + plot_h;

    for (si, s) in c.series.iter().enumerate() {
        for (i, v) in s.values.iter().enumerate() {
            let h = ((v / max) as f32 * plot_h).max(m.eh * 0.5);
            let x = m.margin + col_w * i as f32;
            if s.line {
                // A line series marks its value rather than filling to the axis.
                sb.label("◆", x + col_w / 2.0, base - h, Anchor::Middle, TextSize::Normal, Role::Slot(si as u8 + 3));
            } else {
                let bw = col_w * 0.6;
                sb.shape(Shape::Rect, Rect::new(x + (col_w - bw) / 2.0, base - h, bw, h), String::new(), Role::Slot(si as u8));
            }
        }
    }
    sb.rule((m.margin, base), (m.margin + col_w * count as f32, base), Role::Muted);
    for (i, cat) in c.categories.iter().enumerate() {
        sb.label(cat.clone(), m.margin + col_w * i as f32 + col_w / 2.0, base + m.eh * 0.5, Anchor::Middle, TextSize::Small, Role::Label);
    }
    if !c.y_title.is_empty() {
        sb.label(format!("{} (max {})", c.y_title, trim_number(max)), m.margin, top - m.eh, Anchor::Start, TextSize::Small, Role::Muted);
    }
    sb.build()
}

/// Radar: one spoke per axis. In pixels the curve is a closed polygon; in cells each
/// series becomes a labelled bar per axis, which is the same information legibly.
fn radar(c: &Chart, m: &Metrics, measure: Measure) -> Scene {
    if c.categories.is_empty() || c.series.is_empty() {
        return Scene::default();
    }
    let max = c.max_value().max(1.0);
    if m.cells() {
        let label_w = c.categories.iter().map(|t| m.text_size(t, measure).0).fold(0.0_f32, f32::max);
        let mut sb = Builder::new(m.margin);
        let mut y = title(&mut sb, c, m, label_w + 24.0 * m.ew);
        for (si, s) in c.series.iter().enumerate() {
            if !s.name.is_empty() {
                sb.label(s.name.clone(), m.margin, y, Anchor::Start, TextSize::Normal, Role::Accent);
                y += m.eh;
            }
            for (i, cat) in c.categories.iter().enumerate() {
                let v = s.values.get(i).copied().unwrap_or(0.0);
                let len = ((v / max) as f32 * 16.0 * m.ew).max(m.ew);
                sb.label(cat.clone(), m.margin, y, Anchor::Start, TextSize::Small, Role::Label);
                sb.shape(Shape::Bar, Rect::new(m.margin + label_w + m.ew, y, len, m.eh), String::new(), Role::Slot(si as u8));
                y += m.eh;
            }
            y += m.eh;
        }
        return sb.build();
    }
    let r = 8.0 * m.eh;
    let mut sb = Builder::new(m.margin);
    let width = 2.0 * r + 2.0 * m.margin;
    let top = title(&mut sb, c, m, width);
    let (cx, cy) = (m.margin + r, top + r);
    let spokes = c.categories.len().max(3);
    let at = |i: usize, frac: f32| -> (f32, f32) {
        let a = std::f32::consts::TAU * i as f32 / spokes as f32 - std::f32::consts::FRAC_PI_2;
        (cx + a.cos() * r * frac, cy + a.sin() * r * frac)
    };
    for (i, cat) in c.categories.iter().enumerate() {
        sb.path(vec![(cx, cy), at(i, 1.0)], Stroke::Dotted, Cap::None, Cap::None, "", Role::Muted);
        let (lx, ly) = at(i, 1.15);
        sb.label(cat.clone(), lx, ly, Anchor::Middle, TextSize::Small, Role::Label);
    }
    for (si, s) in c.series.iter().enumerate() {
        let mut points: Vec<(f32, f32)> = (0..c.categories.len()).map(|i| at(i, (s.values.get(i).copied().unwrap_or(0.0) / max) as f32)).collect();
        if let Some(&first) = points.first() {
            points.push(first); // close the ring
        }
        sb.path(points, Stroke::Solid, Cap::None, Cap::None, s.name.clone(), Role::Slot(si as u8));
    }
    let mut scene = sb.build();
    scene.width = scene.width.max(width as u32);
    scene
}

/// A quadrant chart: two axes, four captioned quarters, and the points plotted in them.
fn quadrant(c: &Chart, m: &Metrics, measure: Measure) -> Scene {
    let side = 14.0 * m.eh;
    let mut sb = Builder::new(m.margin);
    let width = side + 2.0 * m.margin;
    let top = title(&mut sb, c, m, width);
    let plot = Rect::new(m.margin, top, side, side);
    sb.shape(Shape::Rect, plot, String::new(), Role::Muted);
    sb.rule((plot.x, plot.y + side / 2.0), (plot.right(), plot.y + side / 2.0), Role::Muted);
    // Captions sit in their own quarter, clockwise from the top right.
    let spots = [
        (plot.x + side * 0.75, plot.y + side * 0.15, &c.quadrants[0]),
        (plot.x + side * 0.25, plot.y + side * 0.15, &c.quadrants[1]),
        (plot.x + side * 0.25, plot.y + side * 0.85, &c.quadrants[2]),
        (plot.x + side * 0.75, plot.y + side * 0.85, &c.quadrants[3]),
    ];
    for (x, y, text) in spots {
        if !text.is_empty() {
            sb.label(text.clone(), x, y, Anchor::Middle, TextSize::Small, Role::Muted);
        }
    }
    for p in &c.points {
        // The y axis grows upward, so a point's value counts from the bottom.
        let x = plot.x + (p.x as f32).clamp(0.0, 1.0) * side;
        let y = plot.bottom() - (p.y as f32).clamp(0.0, 1.0) * side;
        sb.label("●", x, y, Anchor::Middle, TextSize::Normal, Role::Accent);
        sb.label(p.name.clone(), x + m.ew, y, Anchor::Start, TextSize::Small, Role::Label);
    }
    if !c.x_title.is_empty() {
        sb.label(c.x_title.clone(), plot.x + side / 2.0, plot.bottom() + m.eh * 0.5, Anchor::Middle, TextSize::Small, Role::Muted);
    }
    if !c.y_title.is_empty() {
        sb.label(c.y_title.clone(), plot.x, plot.y - m.eh, Anchor::Start, TextSize::Small, Role::Muted);
    }
    let _ = measure;
    sb.build()
}

/// A gantt: one row per task, bars scaled to the whole plan's span.
fn gantt(c: &Chart, m: &Metrics, measure: Measure) -> Scene {
    if c.tasks.is_empty() {
        return Scene::default();
    }
    let start = c.tasks.iter().map(|t| t.start).min().unwrap_or(0);
    let end = c.tasks.iter().map(|t| t.end).max().unwrap_or(start + 1);
    let span = (end - start).max(1) as f32;
    let label_w = c.tasks.iter().map(|t| m.text_size(&t.name, measure).0).fold(0.0_f32, f32::max) + 2.0 * m.ew;
    let bar_w = 30.0 * m.ew;

    let mut sb = Builder::new(m.margin);
    let width = m.margin * 2.0 + label_w + bar_w;
    let mut y = title(&mut sb, c, m, width);
    let x0 = m.margin + label_w;
    let mut section = String::new();
    for t in &c.tasks {
        if t.section != section {
            section = t.section.clone();
            if !section.is_empty() {
                sb.label(section.clone(), m.margin, y, Anchor::Start, TextSize::Normal, Role::Accent);
                y += m.eh;
            }
        }
        let a = x0 + (t.start - start) as f32 / span * bar_w;
        let b = x0 + (t.end - start) as f32 / span * bar_w;
        sb.label(t.name.clone(), m.margin + m.ew, y, Anchor::Start, TextSize::Small, Role::Label);
        if t.milestone {
            sb.label("◆", a, y, Anchor::Middle, TextSize::Normal, Role::Accent);
        } else {
            let w = (b - a).max(m.ew);
            let role = if t.critical {
                Role::Slot(1)
            } else if t.done {
                Role::Muted
            } else if t.active {
                Role::Accent
            } else {
                Role::Slot(4)
            };
            sb.shape(Shape::Bar, Rect::new(a, y, w, m.eh), String::new(), role);
        }
        y += m.eh;
    }
    // The span, so the bars mean something absolute.
    let stamp = |secs: i64| crate::datetime::format(secs, "%Y-%m-%d", 0);
    sb.label(stamp(start), x0, y + m.eh * 0.5, Anchor::Start, TextSize::Small, Role::Muted);
    sb.label(stamp(end), x0 + bar_w, y + m.eh * 0.5, Anchor::End, TextSize::Small, Role::Muted);
    let mut scene = sb.build();
    scene.width = scene.width.max(width as u32);
    scene
}

/// A sankey: source and target columns, with each flow's weight on the link.
fn sankey(c: &Chart, m: &Metrics, measure: Measure) -> Scene {
    if c.flows.is_empty() {
        return Scene::default();
    }
    // Nodes in first-seen order: sources on the left, everything else on the right.
    let mut left: Vec<&str> = Vec::new();
    let mut right: Vec<&str> = Vec::new();
    for (from, to, _) in &c.flows {
        if !left.iter().any(|n| n == &from.as_str()) {
            left.push(from);
        }
        if !right.iter().any(|n| n == &to.as_str()) {
            right.push(to);
        }
    }
    let w = |names: &[&str]| names.iter().map(|n| m.text_size(n, measure).0).fold(m.min_w, f32::max) + 2.0 * m.pad_x;
    let (lw, rw) = (w(&left), w(&right));
    let gap = 12.0 * m.ew;
    let row = 3.0 * m.eh;

    let mut sb = Builder::new(m.margin);
    let width = m.margin * 2.0 + lw + gap + rw;
    let top = title(&mut sb, c, m, width);
    let rect = |i: usize, x: f32, w: f32| Rect::new(x, top + i as f32 * row, w, 2.0 * m.eh);
    for (from, to, value) in &c.flows {
        let a = left.iter().position(|n| n == &from.as_str()).unwrap_or(0);
        let b = right.iter().position(|n| n == &to.as_str()).unwrap_or(0);
        let ra = rect(a, m.margin, lw);
        let rb = rect(b, m.margin + lw + gap, rw);
        sb.path(
            vec![(ra.right(), ra.y + ra.h / 2.0), (rb.x, rb.y + rb.h / 2.0)],
            Stroke::Thick,
            Cap::None,
            Cap::Arrow,
            trim_number(*value),
            Role::Edge,
        );
    }
    for (i, name) in left.iter().enumerate() {
        sb.shape(Shape::Rect, rect(i, m.margin, lw), (*name).to_string(), Role::Node);
    }
    for (i, name) in right.iter().enumerate() {
        sb.shape(Shape::Rect, rect(i, m.margin + lw + gap, rw), (*name).to_string(), Role::Node);
    }
    let mut scene = sb.build();
    scene.width = scene.width.max(width as u32);
    scene
}

/// Packet and info: labelled rows in a frame.
fn rows(c: &Chart, m: &Metrics, measure: Measure) -> Scene {
    if c.rows.is_empty() {
        return Scene::default();
    }
    let key_w = c.rows.iter().map(|(k, _)| m.text_size(k, measure).0).fold(0.0_f32, f32::max);
    let val_w = c.rows.iter().map(|(_, v)| m.text_size(v, measure).0).fold(0.0_f32, f32::max);
    let w = key_w + val_w + 4.0 * m.pad_x;
    let mut sb = Builder::new(m.margin);
    let mut y = title(&mut sb, c, m, w + 2.0 * m.margin);
    for (key, value) in &c.rows {
        let r = Rect::new(m.margin, y, w, 3.0 * m.eh);
        sb.shape(Shape::Rect, r, String::new(), Role::Node);
        if !key.is_empty() {
            sb.label(key.clone(), r.x + m.pad_x, y + m.eh, Anchor::Start, TextSize::Small, Role::Muted);
        }
        sb.label(value.clone(), r.x + key_w + 2.0 * m.pad_x, y + m.eh, Anchor::Start, TextSize::Normal, Role::Label);
        y += 3.0 * m.eh - m.eh * 0.0;
    }
    sb.build()
}

/// `25` rather than `25.0`, and `2.5` when the fraction matters.
fn trim_number(v: f64) -> String {
    if (v - v.round()).abs() < 0.05 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

#[cfg(test)]
mod tests;
