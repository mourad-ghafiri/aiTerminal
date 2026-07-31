use std::path::{Path, PathBuf};
use crate::cli::style::{md_width, muted, reset};

/// The fenced-block language the AI uses for diagrams (kept internal — never shown to users).
pub(crate) const DIAGRAM_LANG: &str = "mermaid";

/// True when `pend` begins a diagram fence that hasn't been closed yet.
pub(crate) fn is_open_diagram_fence(pend: &str) -> bool {
    let mut lines = pend.lines();
    let Some(first) = lines.next().map(str::trim) else { return false };
    let is_fence = first.starts_with("```") || first.starts_with("~~~");
    let lang = first.trim_start_matches(['`', '~']).trim().to_ascii_lowercase();
    if !is_fence || lang != DIAGRAM_LANG {
        return false;
    }
    !lines.any(|l| {
        let t = l.trim_start();
        t.starts_with("```") || t.starts_with("~~~")
    })
}

/// Are we inside our OWN GUI terminal (which draws native diagrams via `OSC 1338`)? The PTY
/// exports `TERM_PROGRAM = <brand>` to its children.
pub(crate) fn is_native_terminal() -> bool {
    std::env::var("TERM_PROGRAM").ok().as_deref() == Some(corelib::brand::NAME)
}

/// Grid rows a diagram needs, from its pure layout height (nominal 8×16 cell). Clamped to a
/// sane band. Shared by the inline `OSC 1338` emitter and the `@md edit` preview layout so a
/// diagram reserves the same height everywhere.
pub(crate) fn diagram_rows(source: &str) -> usize {
    corelib::mermaid::parse(source)
        .map(|d| {
            let l = corelib::mermaid::layout(&d, &|s: &str| (corelib::unicode::str_width(s) as u32 * 8, 16));
            l.height.div_ceil(18).clamp(3, 120) as usize
        })
        .unwrap_or(3)
}

/// Turn a diagram's source into terminal output: a native `OSC 1338` placement (with a
/// reserved row count from the pure layout) when our GUI can draw it, else a clean boxed
/// fallback (other terminals / pipes). No jargon is ever shown to the user.
pub(crate) fn diagram_output(source: &str) -> String {
    if is_native_terminal() && corelib::mermaid::parse(source).is_some() {
        let rows = diagram_rows(source);
        return format!("\x1b]1338;{rows};{}\x07", corelib::codec::base64_encode(source.as_bytes()));
    }
    diagram_text(source)
}

/// A diagram for terminals that can't draw pixels: the real picture in Unicode box art,
/// or — only when it can't be read or won't fit the width — the source in a box. The user
/// never has to look at diagram syntax if we can avoid it.
pub(crate) fn diagram_text(source: &str) -> String {
    let width = md_width();
    match corelib::mermaid::art(source, width) {
        Some(rows) if !rows.is_empty() => {
            let mut out = String::new();
            for r in rows {
                out.push_str(&r);
                out.push('\n');
            }
            out
        }
        _ => diagram_fallback_box(source),
    }
}

/// An image for a terminal that can draw pixels: an `OSC 1339` placement over reserved
/// rows. Anywhere else — or for an image we can't get hold of — the caller's `fallback`
/// (the ordinary `▣ alt` placeholder) is what shows.
///
/// `base` is the document's own directory, so a README's `img/logo.png` resolves the way
/// the document meant it.
pub(crate) fn image_output(src: &str, fallback: &str, base: &Path) -> String {
    if !is_native_terminal() {
        return fallback.to_string();
    }
    match image_placement(src, base) {
        Some((path, rows)) => format!("\x1b]1339;{rows};{}\x07", corelib::codec::base64_encode(path.as_bytes())),
        None => fallback.to_string(),
    }
}

/// Resolve an image source to a local file and the grid rows it should occupy — what a
/// host reserves before asking the app to draw it.
pub(crate) fn image_placement(src: &str, base: &Path) -> Option<(String, usize)> {
    let cfg = crate::config::Config::load();
    let path = resolve_image(src, base, &cfg)?;
    let bytes = std::fs::read(&path).ok()?;
    let img = platform::os::image_decoder().decode(&bytes)?;
    if img.width == 0 || img.height == 0 {
        return None;
    }
    // A grid cell is about twice as tall as it is wide, so a square image needs about
    // half as many rows as it does columns.
    let cols = md_width() as f32;
    let rows = (img.height as f32 / img.width as f32 * cols * 0.5).round() as usize;
    Some((path.to_string_lossy().into_owned(), rows.clamp(2, cfg.md_image_max_rows)))
}

/// A local path for `src`: a file beside the document, or — only when `[md]
/// remote_images` says so — a cached download.
fn resolve_image(src: &str, base: &Path, cfg: &crate::config::Config) -> Option<PathBuf> {
    let src = src.trim();
    if src.is_empty() || src.starts_with("data:") {
        return None;
    }
    if src.starts_with("http://") || src.starts_with("https://") {
        return cfg.md_remote_images.then(|| cached_download(src)).flatten();
    }
    let path = match src.strip_prefix("file://") {
        Some(p) => PathBuf::from(p),
        None => {
            let p = PathBuf::from(src);
            if p.is_absolute() {
                p
            } else {
                base.join(p)
            }
        }
    };
    path.is_file().then_some(path)
}

/// Fetch a remote image once and keep it under `~/.<brand>/cache/images/`.
fn cached_download(url: &str) -> Option<PathBuf> {
    let dir = crate::config::Config::dir().join("cache").join("images");
    let name = format!("{:016x}{}", url_hash(url), image_ext(url));
    let path = dir.join(name);
    if path.is_file() {
        return Some(path);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let bytes = platform::transport::fetch(url).ok()?;
    // Something that isn't an image is not worth keeping, and not worth drawing.
    if bytes.is_empty() || platform::os::image_decoder().decode(&bytes).is_none() {
        return None;
    }
    std::fs::write(&path, &bytes).ok()?;
    Some(path)
}

/// A stable file name for a URL (FNV-1a — a cache key, not a security decision).
fn url_hash(url: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The extension a URL implies, so the cached file is recognizable on disk.
fn image_ext(url: &str) -> String {
    let tail = url.rsplit('/').next().unwrap_or("");
    let ext = tail.rsplit_once('.').map(|(_, e)| e.split(['?', '#']).next().unwrap_or("")).unwrap_or("");
    if (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        format!(".{}", ext.to_ascii_lowercase())
    } else {
        String::new()
    }
}

/// A diagram's drawn rows for a preview `width` columns wide, without styling — the art
/// when it can be drawn, else the boxed source. Shared by the `@md` pager and editor so a
/// diagram occupies exactly the rows it paints.
pub(crate) fn diagram_lines(source: &str, width: usize) -> Vec<String> {
    if let Some(rows) = corelib::mermaid::art(source, width) {
        return rows;
    }
    let w = source.lines().map(corelib::unicode::str_width).max().unwrap_or(0).clamp(7, width.saturating_sub(2).max(7));
    let mut out = vec![format!("╭─ diagram {}╮", "─".repeat(w.saturating_sub(9)))];
    for line in source.lines() {
        let pad = w.saturating_sub(corelib::unicode::str_width(line));
        out.push(format!("│ {line}{} │", " ".repeat(pad)));
    }
    out.push(format!("╰{}╯", "─".repeat(w + 2)));
    out
}


/// A plain boxed rendering of a diagram's source for terminals that can't draw it.
fn diagram_fallback_box(source: &str) -> String {
    let width = source.lines().map(corelib::unicode::str_width).max().unwrap_or(0).clamp(7, 78);
    let (dim, r) = (muted(), reset());
    let mut out = format!("{dim}╭─ diagram {}╮{r}\n", "─".repeat(width.saturating_sub(9)));
    for line in source.lines() {
        let pad = width.saturating_sub(corelib::unicode::str_width(line));
        out.push_str(&format!("{dim}│{r} {line}{} {dim}│{r}\n", " ".repeat(pad)));
    }
    out.push_str(&format!("{dim}╰{}╯{r}\n", "─".repeat(width + 2)));
    out
}
