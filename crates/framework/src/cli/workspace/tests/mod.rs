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
    let all: Vec<String> = ["/help", "/readonly", "/resume", "@flow", "@coder"].iter().map(|s| s.to_string()).collect();
    let mut b = LineBuffer::default();
    typed(&mut b, "/hel");
    assert!(b.complete(&all));
    assert_eq!(b.text(), "/help ");

    // Ambiguity grows only to the common prefix — "/readonly" and "/resume" share
    // nothing past "/re", so nothing is added and NEITHER is picked arbitrarily.
    let mut b = LineBuffer::default();
    typed(&mut b, "/re");
    assert!(!b.complete(&all));
    assert_eq!(b.text(), "/re");
    // With one letter more the tie is broken and completion finishes the word.
    b.apply(&Key::Char('a'));
    assert!(b.complete(&all));
    assert_eq!(b.text(), "/readonly ");

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
