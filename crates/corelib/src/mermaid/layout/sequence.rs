//! Sequence-diagram layout: actor columns, lifelines, activation bars, notes and the
//! framed blocks — all from one top-to-bottom walk of the event timeline.

use super::super::scene::{Anchor, Builder, Cap, Role, Scene, Shape, Stroke, TextSize};
use super::super::{Event, Message, NotePos, Sequence};
use super::{Measure, Metrics};
use crate::types::Rect;

pub(crate) fn layout(s: &Sequence, m: &Metrics, measure: Measure) -> Scene {
    let n = s.actors.len();
    if n == 0 {
        return Scene::default();
    }
    let actor_h = 3.0 * m.eh;
    let gap = 6.0 * m.ew;
    let step = 3.0 * m.eh; // one message: its label, its arrow, and breathing room
    let bar_w = m.ew.max(1.0); // an activation bar's thickness

    // ── columns ──────────────────────────────────────────────────────────────
    // Each column is wide enough for its own name; a message between neighbours widens
    // the pair so its text has somewhere to sit.
    let mut widths: Vec<f32> = s.actors.iter().map(|a| (m.text_size(&a.name, measure).0 + 2.0 * m.pad_x).max(m.min_w)).collect();
    for e in &s.events {
        if let Event::Message(msg) = e {
            let span = msg.from.max(msg.to) - msg.from.min(msg.to);
            let need = m.text_size(&label_of(s, msg, 1), measure).0 + 2.0 * m.pad_x;
            if span == 1 {
                let i = msg.from.min(msg.to);
                widths[i] = widths[i].max(need - gap);
            }
        }
    }
    let title_h = if s.title.is_empty() { 0.0 } else { 2.0 * m.eh };
    let mut cx = Vec::with_capacity(n);
    let mut rects = Vec::with_capacity(n);
    let mut x = m.margin;
    for (i, a) in s.actors.iter().enumerate() {
        // Participants inside a `box` sit within its frame, so leave room for its border.
        if a.bx.is_some() && (i == 0 || s.actors[i - 1].bx != a.bx) {
            x += m.pad_x;
        }
        let r = Rect::new(x, m.margin + title_h, widths[i], actor_h);
        cx.push(r.x + r.w / 2.0);
        rects.push(r);
        x += widths[i] + gap;
        if a.bx.is_some() && (i + 1 == n || s.actors[i + 1].bx != a.bx) {
            x += m.pad_x;
        }
    }
    let width = x - gap + m.margin;
    let top = m.margin + title_h + actor_h;

    // ── the timeline ─────────────────────────────────────────────────────────
    let mut sb = Builder::new(m.margin);
    let mut y = top + m.eh;
    let mut number = 0usize;
    let mut active: Vec<Vec<f32>> = vec![Vec::new(); n]; // open activation starts, per actor
    let mut bars: Vec<Rect> = Vec::new();
    // (y0, caption, divisions as (y, caption)) — `else` / `and` label their own division
    // rather than being crammed into the frame's own caption.
    let mut open: Vec<(f32, String, Vec<(f32, String)>)> = Vec::new();
    let mut done: Vec<(Rect, String, Vec<(f32, String)>)> = Vec::new();
    let mut deaths: Vec<(usize, f32)> = Vec::new();

    for e in &s.events {
        match e {
            Event::Message(msg) => {
                number += 1;
                let text = label_of(s, msg, number);
                if msg.from == msg.to {
                    // A self-call: out of the lifeline, down, and back into it.
                    let x0 = cx[msg.from] + bar_w;
                    let x1 = x0 + m.text_size(&text, measure).0.min(8.0 * m.ew) + 2.0 * m.ew;
                    let (y0, y1) = (y, y + step * 0.6);
                    sb.path(vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)], msg.stroke, Cap::None, msg.head, text, Role::Edge);
                    y += step * 0.6 + m.eh;
                } else {
                    let (a, b) = (cx[msg.from], cx[msg.to]);
                    let dir = if b > a { 1.0 } else { -1.0 };
                    let from_x = a + dir * bar_offset(&active[msg.from], bar_w);
                    let to_x = b - dir * bar_offset(&active[msg.to], bar_w);
                    sb.path(vec![(from_x, y), (to_x, y)], msg.stroke, Cap::None, msg.head, text, Role::Edge);
                    y += step;
                }
                if msg.activate {
                    active[msg.to].push(y - step);
                }
                if msg.deactivate {
                    close_bar(&mut active[msg.from], &mut bars, cx[msg.from], y, bar_w);
                }
            }
            Event::Activate(i) => active[*i].push(y),
            Event::Deactivate(i) => close_bar(&mut active[*i], &mut bars, cx[*i], y, bar_w),
            Event::Destroy(i) => {
                deaths.push((*i, y));
                y += m.eh;
            }
            Event::Note { pos, from, to, text } => {
                let (tw, th) = m.text_size(text, measure);
                let (w, h) = (tw + 2.0 * m.pad_x, th + 2.0 * m.pad_y);
                let r = match pos {
                    NotePos::LeftOf => Rect::new((cx[*from] - w - m.pad_x).max(0.0), y, w, h),
                    NotePos::RightOf => Rect::new(cx[*from] + m.pad_x, y, w, h),
                    NotePos::Over => {
                        let (lo, hi) = (cx[*from].min(cx[*to]), cx[*from].max(cx[*to]));
                        let span = (hi - lo + 2.0 * m.pad_x).max(w);
                        Rect::new(((lo + hi) / 2.0 - span / 2.0).max(0.0), y, span, h)
                    }
                };
                sb.shape(Shape::Note, r, text.clone(), Role::Muted);
                y += h + m.eh;
            }
            Event::BlockStart { label, .. } => {
                open.push((y, label.clone(), Vec::new()));
                y += 1.5 * m.eh; // room for the frame's caption
            }
            Event::BlockElse { label } => {
                if let Some(f) = open.last_mut() {
                    f.2.push((y, label.clone()));
                }
                y += 1.5 * m.eh;
            }
            Event::BlockEnd => {
                if let Some((y0, label, dividers)) = open.pop() {
                    done.push((Rect::new(m.margin * 0.5, y0, width - m.margin, y - y0 + m.eh * 0.5), label, dividers));
                    y += m.eh;
                }
            }
        }
    }
    // Anything still open runs to the bottom of the diagram.
    let bottom = y.max(top + 2.0 * m.eh);
    for i in 0..n {
        while !active[i].is_empty() {
            close_bar(&mut active[i], &mut bars, cx[i], bottom, bar_w);
        }
    }
    for (y0, label, dividers) in open {
        done.push((Rect::new(m.margin * 0.5, y0, width - m.margin, bottom - y0), label, dividers));
    }

    // ── ink, back to front ───────────────────────────────────────────────────
    for (r, label, dividers) in &done {
        for (dy, caption) in dividers {
            sb.rule((r.x + m.ew, *dy), (r.right() - m.ew, *dy), Role::Muted);
            if !caption.is_empty() {
                sb.label(format!(" {caption} "), r.x + 2.0 * m.ew, *dy, Anchor::Start, TextSize::Small, Role::Muted);
            }
        }
        sb.group(*r, label.clone(), Role::Muted);
    }
    for (i, &x) in cx.iter().enumerate() {
        let death = deaths.iter().find(|(a, _)| *a == i).map(|(_, y)| *y);
        sb.path(vec![(x, top), (x, death.unwrap_or(bottom))], Stroke::Dashed, Cap::None, Cap::None, "", Role::Muted);
        if let Some(dy) = death {
            sb.path(vec![(x - m.ew, dy - m.eh * 0.4), (x + m.ew, dy + m.eh * 0.4)], Stroke::Solid, Cap::None, Cap::Cross, "", Role::Muted);
        }
    }
    for b in &bars {
        sb.shape(Shape::Rect, *b, String::new(), Role::Accent);
    }
    for (i, a) in s.actors.iter().enumerate() {
        sb.shape(if a.stick { Shape::Actor } else { Shape::Rect }, rects[i], a.name.clone(), Role::Node);
    }
    // A `box` frames its participants' columns across the head of the diagram.
    for (bi, title) in s.boxes.iter().enumerate() {
        let members: Vec<&Rect> = rects.iter().enumerate().filter(|(i, _)| s.actors[*i].bx == Some(bi)).map(|(_, r)| r).collect();
        if let (Some(first), Some(last)) = (members.first(), members.last()) {
            sb.group(Rect::new(first.x - m.pad_x, first.y - m.eh, last.right() - first.x + 2.0 * m.pad_x, actor_h + m.eh), title.clone(), Role::Muted);
        }
    }
    if !s.title.is_empty() {
        sb.label(s.title.clone(), width / 2.0, m.margin * 0.5, Anchor::Middle, TextSize::Title, Role::Label);
    }

    let mut scene = sb.build();
    scene.width = scene.width.max(width.ceil() as u32);
    scene.height = scene.height.max((bottom + m.margin).ceil() as u32);
    scene
}

/// How far off the lifeline a message starts when activation bars are stacked on it.
fn bar_offset(open: &[f32], bar_w: f32) -> f32 {
    if open.is_empty() {
        0.0
    } else {
        bar_w * (open.len() as f32 * 0.5 + 0.5)
    }
}

/// Close the innermost activation on an actor, recording its bar.
fn close_bar(open: &mut Vec<f32>, bars: &mut Vec<Rect>, x: f32, y: f32, bar_w: f32) {
    if let Some(start) = open.pop() {
        let inset = bar_w * open.len() as f32 * 0.5;
        bars.push(Rect::new(x - bar_w / 2.0 + inset, start, bar_w, (y - start).max(bar_w)));
    }
}

/// A message's drawn text, numbered when the diagram asked for it.
fn label_of(s: &Sequence, m: &Message, number: usize) -> String {
    if s.autonumber && number > 0 {
        format!("{number}. {}", m.text)
    } else {
        m.text.clone()
    }
}

#[cfg(test)]
mod tests;
