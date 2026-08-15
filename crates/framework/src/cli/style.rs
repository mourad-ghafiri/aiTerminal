// ── the live harness display (Claude-Code-style chrome, all on stderr) ───────
//
// stdout stays pure content (the answer / the one marker line); stderr carries
// the experience: a spinner while waiting, dim streamed thinking with a `∴`
// marker, a timed `⚙` tool trace, and a `✓ elapsed · tools · tokens` footer.
// Everything is TTY-aware: piped/background runs get plain, animation-free
// output automatically.

/// Whether stderr is an interactive terminal (spinner + colors allowed).
pub(crate) fn err_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

/// A truecolor escape from a `TT_*_RGB` env var (exported by the shell
/// integration's colors file), so CLI chrome matches the ACTIVE theme; falls
/// back to a plain ANSI code when unset or not a TTY.
fn theme_color(var: &str, ansi_fallback: &str) -> String {
    if !err_is_tty() {
        return String::new();
    }
    match std::env::var(var) {
        Ok(rgb) if rgb.split(';').count() == 3 => format!("\x1b[38;2;{rgb}m"),
        _ => ansi_fallback.to_string(),
    }
}

pub(crate) fn accent() -> String {
    theme_color("TT_ACCENT_RGB", "\x1b[36m")
}
pub(crate) fn muted() -> String {
    theme_color("TT_MUTED_RGB", "\x1b[2m")
}
/// The theme's three semantic hues. `md_style` already reads them for a document's
/// callouts; chrome that reports an OUTCOME — a finished node, a failed one, a run
/// parked for a person — has the same claim on them, and drawing all of it in the one
/// accent throws away a distinction the theme already makes.
pub(crate) fn success() -> String {
    theme_color("TT_SUCCESS_RGB", "\x1b[32m")
}
pub(crate) fn warn() -> String {
    theme_color("TT_WARN_RGB", "\x1b[33m")
}
pub(crate) fn danger() -> String {
    theme_color("TT_ERROR_RGB", "\x1b[31m")
}
/// Emphasis, gated exactly like the colours — an ungated `\x1b[1m` is a stray escape
/// in every redirected line.
pub(crate) fn bold() -> &'static str {
    if err_is_tty() {
        "\x1b[1m"
    } else {
        ""
    }
}
pub(crate) fn reset() -> &'static str {
    // Gated exactly like the colours it closes: with `accent`/`muted` empty off a
    // terminal, an ungated reset is a stray escape in every redirected line.
    if err_is_tty() {
        "\x1b[0m"
    } else {
        ""
    }
}

/// Whether stdout is a terminal (agents/flows/loops stream the answer to stdout, so its
/// Markdown rendering + TTY-gating is keyed on this stream, not stderr).
pub(crate) fn out_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// The Markdown render palette, from the active theme's env colors (with sensible defaults).
pub(crate) fn md_style() -> corelib::md::Style {
    let rgb = |var: &str, default: corelib::types::Rgba8| -> corelib::types::Rgba8 {
        std::env::var(var)
            .ok()
            .and_then(|s| {
                let p: Vec<u8> = s.split(';').filter_map(|x| x.trim().parse().ok()).collect();
                (p.len() == 3).then(|| corelib::types::Rgba8::rgb(p[0], p[1], p[2]))
            })
            .unwrap_or(default)
    };
    let d = corelib::md::Style::default();
    let accent = rgb("TT_ACCENT_RGB", d.accent);
    let muted = rgb("TT_MUTED_RGB", d.muted);
    // The alert hues come from the theme's own semantic tokens, so a callout is the same
    // green/amber/red the rest of the UI uses.
    corelib::md::Style {
        enabled: true,
        heading: accent,
        accent,
        code: d.code,
        muted,
        link: accent,
        success: rgb("TT_SUCCESS_RGB", d.success),
        warn: rgb("TT_WARN_RGB", d.warn),
        error: rgb("TT_ERROR_RGB", d.error),
    }
}

/// Wrap width for rendered Markdown — the split's REAL width (via `TIOCGWINSZ`, since the shell
/// doesn't export `$COLUMNS` to us), minus a small right margin. Falls back to `$COLUMNS`, then
/// 80. No low cap: wide splits are used fully (a generous 400 ceiling just guards absurd sizes).
pub(crate) fn md_width() -> usize {
    term_cols().saturating_sub(2).clamp(24, 400)
}

/// The terminal's width in columns — `TIOCGWINSZ`, then `$COLUMNS`, then 80.
///
/// ONE definition, because anything that repaints in place has to agree with the
/// terminal about where a line ends: a row wider than the window wraps to two VISUAL
/// rows, and a cursor-up count measured in logical lines then climbs too few of them.
pub(crate) fn term_cols() -> usize {
    platform::os::terminal_size()
        .map(|(c, _)| c as usize)
        .or_else(|| std::env::var("COLUMNS").ok().and_then(|c| c.trim().parse::<usize>().ok()))
        .unwrap_or(80)
}

/// The split's height in rows (for the live renderer's overflow guard, and for the
/// flow board deciding whether its cards will fit); 0 if unknown.
pub(crate) fn term_rows() -> usize {
    platform::os::terminal_size().map(|(_, r)| r as usize).unwrap_or(0)
}

/// Markdown render options when writing to a TTY; `None` (raw text) when piped.
/// How markdown renders on a surface: the palette, the wrap width, and whether
/// the surface composites native placements (OSC 1338/1339) or needs box art.
/// The CALLER states its surface — the env sniff below serves only the default
/// CLI case (a child inside one of our panes).
#[derive(Clone)]
pub(crate) struct MdOptions {
    pub style: corelib::md::Style,
    pub width: usize,
    pub native: bool,
}

pub(crate) fn markdown_opts(is_tty: bool) -> Option<MdOptions> {
    is_tty.then(|| MdOptions { style: md_style(), width: md_width(), native: crate::cli::media::is_native_terminal() })
}
