//! Terminal link detection — the **pure** logic behind ⌘-click in the PTY pane: pull the
//! token under the cursor, classify it (URL / filesystem path), and decide what to open.
//! The host (`gui/link.rs`) supplies the live cwd/home and a real [`FsProbe`], then hands
//! the resulting [`OpenAction`] to the OS opener (`platform::os::open_external`); keeping
//! the decision here makes it a small, deterministic, fully unit-tested unit (no I/O).

use std::path::{Path, PathBuf};

/// A token classified by its shape, before the filesystem is consulted.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkKind {
    /// `http://…` / `https://…` — handed to the OS (system browser).
    Url(String),
    /// A resolved (absolute) filesystem path — disambiguated by [`route`].
    Path(PathBuf),
}

/// What the host should do, decided from a [`LinkKind`] + a filesystem probe.
/// Both variants open through the OS (`open_external`): a URL in the system
/// browser, a path with its default app (a folder opens in the file manager).
#[derive(Debug, Clone, PartialEq)]
pub enum OpenAction {
    /// Open an http(s) URL in the system browser.
    Url(String),
    /// Open an existing file/folder with its OS default handler.
    Path(PathBuf),
}

/// The filesystem questions [`route`] needs — abstracted so routing stays pure + testable.
pub trait FsProbe {
    fn exists(&self, p: &Path) -> bool;
}

/// The whitespace-delimited token under `col` in `row` as `(chars, start, end)` — the trimmed
/// char-range `[start, end)` (wrapping quotes/brackets + trailing sentence punctuation removed,
/// so `(https://x).` → `https://x`) plus the row's chars (so the caller maps columns and pulls
/// `chars[start..end]`). `None` when the column sits on whitespace or the token is empty.
pub fn token_span(row: &str, col: usize) -> Option<(Vec<char>, usize, usize)> {
    let chars: Vec<char> = row.chars().collect();
    if col >= chars.len() || chars[col].is_whitespace() {
        return None;
    }
    // The maximal non-whitespace run containing `col`.
    let mut s = col;
    while s > 0 && !chars[s - 1].is_whitespace() {
        s -= 1;
    }
    let mut e = col + 1;
    while e < chars.len() && !chars[e].is_whitespace() {
        e += 1;
    }
    // Trim wrapping brackets/quotes from both ends, then trailing sentence punctuation.
    let wrap = |c: char| "()[]{}<>\"'`".contains(c);
    let tail = |c: char| ".,;:!?".contains(c);
    while s < e && wrap(chars[s]) {
        s += 1;
    }
    while e > s && (wrap(chars[e - 1]) || tail(chars[e - 1])) {
        e -= 1;
    }
    (e > s).then_some((chars, s, e))
}

/// Whether char `i` ends a path candidate: a tab, any non-space whitespace, a wrapping
/// bracket/quote, or a space that is part of a **run of ≥2** (a column gap, e.g. `ls`
/// padding). A *single* space is kept, so a path like `My Folder` / `مجلد عربي` survives.
fn is_boundary(chars: &[char], i: usize) -> bool {
    let c = chars[i];
    if c == '\t' || "()[]{}<>\"'`".contains(c) {
        return true;
    }
    if c.is_whitespace() {
        return c != ' ' || (i > 0 && chars[i - 1] == ' ') || (i + 1 < chars.len() && chars[i + 1] == ' ');
    }
    false
}

/// Trim wrapping brackets/quotes (both ends) + trailing sentence punctuation from a span.
fn trim_span(chars: &[char], mut a: usize, mut b: usize) -> (usize, usize) {
    let wrap = |c: char| "()[]{}<>\"'`".contains(c);
    let tail = |c: char| ".,;:!?".contains(c);
    while a < b && wrap(chars[a]) {
        a += 1;
    }
    while b > a && (wrap(chars[b - 1]) || tail(chars[b - 1])) {
        b -= 1;
    }
    (a, b)
}

/// Resolve the link under char-column `col` in `text` to its trimmed char span
/// `[start, end)` + the [`OpenAction`] it opens — the **filesystem-aware** entry point.
///
/// URLs use the whitespace-delimited token (they never contain spaces). For a **path**,
/// the filesystem disambiguates word boundaries: among the word-aligned spans around
/// `col` (joined by single spaces, so names with spaces — in any language, e.g. Arabic —
/// stay whole), it picks the **longest one that actually exists**. Bounded (a budget caps
/// `exists` probes), so it's cheap on every ⌘-hover.
pub fn link_span(text: &str, col: usize, cwd: Option<&Path>, home: Option<&Path>, fs: &dyn FsProbe) -> Option<(usize, usize, OpenAction)> {
    let chars: Vec<char> = text.chars().collect();
    if col >= chars.len() {
        return None;
    }
    // 1. URL — the plain whitespace token (no spaces to span).
    if !chars[col].is_whitespace() {
        if let Some((_, s, e)) = token_span(text, col) {
            let tok: String = chars[s..e].iter().collect();
            if let Some(kind @ LinkKind::Url(_)) = classify(&tok, cwd, home) {
                if let Some(act) = route(kind, fs) {
                    return Some((s, e, act));
                }
            }
        }
    }
    // 2. Path — the longest existing word-aligned span that contains `col`.
    if is_boundary(&chars, col) {
        return None;
    }
    // The candidate region: the run around `col` bounded by column gaps / brackets, with
    // single internal spaces kept.
    let mut r0 = col;
    while r0 > 0 && !is_boundary(&chars, r0 - 1) {
        r0 -= 1;
    }
    let mut r1 = col + 1;
    while r1 < chars.len() && !is_boundary(&chars, r1) {
        r1 += 1;
    }
    // Word boundaries within the region (paths align to whole space-separated words).
    let starts: Vec<usize> = (r0..r1).filter(|&i| chars[i] != ' ' && (i == r0 || chars[i - 1] == ' ')).collect();
    let ends: Vec<usize> = (r0 + 1..=r1).filter(|&i| chars[i - 1] != ' ' && (i == r1 || chars[i] == ' ')).collect();
    // Spans containing `col`, longest first; the first that exists wins.
    let mut cands: Vec<(usize, usize)> = Vec::new();
    for &a in &starts {
        if a > col {
            break;
        }
        for &b in &ends {
            if b > col && b > a {
                cands.push((a, b));
            }
        }
    }
    cands.sort_by_key(|&(a, b)| std::cmp::Reverse(b - a));
    let mut budget = 48u32; // cap `exists` probes so a ⌘-hover is cheap
    for (a, b) in cands {
        if budget == 0 {
            break;
        }
        let (ta, tb) = trim_span(&chars, a, b);
        if tb <= ta || ta > col || tb <= col {
            continue;
        }
        budget -= 1;
        let s: String = chars[ta..tb].iter().collect();
        if let Some(p) = resolve_path(s.trim(), cwd, home) {
            if fs.exists(&p) {
                return route(LinkKind::Path(p), fs).map(|act| (ta, tb, act));
            }
        }
    }
    None
}

/// Classify a token. `cwd`/`home` resolve relative + `~` paths. Liberal: any non-URL token
/// becomes a `Path` candidate (whether it exists is [`route`]'s job), so a bare filename in
/// `ls` output is clickable; truly unresolvable tokens (relative with no cwd) yield `None`.
pub fn classify(token: &str, cwd: Option<&Path>, home: Option<&Path>) -> Option<LinkKind> {
    let t = token.trim();
    if t.is_empty() {
        return None;
    }
    if t.starts_with("http://") || t.starts_with("https://") {
        return Some(LinkKind::Url(t.to_string()));
    }
    resolve_path(t, cwd, home).map(LinkKind::Path)
}

/// Resolve a path token to an absolute `PathBuf`: `~`/`~/…` against `home`, an absolute path
/// as-is, a relative path against `cwd`. `None` when it can't be made absolute.
fn resolve_path(t: &str, cwd: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    if t == "~" {
        return home.map(PathBuf::from);
    }
    if let Some(rest) = t.strip_prefix("~/") {
        return home.map(|h| h.join(rest));
    }
    let p = Path::new(t);
    if p.is_absolute() {
        return Some(p.to_path_buf());
    }
    cwd.map(|c| c.join(t))
}

/// Decide what to open from a [`LinkKind`] + filesystem probe. `None` when a path doesn't
/// exist (so a non-path word silently does nothing).
pub fn route(kind: LinkKind, fs: &dyn FsProbe) -> Option<OpenAction> {
    match kind {
        LinkKind::Url(u) => Some(OpenAction::Url(u)),
        LinkKind::Path(p) => fs.exists(&p).then_some(OpenAction::Path(p)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_at(row: &str, col: usize) -> Option<String> {
        token_span(row, col).map(|(chars, s, e)| chars[s..e].iter().collect())
    }

    #[test]
    fn token_span_extracts_and_trims() {
        // mid-token click → the whole whitespace-delimited token.
        assert_eq!(token_at("run https://example.com/x now", 8).as_deref(), Some("https://example.com/x"));
        // wrapping punctuation + trailing sentence stop are trimmed.
        assert_eq!(token_at("see (https://a.b/c).", 7).as_deref(), Some("https://a.b/c"));
        assert_eq!(token_at("open 'src/main.rs',", 8).as_deref(), Some("src/main.rs"));
        // query strings survive (whitespace-delimited, not word-char-delimited).
        assert_eq!(token_at("go https://a.b/p?x=1&y=2 ok", 5).as_deref(), Some("https://a.b/p?x=1&y=2"));
        // whitespace under the cursor → nothing.
        assert_eq!(token_at("a  b", 1), None);
    }

    #[test]
    fn classify_recognizes_schemes_and_paths() {
        let cwd = PathBuf::from("/work/proj");
        let home = PathBuf::from("/Users/me");
        assert_eq!(classify("https://x.io", None, None), Some(LinkKind::Url("https://x.io".into())));
        assert_eq!(classify("/etc/hosts", None, None), Some(LinkKind::Path("/etc/hosts".into())));
        assert_eq!(classify("~/notes.md", None, Some(&home)), Some(LinkKind::Path("/Users/me/notes.md".into())));
        assert_eq!(classify("src/main.rs", Some(&cwd), None), Some(LinkKind::Path("/work/proj/src/main.rs".into())));
        // relative with no cwd → unresolvable.
        assert_eq!(classify("src/main.rs", None, None), None);
    }

    struct Mock {
        paths: Vec<PathBuf>,
    }
    impl FsProbe for Mock {
        fn exists(&self, p: &Path) -> bool {
            self.paths.contains(&p.to_path_buf())
        }
    }

    /// `link_span` over a row of chars → the matched substring + action.
    fn span_at(row: &str, col: usize, cwd: &str, fs: &Mock) -> Option<(String, OpenAction)> {
        let cwd = PathBuf::from(cwd);
        let chars: Vec<char> = row.chars().collect();
        link_span(row, col, Some(&cwd), None, fs).map(|(s, e, act)| (chars[s..e].iter().collect(), act))
    }

    #[test]
    fn link_span_spans_paths_with_spaces() {
        let fs = Mock { paths: vec!["/work/My Folder".into(), "/work/My Report.pdf".into()] };
        // "ls:  My Folder" — clicking any part of the spaced name resolves the whole path.
        let row = "ls:  My Folder";
        let my = row.chars().position(|c| c == 'M').unwrap();
        let (tok, act) = span_at(row, my, "/work", &fs).expect("click 'My'");
        assert_eq!(tok, "My Folder");
        assert_eq!(act, OpenAction::Path("/work/My Folder".into()));
        // Clicking 'Folder', or the single space between the words, resolves the same span.
        assert_eq!(span_at(row, my + 3, "/work", &fs).unwrap().0, "My Folder", "click 'Folder'");
        assert_eq!(span_at(row, my + 2, "/work", &fs).unwrap().0, "My Folder", "click the space");
        // A spaced file opens through the OS too.
        let (tok, act) = span_at("see My Report.pdf", 4, "/work", &fs).unwrap();
        assert_eq!(tok, "My Report.pdf");
        assert_eq!(act, OpenAction::Path("/work/My Report.pdf".into()));
    }

    #[test]
    fn link_span_handles_non_ascii_arabic_names() {
        let fs = Mock { paths: vec!["/work/مجلد".into(), "/work/مجلد عربي".into()] };
        // A single-token Arabic folder.
        let (tok, act) = span_at("مجلد", 1, "/work", &fs).unwrap();
        assert_eq!(tok, "مجلد");
        assert_eq!(act, OpenAction::Path("/work/مجلد".into()));
        // An Arabic name WITH a space resolves the whole multi-word path.
        let row = "مجلد عربي";
        let col = row.chars().position(|c| c == 'ع').unwrap();
        assert_eq!(span_at(row, col, "/work", &fs).unwrap().0, "مجلد عربي");
    }

    #[test]
    fn link_span_picks_the_existing_span_not_the_metadata_prefix() {
        // `ls -l`-style line: the single spaces would naively glue the time onto the name,
        // but the filesystem disambiguates — only "My Folder" exists.
        let fs = Mock { paths: vec!["/w/My Folder".into()] };
        let row = "drwxr-xr-x 1 me 10:00 My Folder";
        let chars: Vec<char> = row.chars().collect();
        // Click the 'M' of "My Folder" (the last 'M' on the line) → resolves only "My Folder".
        let m = chars.iter().rposition(|&c| c == 'M').unwrap();
        assert_eq!(span_at(row, m, "/w", &fs).unwrap().0, "My Folder", "the time prefix is excluded");
        // Clicking the timestamp "10:00" (no existing path) resolves nothing.
        let ten = chars.windows(5).position(|w| w == ['1', '0', ':', '0', '0']).unwrap();
        assert!(span_at(row, ten, "/w", &fs).is_none(), "clicking the timestamp resolves nothing");
    }

    #[test]
    fn link_span_url_and_nonexistent() {
        let fs = Mock { paths: vec![] };
        // URLs still work (no spaces).
        let (tok, act) = span_at("open https://example.com/x now", 8, "/w", &fs).unwrap();
        assert_eq!(tok, "https://example.com/x");
        assert_eq!(act, OpenAction::Url("https://example.com/x".into()));
        // Plain prose with no existing path → no link (no false underline).
        assert!(span_at("just some words here", 6, "/w", &fs).is_none());
    }

    #[test]
    fn route_opens_urls_and_existing_paths_only() {
        let fs = Mock { paths: vec!["/p".into(), "/p/a.mp4".into(), "/p/readme.txt".into()] };
        assert_eq!(route(LinkKind::Url("https://x".into()), &fs), Some(OpenAction::Url("https://x".into())));
        // Existing folder / file → the OS opener; a non-existent path → nothing.
        assert_eq!(route(LinkKind::Path("/p".into()), &fs), Some(OpenAction::Path("/p".into())));
        assert_eq!(route(LinkKind::Path("/p/readme.txt".into()), &fs), Some(OpenAction::Path("/p/readme.txt".into())));
        assert_eq!(route(LinkKind::Path("/nope".into()), &fs), None);
    }
}
