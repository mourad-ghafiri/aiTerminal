//! The terminal grid + chrome renderer. Uses the shared size-parameterized
//! glyph cache (`corelib::gfx::text`), so each pane can render at its own font size
//! (per-pane / per-tab zoom).

use crate::plugin::StatusLine;
use corelib::gfx::text::{draw_text, measure_text, GlyphCache};
use corelib::gfx::{Canvas, Surface};
use corelib::types::{FontMetrics, Rect, Rgba8};
use platform::term::{Cell, CellFlags, Color, Selection, Term};
use corelib::theme::Theme;

/// Padding (px) around the grid inside a pane — the chrome gutter (design token).
pub const PAD: f32 = corelib::design::PANE_GUTTER;

/// Pixel size of the surface for a `cols`×`rows` grid at these metrics.
pub fn surface_size(cols: u16, rows: u16, m: &FontMetrics) -> (u32, u32) {
    let w = (cols as f32 * m.cell_w + 2.0 * PAD).ceil() as u32;
    let h = (rows as f32 * m.cell_h + 2.0 * PAD).ceil() as u32;
    (w.max(1), h.max(1))
}

fn resolve(c: Color, theme: &Theme, is_fg: bool) -> Rgba8 {
    match c {
        Color::Default => {
            if is_fg {
                theme.term_fg
            } else {
                theme.term_bg
            }
        }
        Color::Indexed(i) => theme.ansi(i),
        Color::Rgb(r, g, b) => Rgba8::rgb(r, g, b),
    }
}

/// How the cursor is drawn — `[appearance] cursor_style` (`block` is the default).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorStyle {
    Bar,
    Block,
    Underline,
}

impl CursorStyle {
    pub fn from_name(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "bar" => Self::Bar,
            "underline" => Self::Underline,
            _ => Self::Block, // the classic terminal cursor is the default
        }
    }
}

/// Draw a terminal's visible grid at `px` font size, top-left cell at `(ox, oy)`.
#[allow(clippy::too_many_arguments)]
pub fn render_grid(
    surface: &mut Surface,
    term: &Term,
    theme: &Theme,
    cache: &mut GlyphCache,
    px: f32,
    ox: f32,
    oy: f32,
    draw_cursor: bool,
    cursor_style: CursorStyle,
    selection: Option<&Selection>,
    // A ⌘-hover link to underline: `(display-row, col0, col1)` (exclusive end).
    link: Option<(u16, u16, u16)>,
) {
    let m = cache.metrics(px);
    let cols = term.cols();
    let rows = term.rows();

    // The grid owns its rectangle: clear it to the terminal background FIRST.
    // Incremental pane redraws reuse last frame's surface, and default-bg cells
    // skip their background fill below — without this clear, every pixel drawn
    // between glyphs (the caret above all) survives a redraw, stranding ghost
    // cursors on the line as you type, navigate, or delete.
    surface.fill_rect(Rect::new(ox, oy, cols as f32 * m.cell_w, rows as f32 * m.cell_h), theme.term_bg);

    // The cursor lives on the LIVE screen — only show it at the live bottom.
    let cursor_cell = if draw_cursor && term.cursor_visible() && term.at_bottom() {
        let (cx, cy) = term.cursor();
        Some((cx.min(cols.saturating_sub(1)), cy))
    } else {
        None
    };

    for y in 0..rows {
        // Honor the scroll offset: rows above it come from scrollback history. Borrow the
        // row (no per-frame clone). It may be NARROWER than `cols` — scrollback lines keep
        // their capture-time width and a widen doesn't re-flow history — so read it
        // bounds-safe (a short row reads as BLANK past its end) instead of indexing, which
        // would panic and (under `panic=abort`) abort the whole app.
        let row = term.display_row(y);
        let mut x = 0u16;
        while x < cols {
            let cell = row.get(x as usize).copied().unwrap_or(Cell::BLANK);
            if cell.is_wide_spacer() {
                x += 1;
                continue;
            }
            let width = if (x + 1) < cols && row.get((x + 1) as usize).is_some_and(|c| c.is_wide_spacer()) { 2 } else { 1 };

            let mut fg = resolve(cell.fg, theme, true);
            let mut bg = resolve(cell.bg, theme, false);
            if cell.flags.contains(CellFlags::REVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(CellFlags::DIM) {
                fg.a = (fg.a as u16 * 6 / 10) as u8;
            }
            // A block cursor paints its whole cell in the cursor color and the
            // glyph in the background color — classic terminal inversion.
            if cursor_style == CursorStyle::Block && cursor_cell == Some((x, y)) {
                bg = theme.cursor;
                fg = theme.term_bg;
            }

            let px0 = ox + x as f32 * m.cell_w;
            let py0 = oy + y as f32 * m.cell_h;
            let cw = m.cell_w * width as f32;

            if bg != theme.term_bg {
                surface.fill_rect(Rect::new(px0, py0, cw, m.cell_h), bg);
            }
            if let Some(sel) = selection {
                if sel.contains(x, y, cols) {
                    // Neutral translucent gray (the foreground at 50%), matching
                    // the shell-side TT_SEL_BG band — a LIGHT, unmistakable band;
                    // the glyphs draw on top so nothing selected is ever hidden.
                    let paint = Rgba8 { a: 0x80, ..theme.term_fg };
                    surface.fill_rect(Rect::new(px0, py0, cw, m.cell_h), paint);
                }
            }
            if cell.ch != ' ' && !cell.flags.contains(CellFlags::HIDDEN) {
                let baseline = py0 + m.ascent;
                let bold = cell.flags.contains(CellFlags::BOLD);
                if let Some(g) = cache.glyph(cell.ch, px) {
                    if !g.is_blank() {
                        let gx = (px0 + g.left as f32).round() as i32;
                        let gy = (baseline - g.top as f32).round() as i32;
                        surface.blit_mask(gx, gy, &g.coverage, g.width, g.height, fg);
                        if bold {
                            surface.blit_mask(gx + 1, gy, &g.coverage, g.width, g.height, fg);
                        }
                    }
                }
            }
            // ⌘-hover link cue: an accent underline under the hovered token's cells.
            if let Some((ly, c0, c1)) = link {
                if y == ly && x >= c0 && x < c1 {
                    surface.fill_rect(Rect::new(px0, py0 + m.cell_h - 2.0, cw, 1.5), theme.accent);
                }
            }
            x += width as u16;
        }
    }

    if let Some((cx, cy)) = cursor_cell {
        let px0 = ox + cx as f32 * m.cell_w;
        let py0 = oy + cy as f32 * m.cell_h;
        match cursor_style {
            CursorStyle::Block => {} // painted with its cell in the loop above
            CursorStyle::Underline => {
                let h = (m.cell_h * 0.12).clamp(2.0, 4.0);
                surface.fill_rounded_rect(Rect::new(px0, py0 + m.cell_h - h, m.cell_w, h), h * 0.5, theme.cursor);
            }
            CursorStyle::Bar => {
                // A rounded caret — softer than a sharp rect, macOS-insertion-point style.
                let w = (m.cell_w * 0.16).max(2.0);
                surface.fill_rounded_rect(Rect::new(px0, py0, w, m.cell_h), w * 0.5, theme.cursor);
            }
        }
    }

    // A scrollback indicator on the right edge when scrolled up into history.
    let sb = term.scrollback_len();
    if sb > 0 && term.scroll_offset() > 0 {
        let total = (sb + rows as usize) as f32;
        let grid_h = rows as f32 * m.cell_h;
        let thumb_h = (grid_h * (rows as f32 / total)).max(24.0);
        // offset 0 = bottom; map to a thumb position from top.
        let frac = 1.0 - (term.scroll_offset() as f32 / sb as f32);
        let thumb_y = oy + frac * (grid_h - thumb_h);
        let tw = (m.cell_w * 0.18).clamp(3.0, 5.0);
        surface.fill_rounded_rect(Rect::new(ox + cols as f32 * m.cell_w - tw, thumb_y, tw, thumb_h), tw * 0.5, theme.muted);
    }
}

/// Render the grid with its top edge at `grid_top`, clearing the surface first.
pub fn render_terminal_at(
    surface: &mut Surface,
    term: &Term,
    theme: &Theme,
    cache: &mut GlyphCache,
    px: f32,
    grid_top: f32,
) {
    surface.clear(theme.term_bg);
    render_grid(surface, term, theme, cache, px, PAD, grid_top, true, CursorStyle::Block, None, None);
}

pub fn render_terminal(surface: &mut Surface, term: &Term, theme: &Theme, cache: &mut GlyphCache, px: f32) {
    render_terminal_at(surface, term, theme, cache, px, PAD);
}

/// Render one pane (grid + focus border) inside `rect` at font size `px`.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub fn render_pane(
    surface: &mut Surface,
    term: &Term,
    theme: &Theme,
    cache: &mut GlyphCache,
    px: f32,
    rect: Rect,
    focused: bool,
    cursor_style: CursorStyle,
    selection: Option<&Selection>,
    link: Option<(u16, u16, u16)>,
) {
    render_grid(surface, term, theme, cache, px, rect.x + PAD, rect.y + PAD, focused, cursor_style, selection, link);
    if focused {
        let t = 2.0;
        let c = theme.accent;
        surface.fill_rect(Rect::new(rect.x, rect.y, rect.w, t), c);
        surface.fill_rect(Rect::new(rect.x, rect.bottom() - t, rect.w, t), c);
        surface.fill_rect(Rect::new(rect.x, rect.y, t, rect.h), c);
        surface.fill_rect(Rect::new(rect.right() - t, rect.y, t, rect.h), c);
    }
}

/// A tab's 1-based index (its stable identity, used to key the hit rects — not drawn), app
/// icon, name, and active state. The renderer owns how the `icon name` pill is laid out, so the
/// visual design lives entirely in this file.
pub struct TabInfo {
    pub index: usize,
    pub icon: String,
    pub title: String,
    pub active: bool,
}

/// Height in px of a horizontal (top/bottom) tab strip for these metrics. Shared
/// by the live layout (`gui::mod`) and the render so the reserved area and the
/// drawn strip always agree.
///
/// Design A — "Modern pill tabs": the strip is roomy enough to seat a fully
/// rounded pill (radius = pill-height/2) with breathing room above and below, so
/// the active tab reads as a gently elevated chip rather than a cramped box.
pub fn tab_bar_height(m: &FontMetrics) -> f32 {
    (m.cell_h + 18.0).ceil()
}

/// Fit `s` into `max_w` px, appending an ellipsis if it must be truncated, so a
/// long tab title (or a modal body line) ends in `…` rather than being clipped mid-glyph.
pub(in crate::gui) fn fit_label(cache: &mut GlyphCache, s: &str, px: f32, max_w: f32) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    if measure_text(cache, s, px) <= max_w {
        return s.to_string();
    }
    let ell = "\u{2026}";
    let budget = (max_w - measure_text(cache, ell, px)).max(0.0);
    let mut out = String::new();
    let mut w = 0.0;
    for ch in s.chars() {
        let cw = measure_text(cache, &ch.to_string(), px);
        if w + cw > budget {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push_str(ell);
    out
}

/// Render a horizontal tab strip with its top edge at `y`. `at_bottom` is true
/// when the strip sits at the window's bottom (so the 1px divider goes on its top
/// edge, against the panes above). Returns `(height, per-tab clickable rects)`.
///
/// Design A: the active tab is a fully-rounded pill (radius = height/2) filled
/// with a soft vertical gradient and seated on a gentle drop shadow; inactive
/// tabs are flat muted text. The pill rounding + a thin accent underline that
/// hugs the pane edge keep all four orientations speaking the same language.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_tab_bar_top(
    surface: &mut Surface,
    tabs: &[TabInfo],
    theme: &Theme,
    cache: &mut GlyphCache,
    px: f32,
    width_px: u32,
    y: f32,
    at_bottom: bool,
    drag: Option<&super::TabDrag>,
) -> (f32, Vec<(usize, Rect)>) {
    let m = cache.metrics(px);
    let h = tab_bar_height(&m);
    let w = width_px as f32;

    // Base strip + a hairline divider on the edge that meets the panes.
    surface.fill_rect(Rect::new(0.0, y, w, h), theme.surface);
    let divider_y = if at_bottom { y } else { y + h - 1.0 };
    surface.fill_rect(Rect::new(0.0, divider_y, w, 1.0), theme.border());

    // Pill geometry: a chip inset vertically inside the strip, fully rounded.
    let v_inset = 4.0_f32;
    let pill_h = (h - 2.0 * v_inset).max(m.cell_h + 4.0);
    let pill_y = y + (h - pill_h) * 0.5;
    let radius = pill_h * 0.5;
    let pad_x = 14.0_f32; // generous horizontal padding inside each pill
    let gap = 7.0_f32; // space between tabs
    let baseline = pill_y + (pill_h - m.cell_h) * 0.5 + m.ascent;

    // Theme-aware pill fill: a subtle vertical gradient lifting the surface, with
    // a touch of accent so the active chip pops on noir without looking washed-out
    // on colourful themes.
    let pill_top = theme.surface_hover().mix(theme.accent, 0.14);
    let pill_bot = theme.surface_hover().darken(0.04);

    // Scroll-to-active: with many tabs the strip would clip the active one off the
    // right edge. Pick the first visible tab so the active tab always fits — walking
    // left from it using each tab's natural width (clamped, so one long title can't
    // dominate the calc). The leftmost tab stays first until the active tab can't fit.
    let active = tabs.iter().position(|t| t.active).unwrap_or(0);
    let icon_seg = |t: &TabInfo| if t.icon.is_empty() { String::new() } else { format!("{} ", t.icon) };
    let nat = |cache: &mut GlyphCache, t: &TabInfo| -> f32 {
        let cw = measure_text(cache, &icon_seg(t), px);
        (cw + measure_text(cache, &t.title, px) + 2.0 * pad_x).min(240.0)
    };
    let avail = (w - 2.0 * (PAD + 2.0)).max(1.0);
    let mut first = active;
    let mut used = nat(cache, &tabs[active.min(tabs.len().saturating_sub(1))]);
    while first > 0 {
        let prev = nat(cache, &tabs[first - 1]) + gap;
        if used + prev > avail {
            break;
        }
        used += prev;
        first -= 1;
    }

    // The tab being reordered (only once the pointer has actually moved): drawn as a faded
    // ghost in its slot, with an elevated copy following the cursor + an insertion bar.
    let dragging_from = drag.filter(|d| d.moved).map(|d| d.from);

    let mut rects = Vec::new();
    let mut x = PAD + 2.0;
    // A subtle "more tabs to the left" affordance when the strip is scrolled.
    if first > 0 {
        draw_text(surface, cache, "\u{2039}", px, x, baseline, theme.muted, w, false);
        x += measure_text(cache, "\u{2039} ", px);
    }
    let mut clipped_right = false;
    for t in &tabs[first..] {
        let icon = icon_seg(t);
        let icon_w = measure_text(cache, &icon, px);
        // Clamp each pill so the row never runs off the right edge; ellipsise the
        // name to the space that leaves, then size the pill to the drawn label.
        let max_pill_w = (w - x - PAD).max(icon_w + 24.0);
        let name = fit_label(cache, &t.title, px, max_pill_w - 2.0 * pad_x - icon_w);
        let name_w = measure_text(cache, &name, px);
        let pill_w = (icon_w + name_w + 2.0 * pad_x).min(max_pill_w);
        let r = Rect::new(x, pill_y, pill_w, pill_h);
        let is_ghost = dragging_from == Some(t.index - 1);

        if t.active && !is_ghost {
            // Soft drop shadow beneath the pill for gentle elevation.
            let shadow = Rect::new(r.x, r.y + 2.0, r.w, r.h);
            surface.fill_rounded_rect_soft(shadow, radius, theme.shadow(), 6.0);
            // Gradient-filled pill + an accent ring to crisp the edge.
            surface.fill_rounded_rect_gradient(r, radius, pill_top, pill_bot);
            surface.stroke_rounded_rect(r, radius, 1.0, theme.accent.with_alpha(0x80));
        }

        let tx = x + pad_x;
        let clip = r.right() - pad_x * 0.6;
        // The ghost (the lifted tab's vacated slot) reads faintly; its real copy floats below.
        let name_color = if is_ghost { theme.muted.with_alpha(0x66) } else if t.active { theme.fg } else { theme.muted };
        let nx = draw_text(surface, cache, &icon, px, tx, baseline, name_color, clip, false);
        draw_text(surface, cache, &name, px, nx, baseline, name_color, clip, false);

        rects.push((t.index - 1, r));
        x = r.right() + gap;
        if x >= w - PAD {
            clipped_right = tabs[first..].last().map(|l| l.index) != Some(t.index);
            break;
        }
    }
    // ... and a "more tabs to the right" affordance when some are clipped off the end.
    if clipped_right {
        draw_text(surface, cache, "\u{203A}", px, w - PAD - 6.0, baseline, theme.muted, w, false);
    }

    // Drag feedback: an accent insertion bar at the drop gap + the lifted pill floating at the
    // cursor (horizontal strip → it follows the pointer's x).
    if let Some(d) = drag.filter(|d| d.moved) {
        draw_drop_bar(surface, &rects, theme, d.gap, pill_y, pill_h, true);
        if let Some(t) = tabs.get(d.from) {
            let icon = icon_seg(t);
            let iw = measure_text(cache, &icon, px);
            let nw = measure_text(cache, &t.title, px);
            let fw = (iw + nw + 2.0 * pad_x).min(240.0);
            let fx = (d.cursor.x - fw * 0.5).clamp(PAD, (w - PAD - fw).max(PAD));
            draw_floating_pill(surface, cache, theme, px, &icon, &t.title, Rect::new(fx, pill_y, fw, pill_h), pad_x, m.ascent);
        }
    }
    (h, rects)
}

/// Draw the accent **insertion bar** for a tab-reorder drag: a thin rounded accent line at the
/// gap where the dragged tab will land. `gap` is an absolute tab index (`0..=len`); the bar sits
/// at the leading edge of the visible pill at `gap`, or after the last visible pill when the gap
/// is past it. `horizontal` picks the bar's orientation (x-line for top/bottom, y-line for sides).
fn draw_drop_bar(surface: &mut Surface, rects: &[(usize, Rect)], theme: &Theme, gap: usize, cross_pos: f32, cross_len: f32, horizontal: bool) {
    let Some(&(_, first)) = rects.first() else { return };
    // Leading edge of the pill at index == gap, else just after the last visible pill.
    let at = rects.iter().find(|(i, _)| *i == gap).map(|(_, r)| if horizontal { r.x } else { r.y });
    let lead = at.unwrap_or_else(|| {
        let (_, last) = rects.last().unwrap();
        if horizontal { last.right() + 3.0 } else { last.bottom() + 3.0 }
    });
    let thick = 2.5_f32;
    let bar = if horizontal {
        Rect::new(lead - 4.0, cross_pos, thick, cross_len)
    } else {
        Rect::new(first.x, lead - 4.0, cross_len, thick)
    };
    surface.fill_rounded_rect(bar, thick * 0.5, theme.accent);
}

/// Draw the lifted tab as an elevated floating pill (gradient fill + shadow + accent ring +
/// `icon name`) — the "carry" cue shared by both strip orientations during a reorder drag.
#[allow(clippy::too_many_arguments)]
fn draw_floating_pill(surface: &mut Surface, cache: &mut GlyphCache, theme: &Theme, px: f32, icon: &str, title: &str, r: Rect, pad_x: f32, ascent: f32) {
    let radius = r.h * 0.5;
    let m = cache.metrics(px);
    surface.fill_rounded_rect_soft(Rect::new(r.x, r.y + 3.0, r.w, r.h), radius, theme.shadow(), 9.0);
    surface.fill_rounded_rect_gradient(r, radius, theme.surface_hover().mix(theme.accent, 0.22), theme.surface_hover());
    surface.stroke_rounded_rect(r, radius, 1.4, theme.accent);
    let tx = r.x + pad_x;
    let baseline = r.y + (r.h - m.cell_h) * 0.5 + ascent;
    let clip = r.right() - pad_x * 0.6;
    let icon_w = measure_text(cache, icon, px);
    let name = fit_label(cache, title, px, (clip - tx - icon_w).max(0.0));
    let nx = draw_text(surface, cache, icon, px, tx, baseline, theme.fg, clip, false);
    draw_text(surface, cache, &name, px, nx, baseline, theme.fg, clip, false);
}

/// Width of the vertical tab sidebar. Wide enough for a comfortable full-width
/// pill with generous padding and ellipsised names.
pub const SIDE_TAB_W: f32 = 190.0;

/// Render a vertical tab sidebar at `x` spanning `[y, y+height]`. Returns the
/// per-tab clickable rects.
///
/// Design A: each tab is a full-width rounded pill row; the active one gets the
/// exact same gradient fill + drop shadow + accent ring as the top/bottom bar, so
/// all four orientations speak one visual language (the pill itself is the active
/// indicator — no edge sliver to clash with the rounded corners).
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_tab_bar_side(
    surface: &mut Surface,
    tabs: &[TabInfo],
    theme: &Theme,
    cache: &mut GlyphCache,
    px: f32,
    x: f32,
    y: f32,
    height: f32,
    divider_on_left: bool,
    drag: Option<&super::TabDrag>,
) -> Vec<(usize, Rect)> {
    let m = cache.metrics(px);
    let w = SIDE_TAB_W;

    // `divider_on_left == true` is the LEFT sidebar (panes lie to its right);
    // `false` is the RIGHT sidebar (panes to its left). The divider + the active
    // accent bar both hug the pane-facing inner edge so the indicator is mirrored
    // consistently across sides.
    let pane_on_right = divider_on_left;

    surface.fill_rect(Rect::new(x, y, w, height), theme.surface);
    let dx = if pane_on_right { x + w - 1.0 } else { x };
    surface.fill_rect(Rect::new(dx, y, 1.0, height), theme.border());

    let row_h = (m.cell_h + 16.0).ceil();
    let gap = 6.0_f32;
    let side_pad = 12.0_f32; // pill inset from the column edges
    let pad_x = 14.0_f32; // text inset inside the pill
    let radius = row_h * 0.5;

    let pill_top = theme.surface_hover().mix(theme.accent, 0.14);
    let pill_bot = theme.surface_hover().darken(0.04);

    // Scroll-to-active: fixed row height, so keep the active row within the visible window.
    let rows_fit = (((height - 10.0) / (row_h + gap)).floor() as usize).max(1);
    let active = tabs.iter().position(|t| t.active).unwrap_or(0);
    let first = active.saturating_sub(rows_fit.saturating_sub(1)).min(tabs.len().saturating_sub(rows_fit));

    let dragging_from = drag.filter(|d| d.moved).map(|d| d.from);

    let mut rects = Vec::new();
    let mut cy = y + 10.0;
    for t in &tabs[first..] {
        let r = Rect::new(x + side_pad, cy, w - 2.0 * side_pad, row_h);
        let is_ghost = dragging_from == Some(t.index - 1);
        if t.active && !is_ghost {
            let shadow = Rect::new(r.x, r.y + 2.0, r.w, r.h);
            surface.fill_rounded_rect_soft(shadow, radius, theme.shadow(), 6.0);
            surface.fill_rounded_rect_gradient(r, radius, pill_top, pill_bot);
            surface.stroke_rounded_rect(r, radius, 1.0, theme.accent.with_alpha(0x80));
        }

        let tx = r.x + pad_x;
        let clip = r.right() - pad_x * 0.7;
        let baseline = cy + (row_h - m.cell_h) * 0.5 + m.ascent;
        let icon = if t.icon.is_empty() { String::new() } else { format!("{} ", t.icon) };
        let icon_w = measure_text(cache, &icon, px);
        let name = fit_label(cache, &t.title, px, (clip - tx - icon_w).max(0.0));
        let name_color = if is_ghost { theme.muted.with_alpha(0x66) } else if t.active { theme.fg } else { theme.muted };
        let nx = draw_text(surface, cache, &icon, px, tx, baseline, name_color, clip, false);
        draw_text(surface, cache, &name, px, nx, baseline, name_color, clip, false);

        rects.push((t.index - 1, r));
        cy += row_h + gap;
        if cy >= y + height {
            break;
        }
    }

    // Drag feedback: a horizontal insertion bar at the drop gap + the lifted pill following the
    // pointer's y (vertical strip), clamped to the column.
    if let Some(d) = drag.filter(|d| d.moved) {
        draw_drop_bar(surface, &rects, theme, d.gap, x + side_pad, w - 2.0 * side_pad, false);
        if let Some(t) = tabs.get(d.from) {
            let icon = if t.icon.is_empty() { String::new() } else { format!("{} ", t.icon) };
            let fy = (d.cursor.y - row_h * 0.5).clamp(y, (y + height - row_h).max(y));
            draw_floating_pill(surface, cache, theme, px, &icon, &t.title, Rect::new(x + side_pad, fy, w - 2.0 * side_pad, row_h), pad_x, m.ascent);
        }
    }
    rects
}

/// Height in px of the status bar for these metrics.
pub fn status_bar_height(m: &FontMetrics) -> f32 {
    (m.cell_h + 8.0).ceil()
}

/// Render the native status bar across the top of `surface`.
pub fn render_status_bar(
    surface: &mut Surface,
    line: &StatusLine,
    theme: &Theme,
    cache: &mut GlyphCache,
    px: f32,
    width_px: u32,
    y_top: f32,
) {
    let m = cache.metrics(px);
    let h = status_bar_height(&m);
    let w = width_px as f32;
    surface.fill_rect(Rect::new(0.0, y_top, w, h), theme.surface);
    // A hairline divider on the bar's TOP edge, separating it from the content above.
    surface.fill_rect(Rect::new(0.0, y_top, w, 1.0), theme.bg);
    let baseline = y_top + ((h - m.cell_h) * 0.5).max(0.0) + m.ascent;
    let gap = m.cell_w;

    let right_w: f32 = line.right.iter().map(|s| measure_text(cache, &s.text, px) + gap).sum();
    let left_limit = (w - PAD - right_w).max(PAD);

    let mut x = PAD;
    for seg in &line.left {
        if x >= left_limit {
            break;
        }
        let color = status_color(seg.fg.as_deref(), theme, theme.fg);
        x = draw_text(surface, cache, &seg.text, px, x, baseline, color, left_limit, false);
        x += gap;
    }
    let mut rx = w - PAD;
    for seg in line.right.iter().rev() {
        let tw = measure_text(cache, &seg.text, px);
        rx -= tw;
        let color = status_color(seg.fg.as_deref(), theme, theme.muted);
        draw_text(surface, cache, &seg.text, px, rx, baseline, color, w - PAD, false);
        rx -= gap;
    }
}

/// Resolve a status-segment colour token against the active theme. A theme role
/// name maps to that role (so the bar follows the theme); an explicit `#rrggbb`
/// is honoured for power users; anything else falls back to `default`.
fn status_color(token: Option<&str>, theme: &Theme, default: Rgba8) -> Rgba8 {
    let Some(t) = token else { return default };
    match t.trim().to_ascii_lowercase().as_str() {
        "fg" => theme.fg,
        "muted" => theme.muted,
        "accent" => theme.accent,
        "success" => theme.success,
        "warn" => theme.warn,
        "error" => theme.error,
        "surface" => theme.surface,
        "bg" => theme.bg,
        other => Rgba8::from_hex_str(other).unwrap_or(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::testkit::MockShaper;

    fn lit(s: &Surface, theme: &Theme) -> usize {
        let bg = theme.term_bg.to_bgra_premul() & 0x00ff_ffff;
        s.pixels().iter().filter(|&&p| (p & 0x00ff_ffff) != bg).count()
    }

    #[test]
    fn renders_text_into_pixels() {
        let mut t = Term::new(10, 2);
        t.feed(b"Hi");
        let mut cache = GlyphCache::new(Box::new(MockShaper));
        let (w, h) = surface_size(10, 2, &cache.metrics(20.0));
        let mut s = Surface::new(w, h);
        let th = corelib::theme::midnight();
        render_terminal(&mut s, &t, &th, &mut cache, 20.0);
        assert!(lit(&s, &th) > 0);
    }

    #[test]
    fn incremental_rerender_leaves_no_ghost_carets() {
        // The reported bug: typing/navigating stranded old caret bars on the line.
        // Incremental pane redraws reuse the same surface, so a re-render after a
        // cursor move must be pixel-identical to a render on a fresh surface.
        let mut t = Term::new(8, 2);
        t.feed(b"ls -al");
        let mut cache = GlyphCache::new(Box::new(MockShaper));
        let (w, h) = surface_size(8, 2, &cache.metrics(20.0));
        let th = corelib::theme::midnight();
        let mut reused = Surface::new(w, h);
        reused.clear(th.term_bg);
        for style in [CursorStyle::Bar, CursorStyle::Block, CursorStyle::Underline] {
            render_grid(&mut reused, &t, &th, &mut cache, 20.0, 0.0, 0.0, true, style, None, None);
            t.feed(b"\x1b[2D"); // cursor two cells left — the old caret must vanish
            render_grid(&mut reused, &t, &th, &mut cache, 20.0, 0.0, 0.0, true, style, None, None);
            let mut fresh = Surface::new(w, h);
            fresh.clear(th.term_bg);
            render_grid(&mut fresh, &t, &th, &mut cache, 20.0, 0.0, 0.0, true, style, None, None);
            assert_eq!(reused.pixels(), fresh.pixels(), "{style:?}: re-render into a used surface must equal a fresh render");
            t.feed(b"\x1b[2C"); // restore for the next style
        }
    }

    #[test]
    fn cursor_styles_resolve_and_render_distinctly() {
        assert_eq!(CursorStyle::from_name("block"), CursorStyle::Block);
        assert_eq!(CursorStyle::from_name(" Underline "), CursorStyle::Underline);
        assert_eq!(CursorStyle::from_name("bar"), CursorStyle::Bar);
        assert_eq!(CursorStyle::from_name("nonsense"), CursorStyle::Block); // safe fallback = the default
        // Each style paints a different cursor footprint on an otherwise empty grid.
        let t = Term::new(6, 2);
        let mut cache = GlyphCache::new(Box::new(MockShaper));
        let (w, h) = surface_size(6, 2, &cache.metrics(20.0));
        let th = corelib::theme::midnight();
        let mut lit_px = Vec::new();
        for style in [CursorStyle::Bar, CursorStyle::Block, CursorStyle::Underline] {
            let mut s = Surface::new(w, h);
            s.clear(th.term_bg);
            render_grid(&mut s, &t, &th, &mut cache, 20.0, 0.0, 0.0, true, style, None, None);
            lit_px.push(lit(&s, &th));
        }
        assert!(lit_px[0] > 0, "the bar caret draws");
        assert!(lit_px[1] > lit_px[0], "a block cursor fills more than the bar");
        assert!(lit_px[2] > 0 && lit_px[2] != lit_px[1], "underline draws its own footprint");
    }

    #[test]
    fn scrolled_terminal_renders_history() {
        // 5 lines into a 3-row screen → 2 lines in scrollback. Rendering at the live
        // bottom vs scrolled up must show DIFFERENT content (the history).
        let mut t = Term::new(8, 3);
        t.feed(b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD\r\nEEEE");
        let mut cache = GlyphCache::new(Box::new(MockShaper));
        let (w, h) = surface_size(8, 3, &cache.metrics(20.0));
        let th = corelib::theme::midnight();
        let mut live = Surface::new(w, h);
        render_terminal(&mut live, &t, &th, &mut cache, 20.0);
        t.scroll_view(2); // up to the top of history
        let mut scrolled = Surface::new(w, h);
        render_terminal(&mut scrolled, &t, &th, &mut cache, 20.0);
        assert_ne!(live.pixels(), scrolled.pixels(), "scrolling reveals scrollback history");
    }

    #[test]
    fn colored_background_cell_fills() {
        let mut t = Term::new(4, 1);
        t.feed(b"\x1b[41m ");
        let mut cache = GlyphCache::new(Box::new(MockShaper));
        let (w, h) = surface_size(4, 1, &cache.metrics(20.0));
        let mut s = Surface::new(w, h);
        let th = corelib::theme::midnight();
        render_terminal(&mut s, &t, &th, &mut cache, 20.0);
        let red = th.ansi(1).to_bgra_premul() & 0x00ff_ffff;
        assert!(s.pixels().iter().any(|&p| (p & 0x00ff_ffff) == red));
    }

    #[test]
    fn per_size_metrics_differ() {
        let mut cache = GlyphCache::new(Box::new(MockShaper));
        assert!(cache.metrics(30.0).cell_h > cache.metrics(15.0).cell_h);
    }

    #[test]
    fn fit_label_ellipsizes_when_too_long() {
        let mut cache = GlyphCache::new(Box::new(MockShaper));
        let full = "a very long tab title that will not fit";
        // Fits → unchanged.
        let wide = measure_text(&mut cache, full, 15.0) + 10.0;
        assert_eq!(fit_label(&mut cache, full, 15.0, wide), full);
        // Too narrow → truncated, ends in the ellipsis, and stays within budget.
        let narrow = measure_text(&mut cache, "a very", 15.0);
        let cut = fit_label(&mut cache, full, 15.0, narrow);
        assert!(cut.ends_with('\u{2026}'), "got {cut:?}");
        assert!(cut.chars().count() < full.chars().count());
        assert!(measure_text(&mut cache, &cut, 15.0) <= narrow + 0.5);
    }

    #[test]
    fn tab_bars_return_one_rect_per_tab() {
        let mut cache = GlyphCache::new(Box::new(MockShaper));
        let th = corelib::theme::midnight();
        let tabs = vec![
            TabInfo { index: 1, icon: "\u{1F5A5}".into(), title: "zsh".into(), active: false },
            TabInfo { index: 2, icon: String::new(), title: "vim".into(), active: true },
            TabInfo { index: 3, icon: "\u{1F3E0}".into(), title: "home".into(), active: false },
        ];
        let mut s = Surface::new(900, 200);
        s.clear(th.term_bg);
        // Top + bottom share the horizontal renderer; both yield a rect per tab and
        // the advertised height, and paint something.
        let (h, top) = render_tab_bar_top(&mut s, &tabs, &th, &mut cache, 15.0, 900, 0.0, false, None);
        assert_eq!(top.len(), tabs.len());
        assert_eq!(h, tab_bar_height(&cache.metrics(15.0)));
        let (_h, bot) = render_tab_bar_top(&mut s, &tabs, &th, &mut cache, 15.0, 900, 160.0, true, None);
        assert_eq!(bot.len(), tabs.len());
        // Left + right sidebars.
        let mut s2 = Surface::new(SIDE_TAB_W as u32 + 4, 400);
        s2.clear(th.term_bg);
        let left = render_tab_bar_side(&mut s2, &tabs, &th, &mut cache, 15.0, 0.0, 0.0, 400.0, true, None);
        let right = render_tab_bar_side(&mut s2, &tabs, &th, &mut cache, 15.0, 0.0, 0.0, 400.0, false, None);
        assert_eq!(left.len(), tabs.len());
        assert_eq!(right.len(), tabs.len());
        assert!(lit(&s, &th) > 0 && lit(&s2, &th) > 0);
    }

    #[test]
    fn tab_reorder_drag_shows_feedback_and_keeps_rects() {
        // A moved drag still returns one rect per tab AND paints the floating pill + insertion
        // bar (more lit pixels than a static strip) — so the "lift and drop" cue is visible.
        let mut cache = GlyphCache::new(Box::new(MockShaper));
        let th = corelib::theme::midnight();
        let tabs = vec![
            TabInfo { index: 1, icon: "\u{1F5A5}".into(), title: "zsh".into(), active: true },
            TabInfo { index: 2, icon: String::new(), title: "vim".into(), active: false },
            TabInfo { index: 3, icon: "\u{1F3E0}".into(), title: "home".into(), active: false },
        ];
        let drag = super::super::TabDrag {
            from: 0,
            grab: corelib::types::Point::new(40.0, 10.0),
            cursor: corelib::types::Point::new(500.0, 10.0),
            moved: true,
            gap: 3,
        };
        let mut s = Surface::new(900, 200);
        s.clear(th.term_bg);
        let (_h, rects) = render_tab_bar_top(&mut s, &tabs, &th, &mut cache, 15.0, 900, 0.0, false, Some(&drag));
        assert_eq!(rects.len(), tabs.len(), "every tab still has a hit rect mid-drag");
        let mut s_static = Surface::new(900, 200);
        s_static.clear(th.term_bg);
        let _ = render_tab_bar_top(&mut s_static, &tabs, &th, &mut cache, 15.0, 900, 0.0, false, None);
        assert!(lit(&s, &th) > lit(&s_static, &th), "the drag overlay paints extra pixels");
    }
}
