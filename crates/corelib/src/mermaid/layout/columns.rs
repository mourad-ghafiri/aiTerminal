//! Layout for the lane languages — timeline, user journey and kanban.
//!
//! All three are the same picture: titled columns, each holding a stack of cards. A
//! timeline's columns are periods, a journey's are sections, a kanban's are lists.

use super::super::scene::{Anchor, Builder, Cap, Role, Scene, Shape, Stroke, TextSize};
use super::super::Columns;
use super::{Measure, Metrics};
use crate::types::Rect;

pub(crate) fn layout(c: &Columns, m: &Metrics, measure: Measure) -> Scene {
    if c.lanes.is_empty() {
        return Scene::default();
    }
    let title_h = if c.title.is_empty() { 0.0 } else { 2.0 * m.eh };
    let head_h = 3.0 * m.eh; // border, title, border
    let gap = 2.0 * m.ew;

    // A column is as wide as the widest thing in it, header included.
    let widths: Vec<f32> = c
        .lanes
        .iter()
        .map(|lane| {
            let mut w = m.text_size(&lane.title, measure).0;
            for card in &lane.cards {
                w = w.max(m.text_size(&card_text(c, card), measure).0);
            }
            (w + 2.0 * m.pad_x).max(m.min_w)
        })
        .collect();

    let mut sb = Builder::new(m.margin);
    let mut x = m.margin;
    let top = m.margin + title_h;
    let mut bottom = top;
    for (i, lane) in c.lanes.iter().enumerate() {
        let w = widths[i];
        sb.shape(Shape::Rect, Rect::new(x, top, w, head_h), lane.title.clone(), Role::Accent);
        let mut y = top + head_h + m.pad_y;
        for card in &lane.cards {
            let text = card_text(c, card);
            let (_, th) = m.text_size(&text, measure);
            let h = th + 2.0 * m.pad_y;
            sb.shape(Shape::Round, Rect::new(x, y, w, h), text, Role::Node);
            y += h + m.pad_y;
        }
        bottom = bottom.max(y);
        x += w + gap;
    }
    // The timeline's spine: one line under the headers, tying the periods together.
    let right = x - gap;
    sb.path(vec![(m.margin, top + head_h + m.pad_y * 0.5), (right, top + head_h + m.pad_y * 0.5)], Stroke::Solid, Cap::None, Cap::None, "", Role::Muted);
    if !c.title.is_empty() {
        sb.label(c.title.clone(), (m.margin + right) / 2.0, m.margin * 0.5, Anchor::Middle, TextSize::Title, Role::Label);
    }
    let mut scene = sb.build();
    scene.height = scene.height.max((bottom + m.margin) as u32);
    scene
}

/// A card's drawn text: its own words, plus a score badge and any trailing detail.
fn card_text(c: &Columns, card: &super::super::Card) -> String {
    let mut t = card.text.clone();
    if c.scored {
        if let Some(score) = card.score {
            // A journey's 1–5 rating, drawn as a little meter rather than a bare number.
            let filled = score.clamp(0, 5) as usize;
            t.push_str(&format!("  {}{}", "●".repeat(filled), "○".repeat(5 - filled)));
        }
    }
    if !card.detail.is_empty() {
        t.push_str(&format!("\n{}", card.detail));
    }
    t
}
