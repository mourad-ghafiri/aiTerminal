//! Sequence-diagram layout: actor columns, lifelines, and a message timeline.

use super::super::scene::{Builder, Cap, Role, Scene, Shape, Stroke};
use super::super::Sequence;
use super::{Measure, Metrics};
use crate::types::Rect;

pub(crate) fn layout(s: &Sequence, m: &Metrics, measure: Measure) -> Scene {
    let a = s.actors.len();
    if a == 0 {
        return Scene::default();
    }
    // Three lines of actor box (border, name, border) and three per message (its label,
    // the arrow, breathing room) — the minimum that stays legible in character cells.
    let actor_h = 3.0 * m.eh;
    let actor_gap = 5.0 * m.ew;
    let msg_top = 2.0 * m.eh;
    let msg_gap = 3.0 * m.eh;

    let mut sb = Builder::new(m.margin);
    let mut rects = Vec::with_capacity(a);
    let mut cx = Vec::with_capacity(a);
    let mut x = m.margin;
    for name in &s.actors {
        let w = (m.text_size(name, measure).0 + 2.0 * m.pad_x).max(m.min_w);
        let r = Rect::new(x, m.margin, w, actor_h);
        rects.push(r);
        cx.push(r.x + r.w / 2.0);
        x += w + actor_gap;
    }
    let width = x - actor_gap + m.margin;
    let lifeline_top = m.margin + actor_h;
    let bottom = lifeline_top + msg_top + msg_gap * s.messages.len().max(1) as f32;

    // Lifelines first, so messages draw over them.
    for &x in &cx {
        sb.path(vec![(x, lifeline_top), (x, bottom)], Stroke::Dashed, Cap::None, Cap::None, "", Role::Muted);
    }
    let mut y = lifeline_top + msg_top;
    for msg in &s.messages {
        if msg.from < a && msg.to < a {
            let stroke = if msg.dashed { Stroke::Dashed } else { Stroke::Solid };
            sb.path(vec![(cx[msg.from], y), (cx[msg.to], y)], stroke, Cap::None, Cap::Arrow, msg.text.clone(), Role::Edge);
        }
        y += msg_gap;
    }
    for (i, name) in s.actors.iter().enumerate() {
        sb.shape(Shape::Rect, rects[i], name.clone(), Role::Node);
    }
    let mut scene = sb.build();
    scene.width = scene.width.max(width.ceil() as u32);
    scene.height = scene.height.max((bottom + m.margin).ceil() as u32);
    scene
}

