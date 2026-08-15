//! The workspace input panel — ONE shape for every state, so the geometry never
//! jumps when a turn starts or the guard asks.
//!
//! The look is the settled harnesses' input, drawn with our engine: a heavy left
//! accent bar whose color states the mode (accent = build, warn = plan and the
//! guard's ask), an elevated surface, the input rows drawn as real lines with a
//! true `(row, col)` caret, and a meta row INSIDE the panel's bottom — mode ·
//! persona · model on the left, tokens · cost · overlay on the right. There is
//! no separate status strip: the app's own footer keeps its job, this panel
//! keeps the sitting's.

use crate::cli::workspace::screen::{PanelState, Status};

use super::*;

/// The braille spinner, shared look with the CLI surfaces.
const FRAMES: [char; 10] = ['\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280f}'];
/// The left accent bar's width, in pixels.
const BAR_W: f32 = 3.0;
/// Horizontal padding between the bar and the text.
const PAD_X: f32 = 12.0;
/// Vertical padding above the first row and below the meta row.
const PAD_Y: f32 = 8.0;
/// The air between the input rows and the meta row.
const META_GAP: f32 = 6.0;

/// The panel's height for `input_rows` visible rows — pure, the layout's input.
pub(crate) fn panel_height(cell_h: f32, input_rows: usize) -> f32 {
    PAD_Y + input_rows.max(1) as f32 * cell_h + META_GAP + cell_h + PAD_Y
}

/// How many input rows the panel shows for a state — pure, the layout's input.
/// (Working shows the one draft row; the guard's ask shows its one question row.)
pub(crate) fn input_rows(panel: &PanelState) -> usize {
    match panel {
        PanelState::Editing(view) => view.rows.len().max(1),
        _ => 1,
    }
}

/// A meta segment's color, resolved against the theme at draw time.
enum Tone {
    Fg,
    Muted,
    Accent,
    Warn,
}

/// What the panel draws for the current state — the single mapping from the
/// model to pixels, so every state shares one shape.
struct PanelView {
    /// The mode color: the left bar and the chevron.
    bar: Tone,
    rows: Vec<String>,
    /// The caret's `(row, col)`; `None` hides it (the guard's ask).
    cursor: Option<(usize, usize)>,
    /// Show the placeholder behind the caret (empty editor only).
    placeholder: bool,
    meta_left: Vec<(String, Tone)>,
}

fn view_for(panel: &PanelState, status: &Status, tick: usize) -> PanelView {
    match panel {
        PanelState::Editing(view) => {
            let mut left = vec![
                (status.root.clone(), Tone::Fg),
                (if status.plan { "plan" } else { "build" }.to_string(), if status.plan { Tone::Warn } else { Tone::Accent }),
            ];
            if let Some(p) = &status.persona {
                left.push((format!("@{p}"), Tone::Fg));
            }
            if !status.model.is_empty() {
                left.push((status.model.clone(), Tone::Muted));
            }
            PanelView {
                bar: if status.plan { Tone::Warn } else { Tone::Accent },
                rows: view.rows.clone(),
                cursor: Some(view.cursor),
                placeholder: view.rows.iter().all(|r| r.is_empty()),
                meta_left: left,
            }
        }
        PanelState::Working { label, draft, steering } => {
            let row = match steering {
                Some(s) => format!("\u{21b3} steering: {s}"),
                None => draft.clone(),
            };
            let caret = steering.is_none().then(|| (0, row.chars().count()));
            PanelView {
                bar: Tone::Accent,
                cursor: caret,
                placeholder: false,
                rows: vec![row],
                meta_left: vec![
                    (format!("{} {label}", FRAMES[tick % FRAMES.len()]), Tone::Accent),
                    ("esc interrupts \u{b7} enter steers".into(), Tone::Muted),
                ],
            }
        }
        PanelState::Ask { act, reason } => PanelView {
            bar: Tone::Warn,
            rows: vec![format!("\u{26a0} the guard asks before {act} \u{2014} {reason}")],
            cursor: None,
            placeholder: false,
            meta_left: vec![("y allows \u{b7} n or esc refuses".into(), Tone::Warn)],
        },
        PanelState::Hidden => PanelView { bar: Tone::Muted, rows: vec![String::new()], cursor: None, placeholder: false, meta_left: Vec::new() },
    }
}

/// Draw the panel into `rect` (sized by [`panel_height`] for [`input_rows`]).
pub(crate) fn draw_panel(
    surface: &mut Surface,
    cache: &mut GlyphCache,
    theme: &Theme,
    base_px: f32,
    rect: Rect,
    panel: &PanelState,
    status: &Status,
    tick: usize,
) {
    use corelib::gfx::text::{draw_text, measure_text};
    let m = cache.metrics(base_px);
    let v = view_for(panel, status, tick);
    let tone = |t: &Tone| match t {
        Tone::Fg => theme.fg,
        Tone::Muted => theme.muted,
        Tone::Accent => theme.accent,
        Tone::Warn => theme.warn,
    };

    surface.fill_rounded_rect(rect, 8.0, theme.surface);
    // The mode's bar: the panel's left edge, inset to respect the rounding.
    surface.fill_rect(Rect::new(rect.x, rect.y + 2.0, BAR_W, rect.h - 4.0), tone(&v.bar));

    let left = rect.x + BAR_W + PAD_X;
    let right = rect.x + rect.w - PAD_X;
    let mut baseline = rect.y + PAD_Y + m.ascent;

    // The input rows, real lines each — the chevron leads the first.
    let chevron_end = draw_text(surface, cache, "\u{276f} ", base_px, left, baseline, tone(&v.bar), right, true);
    let caret_w = m.cell_w.max(4.0);
    let mut caret_px: Option<(f32, f32)> = None;
    for (i, row) in v.rows.iter().enumerate() {
        // Continuation rows align under the first row's text, past the chevron.
        let x0 = chevron_end;
        draw_text(surface, cache, row, base_px, x0, baseline, theme.fg, right, false);
        if v.cursor == Some((i, 0)) && row.is_empty() && i == 0 && v.placeholder {
            // Empty editor: caret right after the chevron, the placeholder behind it.
            draw_text(
                surface,
                cache,
                "ask anything \u{b7} / commands \u{b7} @ agents & flows \u{b7} ! shell",
                base_px,
                x0 + caret_w + 6.0,
                baseline,
                theme.muted,
                right,
                false,
            );
            caret_px = Some((x0, baseline - m.ascent));
        } else if let Some((cr, cc)) = v.cursor {
            if cr == i {
                let before: String = row.chars().take(cc).collect();
                caret_px = Some((x0 + measure_text(cache, &before, base_px), baseline - m.ascent));
            }
        }
        baseline += m.cell_h;
    }
    if let Some((cx, cy)) = caret_px {
        surface.fill_rect(Rect::new(cx, cy, caret_w, m.cell_h), tone(&v.bar));
    }

    // The meta row: the sitting's facts, INSIDE the panel.
    let meta_baseline = rect.y + rect.h - PAD_Y - m.cell_h + m.ascent;
    let mut x = left;
    let mut first = true;
    for (text, t) in v.meta_left.iter().filter(|(text, _)| !text.is_empty()) {
        if !first {
            x = draw_text(surface, cache, " \u{b7} ", base_px, x, meta_baseline, theme.muted, right, false);
        }
        first = false;
        x = draw_text(surface, cache, text, base_px, x, meta_baseline, tone(t), right, false);
    }
    let mut meta_right = String::new();
    if status.tokens.0 + status.tokens.1 > 0 {
        meta_right.push_str(&format!("{} in / {} out \u{b7} ${:.3} \u{b7} ", status.tokens.0, status.tokens.1, status.cost));
    }
    meta_right.push_str(if status.overlay_on { "\u{25cf} overlay" } else { "\u{25cb} global" });
    let w = measure_text(cache, &meta_right, base_px);
    let rx = (right - w).max(x + 16.0);
    draw_text(surface, cache, &meta_right, base_px, rx, meta_baseline, theme.muted, right, false);
}

#[cfg(test)]
mod tests;
