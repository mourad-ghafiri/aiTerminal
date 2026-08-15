//! The workspace input panel — ONE shape for every state, so the geometry never
//! jumps when a turn starts or the guard asks.
//!
//! The look is the settled harnesses' input, drawn with our engine: a heavy left
//! accent bar whose color states the mode (accent = build, warn = plan and the
//! guard's ask, success = auto), an elevated surface, the input rows drawn as real lines with a
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
        PanelState::Editing(view) | PanelState::Question { view, .. } => view.rows.len().max(1),
        _ => 1,
    }
}

/// A meta segment's color, resolved against the theme at draw time.
enum Tone {
    Muted,
    Accent,
    Warn,
    /// Auto mode's color — the sitting is flowing on the judge's approvals.
    Success,
}

/// The mode's tone — ONE mapping, so the bar and the meta word can never
/// disagree about what mode the sitting is in.
fn mode_tone(mode: crate::cli::workspace::screen::Mode) -> Tone {
    use crate::cli::workspace::screen::Mode;
    match mode {
        Mode::Plan => Tone::Warn,
        Mode::Build => Tone::Accent,
        Mode::Auto => Tone::Success,
    }
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

fn view_for(panel: &PanelState, status: &Status, tick: usize, elapsed: Option<std::time::Duration>) -> PanelView {
    use crate::cli::workspace::screen::Mode;
    match panel {
        PanelState::Editing(view) => {
            // The sitting's facts live in the sticky header now; the panel's meta
            // row says what THIS mode means, here, where the typing happens.
            let hint = match status.mode {
                Mode::Plan => "planning \u{2014} the finished plan will ask your approval",
                Mode::Build => "@ files \u{b7} / commands \u{b7} ! shell \u{b7} shift+tab mode",
                Mode::Auto => "\u{26a1} auto \u{2014} the judge answers confirms; you when it declines",
            };
            PanelView {
                bar: mode_tone(status.mode),
                rows: view.rows.clone(),
                cursor: Some(view.cursor),
                placeholder: view.rows.iter().all(|r| r.is_empty()),
                meta_left: vec![(hint.to_string(), Tone::Muted)],
            }
        }
        PanelState::Working { label, draft, steering } => {
            let row = match steering {
                Some(s) => format!("\u{21b3} steering: {s}"),
                None => draft.clone(),
            };
            let caret = steering.is_none().then(|| (0, row.chars().count()));
            let mut meta = vec![(format!("{} {label}", FRAMES[tick % FRAMES.len()]), Tone::Accent)];
            if let Some(e) = elapsed {
                meta.push((super::header::clock(e), Tone::Accent));
            }
            meta.push(("esc interrupts \u{b7} enter steers".into(), Tone::Muted));
            PanelView { bar: Tone::Accent, cursor: caret, placeholder: false, rows: vec![row], meta_left: meta }
        }
        PanelState::Question { text, view } => {
            let mut q: String = text.chars().take(64).collect();
            if text.chars().count() > 64 {
                q.push('\u{2026}');
            }
            PanelView {
                bar: Tone::Accent,
                rows: view.rows.clone(),
                cursor: Some(view.cursor),
                placeholder: false,
                meta_left: vec![(format!("? {q}"), Tone::Accent), ("enter answers \u{b7} esc declines".into(), Tone::Muted)],
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_panel(
    surface: &mut Surface,
    cache: &mut GlyphCache,
    theme: &Theme,
    base_px: f32,
    rect: Rect,
    panel: &PanelState,
    status: &Status,
    tick: usize,
    elapsed: Option<std::time::Duration>,
) {
    use corelib::gfx::text::{draw_text, measure_text};
    let m = cache.metrics(base_px);
    let v = view_for(panel, status, tick, elapsed);
    let tone = |t: &Tone| match t {
        Tone::Muted => theme.muted,
        Tone::Accent => theme.accent,
        Tone::Warn => theme.warn,
        Tone::Success => theme.success,
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
                "ask anything about this folder\u{2026}",
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

    // The meta row: the state's own hints, INSIDE the panel. The sitting's
    // facts (mode, model, spend, overlay) live in the sticky header.
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
}

#[cfg(test)]
mod tests;
