use super::input::{Edit, LineBuffer};
use super::trust::{establish, reset, Trust};
use crate::mdedit::key::Key;

// ── the line editor's pure rules ────────────────────────────────────────────

fn typed(buf: &mut LineBuffer, text: &str) {
    for c in text.chars() {
        buf.apply(&Key::Char(c));
    }
}

#[test]
fn the_line_buffer_edits_the_way_a_shell_line_does() {
    let mut b = LineBuffer::default();
    typed(&mut b, "hello wrld");
    // Cursor back over "rld", insert the missing o.
    for _ in 0..3 {
        b.apply(&Key::Left);
    }
    b.apply(&Key::Char('o'));
    assert_eq!(b.text(), "hello world");

    b.apply(&Key::Home);
    b.apply(&Key::Delete);
    assert_eq!(b.text(), "ello world");
    b.apply(&Key::End);
    b.apply(&Key::Backspace);
    assert_eq!(b.text(), "ello worl");

    // Ctrl+W kills the word before the cursor, spaces first.
    b.apply(&Key::Ctrl('w'));
    assert_eq!(b.text(), "ello ");
    // Ctrl+U kills to the start.
    b.apply(&Key::Ctrl('u'));
    assert_eq!(b.text(), "");

    assert_eq!(b.apply(&Key::Enter), Edit::Accept);
    assert_eq!(b.apply(&Key::Ctrl('c')), Edit::Cancel);
    assert_eq!(b.apply(&Key::Ctrl('d')), Edit::End, "Ctrl+D on an EMPTY line ends input");
    typed(&mut b, "x");
    assert_eq!(b.apply(&Key::Ctrl('d')), Edit::Ignored, "…and does nothing on a non-empty one");
}

#[test]
fn tab_completes_the_typed_vocabularies_and_nothing_else() {
    let all: Vec<String> = ["/help", "/redo", "/resume", "@flow", "@coder"].iter().map(|s| s.to_string()).collect();
    let mut b = LineBuffer::default();
    typed(&mut b, "/hel");
    assert!(b.complete(&all));
    assert_eq!(b.text(), "/help ");

    // Ambiguity grows only to the common prefix — "/redo" and "/resume" share
    // nothing past "/re", so nothing is added and NEITHER is picked arbitrarily.
    let mut b = LineBuffer::default();
    typed(&mut b, "/re");
    assert!(!b.complete(&all));
    assert_eq!(b.text(), "/re");
    // With one letter more the tie is broken and completion finishes the word.
    b.apply(&Key::Char('d'));
    assert!(b.complete(&all));
    assert_eq!(b.text(), "/redo ");

    let mut b = LineBuffer::default();
    typed(&mut b, "@fl");
    assert!(b.complete(&all));
    assert_eq!(b.text(), "@flow ");

    // Plain words never complete — the vocabulary is / and @ only.
    let mut b = LineBuffer::default();
    typed(&mut b, "hel");
    assert!(!b.complete(&all));
}

// ── the trust gate ──────────────────────────────────────────────────────────

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tt-ws-trust-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn trust_is_asked_once_remembered_and_reopened_only_by_what_executes() {
    let root = scratch("gate-root");
    let session = scratch("gate-session");
    std::fs::create_dir_all(root.join(".aiTerminal/mcp")).unwrap();

    // First open asks, and the question names the folder.
    let mut asked = Vec::new();
    let mut ask_yes = |q: &str| {
        asked.push(q.to_string());
        true
    };
    assert_eq!(establish(&root, &session, &mut ask_yes), Trust::Granted);
    assert_eq!(asked.len(), 1);
    assert!(asked[0].contains("open") && asked[0].contains("workspace"), "{}", asked[0]);

    // Unchanged project: the stored yes stands, nobody is asked.
    let mut must_not = |_: &str| panic!("an unchanged folder must not re-prompt");
    assert_eq!(establish(&root, &session, &mut must_not), Trust::Granted);

    // Prose changes nothing…
    std::fs::write(root.join("aiTerminal.md"), "notes").unwrap();
    assert_eq!(establish(&root, &session, &mut must_not), Trust::Granted);

    // …but a new MCP declaration is code, and re-opens the question.
    std::fs::write(root.join(".aiTerminal/mcp/new.toml"), "command = \"srv\"").unwrap();
    let mut asked_again = false;
    let mut ask_no = |q: &str| {
        asked_again = true;
        assert!(q.contains("MCP"), "the prompt names what runs code: {q}");
        false
    };
    assert_eq!(establish(&root, &session, &mut ask_no), Trust::Declined);
    assert!(asked_again);

    // A declined folder is not nagged on every open…
    let mut must_not2 = |_: &str| panic!("a declined folder must not re-prompt by itself");
    assert_eq!(establish(&root, &session, &mut must_not2), Trust::Declined);

    // …until /trust forgets the answer deliberately.
    reset(&session);
    let mut ask_final = |_: &str| true;
    assert_eq!(establish(&root, &session, &mut ask_final), Trust::Granted);

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&session);
}

#[test]
fn ctrl_j_grows_a_multiline_draft_and_the_caret_walks_its_rows() {
    let mut b = LineBuffer::default();
    typed(&mut b, "first");
    b.apply(&Key::Ctrl('j'));
    typed(&mut b, "second");
    assert_eq!(b.rows(), vec!["first".to_string(), "second".to_string()], "Ctrl+J grows the box; Enter still submits");
    assert_eq!(b.row_col(), (1, 6));
    assert!(b.move_row(false), "the caret walks up inside the draft");
    assert_eq!(b.row_col(), (0, 5), "the column clamps to the shorter row");
    assert!(!b.move_row(false), "…and the top edge falls through to history");
    // Ctrl+K kills to end of LINE, never through the newline.
    b.apply(&Key::Home);
    b.apply(&Key::Ctrl('k'));
    assert_eq!(b.rows(), vec!["".to_string(), "second".to_string()]);
}

#[test]
fn shift_tab_arrives_as_backtab_from_its_escape() {
    use crate::mdedit::key::parse_key;
    assert_eq!(parse_key(b"\x1b[Z"), Some((Key::BackTab, 3)));
    // …and a bare LF is Ctrl+J, distinct from Enter's CR.
    assert_eq!(parse_key(b"\n"), Some((Key::Ctrl('j'), 1)));
    assert_eq!(parse_key(b"\r"), Some((Key::Enter, 1)));
}

// ── inline runs: the line forwarder the GUI's child executor reads through ──

#[test]
fn forward_lines_turns_a_runs_output_into_appends_and_stops_when_nobody_listens() {
    let (tx, rx) = std::sync::mpsc::channel();
    let output = std::io::Cursor::new("node a \u{2713}\nnode b \u{2717}\n".as_bytes().to_vec());
    super::forward_lines(output, &tx);
    let mut lines = Vec::new();
    while let Ok(super::ui::Event::Append(line)) = rx.try_recv() {
        lines.push(line);
    }
    assert_eq!(lines, vec!["node a \u{2713}", "node b \u{2717}"]);

    // A dropped receiver ends the forwarder instead of spinning on send errors.
    let (tx, _) = std::sync::mpsc::channel();
    super::forward_lines(std::io::Cursor::new("unheard\n".as_bytes().to_vec()), &tx);
}

// ── the repo map: bounded orientation for the turn's grounding ──────────────

#[test]
fn the_repo_map_groups_by_top_dir_and_stays_bounded() {
    let dir = std::env::temp_dir().join(format!("tt-repo-map-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for f in ["src/main.rs", "src/lib.rs", "docs/guide.md", "README.md"] {
        let p = dir.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x").unwrap();
    }
    let map = super::repo_map(&dir);
    assert!(map.contains("src/") && map.contains("2 file(s)"), "{map}");
    assert!(map.contains("docs/") && map.contains("README.md"), "{map}");
    assert!(map.lines().count() <= 40, "the map is a summary, not a listing");
    assert!(super::repo_map(&dir.join("empty-nowhere")).is_empty(), "an empty folder maps to nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

// ── /guard: the dry-run verdict, in the guard's own words ──────────────────

#[test]
fn the_guard_verdict_answers_without_running_anything() {
    use crate::cli::workspace::repl::verdict_line;
    let guard = crate::guard::Guard::from_toml(
        "[[guard.command]]\npattern = \"^touch-the-config\"\nrule = \"confirm\"\n[[guard.command]]\npattern = \"^drop-the-db\"\nrule = \"deny\"\n[[guard.path]]\npattern = \"\\\\.pem$\"\nrule = \"deny\"\n",
    );
    assert!(verdict_line(&guard, "echo hello").contains("allowed"));
    let confirm = verdict_line(&guard, "touch-the-config now");
    assert!(confirm.contains("would ask you first"), "{confirm}");
    let deny = verdict_line(&guard, "drop-the-db now");
    assert!(deny.contains("denied"), "{deny}");
    let read = verdict_line(&guard, "read /keys/server.pem");
    assert!(read.contains("reading") && read.contains("denied"), "{read}");
    let write = verdict_line(&guard, "write notes.txt");
    assert!(write.contains("writing"), "{write}");
}
