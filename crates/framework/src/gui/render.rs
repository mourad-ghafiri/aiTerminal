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

    // Composite native inline diagrams over their reserved rows (primary screen only, and
    // only while fully in view — a partially-scrolled diagram is hidden until fully visible,
    // so drawing never bleeds outside this pane's grid rectangle).
    if !term.in_alt_screen() {
        draw_placements(surface, term, theme, cache, px, ox, oy, m.cell_w, m.cell_h, cols, rows);
    } else {
        draw_alt_placements(surface, term, theme, cache, px, ox, oy, m.cell_w, m.cell_h, cols, rows);
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

/// Linear blend of two colors (`t` in 0..=1), opaque.
fn mix(a: Rgba8, b: Rgba8, t: f32) -> Rgba8 {
    let l = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    Rgba8::rgb(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b))
}

/// A thick line segment drawn as a quad (there is no stroke_line primitive).
fn draw_seg(surface: &mut Surface, a: (f32, f32), b: (f32, f32), thick: f32, color: Rgba8) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let (nx, ny) = (-dy / len * thick / 2.0, dx / len * thick / 2.0);
    surface.fill_polygon(&[(a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny), (a.0 - nx, a.1 - ny)], color);
}

/// A filled triangle arrowhead whose tip is at `tip`, pointing away from `from`.
fn draw_arrowhead(surface: &mut Surface, tip: (f32, f32), from: (f32, f32), size: f32, color: Rgba8) {
    let (dx, dy) = (tip.0 - from.0, tip.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let (ux, uy) = (dx / len, dy / len);
    let (px, py) = (-uy, ux);
    let base = (tip.0 - ux * size, tip.1 - uy * size);
    let l = (base.0 + px * size * 0.5, base.1 + py * size * 0.5);
    let r = (base.0 - px * size * 0.5, base.1 - py * size * 0.5);
    surface.fill_polygon(&[tip, l, r], color);
}

/// Composite each fully-visible diagram placement onto the pane surface.
#[allow(clippy::too_many_arguments)]
fn draw_placements(surface: &mut Surface, term: &Term, theme: &Theme, cache: &mut GlyphCache, px: f32, ox: f32, oy: f32, cw: f32, ch: f32, cols: u16, rows: u16) {
    let sb = term.scrollback_len() as isize;
    let off = term.scroll_offset() as isize;
    for p in term.placements() {
        let y_top = p.g as isize - sb + off;
        let y_bot = y_top + p.rows as isize;
        if y_top < 0 || y_bot > rows as isize {
            continue; // not fully in view → skip (never draw outside the pane)
        }
        let rect = Rect::new(ox + 3.0, oy + y_top as f32 * ch + 2.0, cols as f32 * cw - 6.0, p.rows as f32 * ch - 4.0);
        // Clear the reserved region (over any stray cells) then draw the diagram into it.
        surface.fill_rect(Rect::new(ox, oy + y_top as f32 * ch, cols as f32 * cw, p.rows as f32 * ch), theme.term_bg);
        match p.kind {
            platform::term::Inline::Diagram => draw_diagram(surface, cache, px, theme, rect, &p.source, cw, ch),
            platform::term::Inline::Image => draw_image(surface, cache, px, theme, rect, &p.source),
        }
    }
}

/// Composite alternate-screen diagram placements (a full-screen app like `@md edit`). Positioned
/// by absolute cell (`row`,`col`) and confined to `cols` columns, so a diagram in one split pane
/// never bleeds into another. Clipped to the pane's grid; off-screen placements are skipped.
#[allow(clippy::too_many_arguments)]
fn draw_alt_placements(surface: &mut Surface, term: &Term, theme: &Theme, cache: &mut GlyphCache, px: f32, ox: f32, oy: f32, cw: f32, ch: f32, cols: u16, rows: u16) {
    for p in term.alt_placements() {
        let span = p.cols.min(cols as usize - p.col.min(cols as usize));
        if p.col >= cols as usize || p.row >= rows as usize || span == 0 {
            continue;
        }
        let vis_rows = p.rows.min(rows as usize - p.row); // clamp height to the pane
        let x0 = ox + p.col as f32 * cw;
        let y0 = oy + p.row as f32 * ch;
        let w = span as f32 * cw;
        let h = vis_rows as f32 * ch;
        // Clear the reserved region (over any stray cells) then draw the diagram into it.
        surface.fill_rect(Rect::new(x0, y0, w, h), theme.term_bg);
        let inner = Rect::new(x0 + 3.0, y0 + 2.0, w - 6.0, h - 4.0);
        match p.kind {
            platform::term::Inline::Diagram => draw_diagram(surface, cache, px, theme, inner, &p.source, cw, ch),
            platform::term::Inline::Image => draw_image(surface, cache, px, theme, inner, &p.source),
        }
    }
}

/// A dashed/dotted line: the segment chopped into on/off runs.
fn draw_dashed(surface: &mut Surface, a: (f32, f32), b: (f32, f32), thick: f32, color: Rgba8, on: f32, off: f32) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len <= 0.001 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let step = (on + off).max(1.0);
    let mut t = 0.0;
    while t < len {
        let e = (t + on).min(len);
        draw_seg(surface, (a.0 + ux * t, a.1 + uy * t), (a.0 + ux * e, a.1 + uy * e), thick, color);
        t += step;
    }
}

/// A hollow triangle head (UML inheritance) — the outline of [`draw_arrowhead`].
fn draw_open_head(surface: &mut Surface, tip: (f32, f32), from: (f32, f32), size: f32, thick: f32, color: Rgba8) {
    let (dx, dy) = (tip.0 - from.0, tip.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let (ux, uy) = (dx / len, dy / len);
    let (px, py) = (-uy, ux);
    let base = (tip.0 - ux * size, tip.1 - uy * size);
    let l = (base.0 + px * size * 0.5, base.1 + py * size * 0.5);
    let r = (base.0 - px * size * 0.5, base.1 - py * size * 0.5);
    draw_seg(surface, tip, l, thick, color);
    draw_seg(surface, tip, r, thick, color);
    draw_seg(surface, l, r, thick, color);
}

/// A diamond end cap (UML aggregation hollow / composition filled).
fn draw_diamond_head(surface: &mut Surface, tip: (f32, f32), from: (f32, f32), size: f32, thick: f32, color: Rgba8, filled: bool) {
    let (dx, dy) = (tip.0 - from.0, tip.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let (ux, uy) = (dx / len, dy / len);
    let (px, py) = (-uy, ux);
    let mid = (tip.0 - ux * size * 0.5, tip.1 - uy * size * 0.5);
    let back = (tip.0 - ux * size, tip.1 - uy * size);
    let pts = [tip, (mid.0 + px * size * 0.35, mid.1 + py * size * 0.35), back, (mid.0 - px * size * 0.35, mid.1 - py * size * 0.35)];
    if filled {
        surface.fill_polygon(&pts, color);
    } else {
        for i in 0..4 {
            draw_seg(surface, pts[i], pts[(i + 1) % 4], thick, color);
        }
    }
}

/// Draw one scene item's end cap at `at`, pointing away from `from`.
#[allow(clippy::too_many_arguments)]
fn draw_cap(surface: &mut Surface, cap: corelib::mermaid::Cap, at: (f32, f32), from: (f32, f32), size: f32, thick: f32, color: Rgba8) {
    use corelib::mermaid::Cap;
    match cap {
        Cap::None => {}
        Cap::Arrow | Cap::Open => draw_arrowhead(surface, at, from, size, color),
        Cap::Triangle => draw_open_head(surface, at, from, size, thick, color),
        Cap::Diamond => draw_diamond_head(surface, at, from, size, thick, color, false),
        Cap::FilledDiamond => draw_diamond_head(surface, at, from, size, thick, color, true),
        Cap::Circle => surface.fill_circle(at.0, at.1, size * 0.35, color),
        Cap::Cross => {
            let s = size * 0.35;
            draw_seg(surface, (at.0 - s, at.1 - s), (at.0 + s, at.1 + s), thick, color);
            draw_seg(surface, (at.0 + s, at.1 - s), (at.0 - s, at.1 + s), thick, color);
        }
        Cap::Tick | Cap::CrowFoot => {
            let (dx, dy) = (at.0 - from.0, at.1 - from.1);
            let len = (dx * dx + dy * dy).sqrt().max(0.001);
            let (px, py) = (-dy / len * size * 0.4, dx / len * size * 0.4);
            draw_seg(surface, (at.0 - px, at.1 - py), (at.0 + px, at.1 + py), thick, color);
        }
    }
}

/// The color a scene role takes in the active theme. `Slot(n)` walks the bright ANSI
/// ramp, so categorical series (pie slices, sections) restyle with the theme.
fn role_color(role: corelib::mermaid::Role, theme: &Theme) -> Rgba8 {
    use corelib::mermaid::Role;
    match role {
        Role::Node => theme.accent,
        Role::Edge => theme.muted,
        Role::Label => theme.term_fg,
        Role::Muted => theme.muted,
        Role::Accent => theme.accent,
        Role::Slot(n) => theme.ansi(9 + n % 6),
    }
}

/// Decoded images, kept between frames so scrolling a README doesn't re-decode a logo on
/// every repaint. Keyed by path; bounded, because a document can name any number of files.
fn image_cache() -> &'static std::sync::Mutex<Vec<(String, Option<std::sync::Arc<corelib::types::DecodedImage>>)>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Vec<(String, Option<std::sync::Arc<corelib::types::DecodedImage>>)>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// The decoded image at `path`, decoding (and remembering) it on first use. A file that
/// can't be read or decoded is remembered as such, so we don't retry it every frame.
fn decoded_image(path: &str) -> Option<std::sync::Arc<corelib::types::DecodedImage>> {
    let mut cache = image_cache().lock().ok()?;
    if let Some((_, img)) = cache.iter().find(|(p, _)| p == path) {
        return img.clone();
    }
    let decoded = std::fs::read(path).ok().and_then(|bytes| platform::os::image_decoder().decode(&bytes)).map(std::sync::Arc::new);
    if cache.len() >= 32 {
        cache.remove(0);
    }
    cache.push((path.to_string(), decoded.clone()));
    decoded
}

/// Draw an image file into `rect`, keeping its aspect ratio and centering the result.
fn draw_image(surface: &mut Surface, cache: &mut GlyphCache, px: f32, theme: &Theme, rect: Rect, path: &str) {
    let Some(img) = decoded_image(path) else {
        // Unreadable: say so where the picture would have been, rather than leave a hole.
        let name = path.rsplit('/').next().unwrap_or(path);
        let m = cache.metrics(px);
        draw_text(surface, cache, &format!("▣ {name}"), px, rect.x, rect.y + m.ascent, theme.muted, rect.right(), false);
        return;
    };
    if img.width == 0 || img.height == 0 {
        return;
    }
    let scale = (rect.w / img.width as f32).min(rect.h / img.height as f32);
    let (w, h) = (img.width as f32 * scale, img.height as f32 * scale);
    surface.draw_image(Rect::new(rect.x + (rect.w - w) / 2.0, rect.y + (rect.h - h) / 2.0, w, h), &img);
}

/// Draw a parsed+laid-out diagram, scaled to fit `rect`.
fn draw_diagram(surface: &mut Surface, cache: &mut GlyphCache, px: f32, theme: &Theme, rect: Rect, source: &str, cw: f32, ch: f32) {
    use corelib::mermaid::{layout, parse, Anchor, Item, Stroke, TextSize};
    let Some(d) = parse(source) else { return };
    let lay = layout(&d, &|s: &str| (corelib::unicode::str_width(s) as u32 * cw as u32, ch as u32));
    if lay.width == 0 || lay.height == 0 {
        return;
    }
    let scale = (rect.w / lay.width as f32).min(rect.h / lay.height as f32).clamp(0.05, 2.0);
    let dw = lay.width as f32 * scale;
    let dh = lay.height as f32 * scale;
    let ox2 = rect.x + (rect.w - dw) / 2.0;
    let oy2 = rect.y + (rect.h - dh) / 2.0;
    let tp = |x: f32, y: f32| (ox2 + x * scale, oy2 + y * scale);

    let node_fill = mix(theme.term_bg, theme.accent, 0.14);
    let base_px = (px * scale).clamp(7.0, px);
    let line = (1.5 * scale).clamp(1.0, 3.0);
    let head = (9.0 * scale).clamp(5.0, 12.0);

    for item in &lay.items {
        match item {
            Item::Group { rect: r, title, role } => {
                let (gx, gy) = tp(r.x, r.y);
                let gr = Rect::new(gx, gy, r.w * scale, r.h * scale);
                let col = role_color(*role, theme);
                surface.fill_rounded_rect(gr, (8.0 * scale).clamp(3.0, 12.0), mix(theme.term_bg, col, 0.06));
                surface.stroke_rounded_rect(gr, (8.0 * scale).clamp(3.0, 12.0), line, mix(theme.term_bg, col, 0.5));
                if !title.is_empty() {
                    let m = cache.metrics(base_px);
                    draw_text(surface, cache, title, base_px, gr.x + 6.0, gr.y + m.ascent + 2.0, theme.muted, gr.right() - 4.0, false);
                }
            }
            Item::Path { points, stroke, tail, head: h, label, role } => {
                if points.len() < 2 {
                    continue;
                }
                let col = role_color(*role, theme);
                let thick = if *stroke == Stroke::Thick { line * 2.0 } else { line };
                let pts: Vec<(f32, f32)> = points.iter().map(|&(x, y)| tp(x, y)).collect();
                for w in pts.windows(2) {
                    match stroke {
                        Stroke::Dashed => draw_dashed(surface, w[0], w[1], thick, col, 6.0 * scale, 5.0 * scale),
                        Stroke::Dotted => draw_dashed(surface, w[0], w[1], thick, col, 2.0 * scale, 4.0 * scale),
                        _ => draw_seg(surface, w[0], w[1], thick, col),
                    }
                }
                let last = pts[pts.len() - 1];
                let prev = pts[pts.len() - 2];
                draw_cap(surface, *h, last, prev, head, thick, col);
                draw_cap(surface, *tail, pts[0], pts[1], head, thick, col);
                if !label.is_empty() {
                    // Sit on the middle of the middle segment, on a chip so the line
                    // doesn't strike through the text.
                    let mid = pts.len() / 2;
                    let (a, b) = (pts[mid - 1], pts[mid]);
                    let (mx, my) = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
                    let m = cache.metrics(base_px);
                    let tw = measure_text(cache, label, base_px);
                    surface.fill_rect(Rect::new(mx - tw / 2.0 - 2.0, my - m.cell_h / 2.0, tw + 4.0, m.cell_h), theme.term_bg);
                    draw_text(surface, cache, label, base_px, mx - tw / 2.0, my + m.ascent / 2.0, theme.muted, mx + tw / 2.0 + 2.0, false);
                }
            }
            Item::Shape { kind, rect: r, label, role } => {
                let (nx, ny) = tp(r.x, r.y);
                let (nw, nh) = (r.w * scale, r.h * scale);
                let nr = Rect::new(nx, ny, nw, nh);
                let edge = role_color(*role, theme);
                let fill = if matches!(role, corelib::mermaid::Role::Slot(_)) { mix(theme.term_bg, edge, 0.3) } else { node_fill };
                draw_node_shape(surface, *kind, nr, fill, edge, line, scale);
                if !label.is_empty() {
                    let m = cache.metrics(base_px);
                    let lines: Vec<&str> = label.split('\n').collect();
                    let total = lines.len() as f32 * m.cell_h;
                    let mut baseline = ny + (nh - total) / 2.0 + m.ascent;
                    for l in lines {
                        let tw = measure_text(cache, l, base_px).min(nw - 4.0);
                        draw_text(surface, cache, l, base_px, (nx + (nw - tw) / 2.0).max(nx + 2.0), baseline, theme.term_fg, nx + nw - 2.0, false);
                        baseline += m.cell_h;
                    }
                }
            }
            Item::Wedge { cx, cy, r, a0, a1, slot } => {
                let (wx, wy) = tp(*cx, *cy);
                surface.fill_wedge(wx, wy, r * scale, *a0, *a1, role_color(corelib::mermaid::Role::Slot(*slot), theme));
            }
            Item::Label { text, x, y, anchor, size, role } => {
                let lpx = match size {
                    TextSize::Title => (base_px * 1.15).min(px),
                    TextSize::Small => (base_px * 0.85).max(7.0),
                    TextSize::Normal => base_px,
                };
                let m = cache.metrics(lpx);
                let (lx, ly) = tp(*x, *y);
                let tw = measure_text(cache, text, lpx);
                let sx = match anchor {
                    Anchor::Start => lx,
                    Anchor::Middle => lx - tw / 2.0,
                    Anchor::End => lx - tw,
                };
                draw_text(surface, cache, text, lpx, sx, ly + m.ascent, role_color(*role, theme), sx + tw + 2.0, false);
            }
            Item::Rule { a, b, role } => draw_seg(surface, tp(a.0, a.1), tp(b.0, b.1), line, role_color(*role, theme)),
        }
    }
}

/// One node outline. Every mermaid shape reduces to a rounded rect, a polygon or a circle.
fn draw_node_shape(surface: &mut Surface, kind: corelib::mermaid::Shape, r: Rect, fill: Rgba8, edge: Rgba8, line: f32, scale: f32) {
    use corelib::mermaid::Shape;
    let (x, y, w, h) = (r.x, r.y, r.w, r.h);
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let slant = (w * 0.15).min(h * 0.6);
    let poly = |surface: &mut Surface, pts: &[(f32, f32)]| {
        surface.fill_polygon(pts, fill);
        for i in 0..pts.len() {
            draw_seg(surface, pts[i], pts[(i + 1) % pts.len()], line, edge);
        }
    };
    match kind {
        Shape::Diamond => poly(surface, &[(cx, y), (x + w, cy), (cx, y + h), (x, cy)]),
        Shape::Hexagon => poly(surface, &[(x + slant, y), (x + w - slant, y), (x + w, cy), (x + w - slant, y + h), (x + slant, y + h), (x, cy)]),
        Shape::Parallelogram => poly(surface, &[(x + slant, y), (x + w, y), (x + w - slant, y + h), (x, y + h)]),
        Shape::ParallelogramAlt => poly(surface, &[(x, y), (x + w - slant, y), (x + w, y + h), (x + slant, y + h)]),
        Shape::Trapezoid => poly(surface, &[(x + slant, y), (x + w - slant, y), (x + w, y + h), (x, y + h)]),
        Shape::TrapezoidAlt => poly(surface, &[(x, y), (x + w, y), (x + w - slant, y + h), (x + slant, y + h)]),
        Shape::Asymmetric => poly(surface, &[(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x + slant, cy)]),
        Shape::Circle | Shape::DoubleCircle => {
            let rad = w.min(h) / 2.0;
            surface.fill_circle(cx, cy, rad, fill);
            if kind == Shape::DoubleCircle {
                surface.fill_circle(cx, cy, (rad - line * 2.0).max(1.0), fill);
            }
        }
        Shape::Actor => {
            // A stick figure: head, body, arms, legs.
            let head_r = (h * 0.18).max(2.0);
            surface.fill_circle(cx, y + head_r, head_r, edge);
            draw_seg(surface, (cx, y + head_r * 2.0), (cx, y + h * 0.65), line, edge);
            draw_seg(surface, (cx - w * 0.18, y + h * 0.42), (cx + w * 0.18, y + h * 0.42), line, edge);
            draw_seg(surface, (cx, y + h * 0.65), (cx - w * 0.16, y + h), line, edge);
            draw_seg(surface, (cx, y + h * 0.65), (cx + w * 0.16, y + h), line, edge);
        }
        Shape::Bar => surface.fill_rect(r, edge),
        Shape::Note => {
            surface.fill_rounded_rect(r, (3.0 * scale).clamp(2.0, 6.0), fill);
            surface.stroke_rounded_rect(r, (3.0 * scale).clamp(2.0, 6.0), line, edge);
        }
        _ => {
            let radius = match kind {
                Shape::Stadium | Shape::Round | Shape::Cylinder => h / 2.0,
                _ => (6.0 * scale).clamp(3.0, 10.0),
            };
            surface.fill_rounded_rect(r, radius, fill);
            surface.stroke_rounded_rect(r, radius, line, edge);
            if kind == Shape::Subroutine {
                draw_seg(surface, (x + slant, y), (x + slant, y + h), line, edge);
                draw_seg(surface, (x + w - slant, y), (x + w - slant, y + h), line, edge);
            }
        }
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
