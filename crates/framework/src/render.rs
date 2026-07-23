//! The headless renderers — one rendered frame written to an image file (PPM or
//! PNG), so the whole PTY → `term` → `gfx` → glyphs → present pipeline is
//! verifiable without a GUI session.
//!
//! Each entry point owns the corelib/platform types (surfaces, glyph caches, the
//! shaper, the VT engine) so the binary stays a thin arg-parser: it calls these
//! and never names a `corelib::`/`platform::` path itself.

use std::io::Write;
use std::path::{Path, PathBuf};

use corelib::types::PtyCommand;
use platform::term::Term;

use crate::gui::render::{render_status_bar, render_terminal_at, status_bar_height, PAD};

/// The parameters for a headless terminal-grid render (`--render-ppm`).
pub struct TerminalRender {
    pub cols: u16,
    pub rows: u16,
    pub px: f32,
    pub cmd: String,
    pub plugins: Option<String>,
    pub theme: Option<String>,
}

/// Minimal binary PPM (P6) writer — uncompressed, so no PNG/DEFLATE dependency.
pub fn write_ppm(path: &str, pixels: &[u32], w: u32, h: u32) -> std::io::Result<()> {
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    out.reserve(pixels.len() * 3);
    for &p in pixels {
        out.push(((p >> 16) & 0xff) as u8);
        out.push(((p >> 8) & 0xff) as u8);
        out.push((p & 0xff) as u8);
    }
    std::fs::File::create(path)?.write_all(&out)
}

fn run_to_eof(cmd: &PtyCommand, term: &mut Term) -> std::io::Result<usize> {
    let pty = platform::os::spawn_pty(cmd)?;
    let mut buf = [0u8; 8192];
    let mut total = 0usize;
    loop {
        let n = pty.read(&mut buf)?;
        if n == 0 {
            break;
        }
        term.feed(&buf[..n]);
        total += n;
        if total > 1 << 20 {
            break; // 1 MiB safety cap
        }
    }
    Ok(total)
}

/// `--render-ppm <out>` — run the command to EOF through the VT engine, rasterize
/// the grid + the native (plugin-driven) status bar, and write the frame.
pub fn headless_render(args: &TerminalRender, out_path: &str) -> std::io::Result<()> {
    let cmd = PtyCommand {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), args.cmd.clone()],
        cols: args.cols,
        rows: args.rows,
        login: false,
        ..Default::default()
    };

    let mut term = Term::new(args.cols, args.rows);
    let bytes = run_to_eof(&cmd, &mut term)?;

    let mut cache = corelib::gfx::text::GlyphCache::new(platform::os::text_shaper());
    let m = cache.metrics(args.px);
    let bar_h = status_bar_height(&m);
    let w = (args.cols as f32 * m.cell_w + 2.0 * PAD).ceil() as u32;
    let total_h = (bar_h + args.rows as f32 * m.cell_h + PAD).ceil() as u32;
    let mut surface = corelib::gfx::Surface::new(w, total_h);
    let theme = match &args.theme {
        Some(n) => crate::config::Config::resolve_theme(n),
        None => corelib::theme::midnight(),
    };

    // terminal grid at the top; the status bar sits along the bottom edge
    render_terminal_at(&mut surface, &term, &theme, &mut cache, args.px, 0.0);

    // native status bar driven by the declarative plugin registry (loaded
    // dynamically from ~/.aiTerminal/plugins/ — empty by default).
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let ctx = crate::plugin::probe_context(&cwd, args.cols);
    let mut registry = crate::plugin::PluginRegistry::new();
    let store = crate::plugin::store::PluginStore::at(crate::config::Config::plugins_dir());
    for m in store.enabled_manifests() {
        registry.add_trusted(m);
    }
    if let Some(dir) = &args.plugins {
        let n = registry.load_dir(Path::new(dir));
        println!("loaded {n} declarative plugin(s) from {dir}");
    }
    let vars = registry.evaluate(&ctx);
    let line = registry.status_line(&vars);
    render_status_bar(&mut surface, &line, &theme, &mut cache, args.px, w, total_h as f32 - bar_h);

    write_ppm(out_path, surface.pixels(), w, total_h)?;

    let branch = match vars.get("git.branch") {
        "" => "—",
        b => b,
    };
    let plugins = registry.names().join(", ");
    println!(
        "rendered {w}×{total_h}px frame to {out_path}\n  consumed {bytes} PTY bytes · cell {:.0}×{:.0} · git:{branch}\n  plugins: {plugins}",
        m.cell_w, m.cell_h
    );
    Ok(())
}

/// `--render-icon <out.png>` — draw the application icon (a dark rounded tile with
/// macOS-style window dots and a green `>_` prompt) and write it as PNG. The
/// bundle script renders this at 1024² and feeds it to `sips`/`iconutil`.
pub fn render_icon(out_path: &str) -> std::io::Result<()> {
    use corelib::gfx::Canvas;
    use corelib::types::{Rect, Rgba8};

    let size = 1024u32;
    let sf = size as f32;
    let mut s = corelib::gfx::Surface::new(size, size);
    s.clear(Rgba8::TRANSPARENT);

    // The rounded tile (macOS Big-Sur-ish margins + corner radius), with a thin
    // accent ring for depth (accent tile, then the dark tile inset over it).
    let margin = sf * 0.098;
    let tile = sf - 2.0 * margin;
    let radius = tile * 0.225;
    s.fill_rounded_rect(Rect::new(margin, margin, tile, tile), radius, Rgba8::hex(0x2A2E37));
    let ring = sf * 0.008;
    s.fill_rounded_rect(
        Rect::new(margin + ring, margin + ring, tile - 2.0 * ring, tile - 2.0 * ring),
        radius - ring,
        Rgba8::hex(0x14161B),
    );

    // macOS-style traffic-light dots, top-left of the tile.
    let dot = tile * 0.058;
    let dy = margin + tile * 0.135;
    let dx0 = margin + tile * 0.125;
    let step = dot * 1.6;
    for (i, c) in [0xFF5F57u32, 0xFEBC2E, 0x28C840].iter().enumerate() {
        s.fill_rounded_rect(Rect::new(dx0 + i as f32 * step, dy, dot, dot), dot * 0.5, Rgba8::hex(*c));
    }

    // The brand mark, centered: a terminal prompt chevron + "@ai" — "a terminal with
    // native AI". The chevron is terminal-green; "@ai" is sky-accent so the AI half reads
    // distinctly. Both parts are measured and centered together so the lockup stays balanced.
    use corelib::gfx::text::{draw_text, measure_text};
    let mut cache = corelib::gfx::text::GlyphCache::new(platform::os::text_shaper());
    let px = tile * 0.30;
    let m = cache.metrics(px);
    let chevron = "\u{276F}"; // ❯
    let ai = "@ai";
    let gap = px * 0.30;
    let total = measure_text(&mut cache, chevron, px) + gap + measure_text(&mut cache, ai, px);
    let x0 = (sf - total) * 0.5;
    let baseline = sf * 0.52 + m.ascent * 0.5;
    let nx = draw_text(&mut s, &mut cache, chevron, px, x0, baseline, Rgba8::hex(0x4ADE80), sf, true);
    draw_text(&mut s, &mut cache, ai, px, nx + gap, baseline, Rgba8::hex(0x38BDF8), sf, true);

    corelib::gfx::png::write_surface(out_path, &s)?;
    println!("rendered app icon {size}\u{00d7}{size} \u{2192} {out_path}");
    Ok(())
}

/// `--render-chrome <pos>` — render the window chrome (status bar + a multi-tab bar
/// with sample tabs) in one of the four tab-bar orientations (`top|bottom|left|right`),
/// with a sample pane behind it, so the tab design is verifiable headlessly with the
/// real font shaper. Used to compare tab-bar designs across orientations.
pub fn render_chrome(pos: &str, theme_name: Option<&str>, out_path: &str) -> std::io::Result<()> {
    use crate::gui::render::{render_tab_bar_side, render_tab_bar_top, tab_bar_height, TabInfo, SIDE_TAB_W};
    use crate::plugin::{Segment, StatusLine};
    use corelib::gfx::text::draw_text;
    use corelib::gfx::Canvas;
    use corelib::types::Rect;

    let theme = match theme_name {
        Some(n) => crate::config::Config::resolve_theme(n),
        None => corelib::theme::midnight(),
    };
    let mut cache = corelib::gfx::text::GlyphCache::new(platform::os::text_shaper());
    let px = 15.0;
    let (w, h) = (1200u32, 720u32);
    let mut surface = corelib::gfx::Surface::new(w, h);
    surface.clear(theme.bg);

    let tabs_src = [
        ("\u{1F5A5}", "Terminal [zsh]"), ("\u{1F5A5}", "vim main.rs"), ("\u{1F3E0}", "Home"),
        ("\u{1F5A5}", "cargo build"), ("\u{1F5A5}", "Terminal [bash]"),
        ("\u{1F310}", "docs"), ("\u{1F5A5}", "htop"), ("\u{1F5A5}", "git log"), ("\u{1F5A5}", "ssh prod"),
        ("\u{1F4C1}", "Files"), ("\u{1F5A5}", "Terminal [zsh]"), ("\u{1F310}", "less README"), ("\u{1F5A5}", "build"),
    ];
    let active = 11usize; // a late tab, to exercise scroll-to-active in the strip
    let tabs: Vec<TabInfo> = tabs_src
        .iter()
        .enumerate()
        .map(|(i, (ic, n))| TabInfo { index: i + 1, icon: (*ic).to_string(), title: (*n).to_string(), active: i == active })
        .collect();

    let seg = |text: &str, fg: &str| Segment { text: text.into(), fg: Some(fg.into()), bg: None };
    let line = StatusLine {
        left: vec![seg("\u{2387} main \u{25CF}", "success"), seg("\u{1F4C1} ~/project", "accent")],
        right: vec![seg("user@mac", "muted"), seg("14:32", "muted")],
    };
    let status_h = status_bar_height(&cache.metrics(px));
    render_status_bar(&mut surface, &line, &theme, &mut cache, px, w, h as f32 - status_h);
    let m = cache.metrics(px);

    // Fill the pane area with the terminal background + a faux prompt, so the tab bar's
    // `surface` colour reads against real pane content (as it does live).
    let draw_pane = |surface: &mut corelib::gfx::Surface, cache: &mut corelib::gfx::text::GlyphCache, area: Rect| {
        surface.fill_rect(area, theme.term_bg);
        let by = area.y + PAD + cache.metrics(px).ascent;
        draw_text(surface, cache, "\u{276F} echo hello", px, area.x + PAD, by, theme.term_fg, area.right() - PAD, false);
    };

    // Panes fill from the top down to the status bar (which is along the bottom edge).
    let mut area = Rect::new(0.0, 0.0, w as f32, h as f32 - status_h);
    let side_h = h as f32 - status_h;
    match pos {
        "bottom" => {
            // Tab strip just above the status bar.
            let tab_h = tab_bar_height(&m);
            area.h -= tab_h;
            draw_pane(&mut surface, &mut cache, area);
            render_tab_bar_top(&mut surface, &tabs, &theme, &mut cache, px, w, h as f32 - status_h - tab_h, true, None);
        }
        "left" => {
            area.x += SIDE_TAB_W;
            area.w -= SIDE_TAB_W;
            draw_pane(&mut surface, &mut cache, area);
            render_tab_bar_side(&mut surface, &tabs, &theme, &mut cache, px, 0.0, 0.0, side_h, true, None);
        }
        "right" => {
            area.w -= SIDE_TAB_W;
            draw_pane(&mut surface, &mut cache, area);
            render_tab_bar_side(&mut surface, &tabs, &theme, &mut cache, px, w as f32 - SIDE_TAB_W, 0.0, side_h, false, None);
        }
        _ => {
            let tab_h = tab_bar_height(&m);
            area.y += tab_h;
            area.h -= tab_h;
            draw_pane(&mut surface, &mut cache, area);
            render_tab_bar_top(&mut surface, &tabs, &theme, &mut cache, px, w, 0.0, false, None);
        }
    }

    write_ppm(out_path, surface.pixels(), w, h)?;
    println!("rendered chrome [{pos}] \u{2192} {w}\u{00d7}{h}px \u{2192} {out_path}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_ppm_emits_a_valid_p6_frame() {
        let path = std::env::temp_dir().join(format!("tt-ppm-{}.ppm", std::process::id()));
        // 2×1: a red and a blue pixel (0xRRGGBB in the u32 layout the renderer uses).
        write_ppm(&path.to_string_lossy(), &[0x00FF0000, 0x000000FF], 2, 1).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"P6\n2 1\n255\n"), "PPM header");
        assert_eq!(&bytes[bytes.len() - 6..], &[0xFF, 0, 0, 0, 0, 0xFF], "RGB triplets in order");
        let _ = std::fs::remove_file(&path);
    }
}
