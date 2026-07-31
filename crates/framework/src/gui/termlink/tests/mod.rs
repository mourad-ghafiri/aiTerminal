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
