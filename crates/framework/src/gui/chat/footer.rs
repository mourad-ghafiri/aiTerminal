//! The workspace's facts row — the statusline UNDER the input.
//!
//! The facts people glance at — where am I, which mode, how far along, what is
//! it costing, how long has it been running — must never scroll away. One row
//! pinned beneath the input panel, welcome screen included, drawn straight on
//! the pane's ground (no strip, no box), fed entirely by the [`Status`] the
//! REPL already composes.
//!
//! The composition is a pure function ([`segments`]) so the vocabulary — the
//! mode pill, the `▰▱` progress arithmetic, the k-formatting, what gets dropped
//! first when the window narrows — is unit-tested without a surface.

use crate::cli::workspace::screen::{Mode, Status};

use super::*;

/// What one header segment is, for the renderer to tone.
#[derive(Debug, PartialEq)]
pub(crate) enum Kind {
    /// `✦ root` — the surface's identity. Accent, bold, never dropped.
    Brand,
    /// The mode's name in a colored capsule. Never dropped.
    Pill,
    /// `@persona`, when pinned.
    Persona,
    /// `▰▰▱▱▱ 3/9` — the approved plan's progress. Accent.
    Progress,
    /// Muted facts: the model, the spend, the overlay dot.
    Muted,
    /// `0:42` — how long the working turn has been running. Accent.
    Clock,
}

/// One segment: its text, its kind, and how hard it clings when space runs out
/// (higher survives longer).
#[derive(Debug, PartialEq)]
pub(crate) struct Seg {
    pub(crate) text: String,
    pub(crate) kind: Kind,
    pub(crate) keep: u8,
}

fn seg(text: impl Into<String>, kind: Kind, keep: u8) -> Seg {
    Seg { text: text.into(), kind, keep }
}

/// Thousands, readable: `950`, `12.3k`, `4.1m`.
pub(crate) fn kfmt(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}m", n as f64 / 1_000_000.0),
    }
}

/// A five-cell progress bar plus the ratio: `▰▰▱▱▱ 3/9`.
pub(crate) fn progress(done: usize, total: usize) -> String {
    let filled = match total {
        0 => 0,
        t => (done * 5 + t / 2) / t,
    }
    .min(5);
    let bar: String = (0..5).map(|i| if i < filled { '\u{25b0}' } else { '\u{25b1}' }).collect();
    format!("{bar} {done}/{total}")
}

/// `0:42`, `12:05` — a working turn's age.
pub(crate) fn clock(elapsed: std::time::Duration) -> String {
    let s = elapsed.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// The header's two clusters, in display order. The left cluster is identity
/// and survives anything; the right cluster's segments carry `keep` weights so
/// the renderer can shed the least vital first on a narrow surface.
pub(crate) fn segments(status: &Status, elapsed: Option<std::time::Duration>) -> (Vec<Seg>, Vec<Seg>) {
    let mut left = vec![seg(format!("\u{2726} {}", status.root), Kind::Brand, 255), seg(status.mode.name(), Kind::Pill, 255)];
    if let Some(p) = &status.persona {
        left.push(seg(format!("@{p}"), Kind::Persona, 200));
    }
    let mut right = Vec::new();
    if let Some((done, total)) = status.tasks {
        right.push(seg(progress(done, total), Kind::Progress, 4));
    }
    if !status.model.is_empty() {
        right.push(seg(status.model.clone(), Kind::Muted, 2));
    }
    if status.tokens.0 + status.tokens.1 > 0 {
        right.push(seg(
            format!("{} in / {} out \u{b7} ${:.3}", kfmt(status.tokens.0), kfmt(status.tokens.1), status.cost),
            Kind::Muted,
            1,
        ));
    }
    right.push(seg(if status.overlay_on { "\u{25cf} project" } else { "\u{25cb} global" }, Kind::Muted, 3));
    if let Some(e) = elapsed {
        right.push(seg(clock(e), Kind::Clock, 5));
    }
    (left, right)
}

/// The row's height for a font size — layout's input.
pub(crate) fn footer_height(cell_h: f32) -> f32 {
    cell_h + 8.0
}

/// Draw the facts row into `rect`, straight on the pane's ground: the left
/// cluster from the left, the right cluster right-aligned — shedding its
/// lowest-`keep` segments until it fits beside the left one.
pub(crate) fn draw_footer(
    surface: &mut Surface,
    cache: &mut GlyphCache,
    theme: &Theme,
    base_px: f32,
    rect: Rect,
    status: &Status,
    elapsed: Option<std::time::Duration>,
) {
    use corelib::gfx::text::{draw_text, measure_text};
    let m = cache.metrics(base_px);

    let (left, mut right) = segments(status, elapsed);
    let baseline = rect.y + (rect.h - m.cell_h) * 0.5 + m.ascent;
    let mode_color = match status.mode {
        Mode::Plan => theme.warn,
        Mode::Build => theme.accent,
        Mode::Auto => theme.success,
    };
    let pill_pad = 8.0;

    // The left cluster, walked left to right.
    let mut x = rect.x + 12.0;
    for s in &left {
        match s.kind {
            Kind::Pill => {
                let w = measure_text(cache, &s.text, base_px);
                let pill = Rect::new(x, rect.y + (rect.h - m.cell_h - 4.0) * 0.5, w + 2.0 * pill_pad, m.cell_h + 4.0);
                surface.fill_rounded_rect(pill, (pill.h * 0.5).min(9.0), mode_color);
                draw_text(surface, cache, &s.text, base_px, x + pill_pad, baseline, theme.bg, rect.x + rect.w, true);
                x = pill.x + pill.w + 10.0;
            }
            _ => {
                let color = match s.kind {
                    Kind::Brand => theme.accent,
                    _ => theme.fg,
                };
                x = draw_text(surface, cache, &s.text, base_px, x, baseline, color, rect.x + rect.w, s.kind == Kind::Brand) + 10.0;
            }
        }
    }

    // The right cluster: shed the least vital until it fits beside the left.
    let sep = " \u{b7} ";
    let sep_w = measure_text(cache, sep, base_px);
    let width_of = |cache: &mut GlyphCache, segs: &[Seg]| -> f32 {
        segs.iter().map(|s| measure_text(cache, &s.text, base_px)).sum::<f32>() + sep_w * segs.len().saturating_sub(1) as f32
    };
    let avail = (rect.x + rect.w - 12.0 - x).max(0.0);
    while !right.is_empty() && width_of(cache, &right) > avail {
        let (weakest, _) = right.iter().enumerate().min_by_key(|(_, s)| s.keep).expect("non-empty");
        right.remove(weakest);
    }
    let mut rx = rect.x + rect.w - 12.0 - width_of(cache, &right);
    for (i, s) in right.iter().enumerate() {
        if i > 0 {
            rx = draw_text(surface, cache, sep, base_px, rx, baseline, theme.muted, rect.x + rect.w, false);
        }
        let (color, bold) = match s.kind {
            Kind::Progress | Kind::Clock => (theme.accent, true),
            _ => (theme.muted, false),
        };
        rx = draw_text(surface, cache, &s.text, base_px, rx, baseline, color, rect.x + rect.w, bold);
    }
}

#[cfg(test)]
mod tests;
