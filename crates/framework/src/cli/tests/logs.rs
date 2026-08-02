//! What a run's output looks like when somebody reads it back.
//!
//! Every log here is a real `.md` written by the code that writes it in production —
//! `flowruns::write_node`, `loops::write_iteration`, the job run-log header — so these
//! prove the round trip and not a fixture somebody typed to match.

use crate::cli::logs::LogSink;
use crate::cli::md::write_answer;
use crate::cli::observe::Recorder;

/// A scratch folder for one test, named after it so two never collide.
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tt-logs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The bytes a log would put on the screen, with `tty` deciding which screen.
fn read_back(path: &std::path::Path, markdown: bool, tty: bool) -> String {
    let mut sink = LogSink::open(markdown, tty, path);
    let screen = Recorder::default();
    let mut w = screen.clone();
    sink.feed(&mut w, &std::fs::read_to_string(path).unwrap());
    sink.close(&mut w);
    screen.text()
}

#[test]
fn a_log_is_drawn_as_the_document_it_is() {
    // The shape `loops::write_iteration` composes. Read back on a terminal it is a
    // document: the heading is drawn rather than spelled, the bullet is a bullet, and the
    // verifier's raw output keeps every character it had.
    let dir = scratch("document");
    let path = dir.join("1.md");
    std::fs::write(&path, "## iteration 1\n\nFixed the **parser**:\n\n- narrowed the guard\n\n### verifier\n\n```\ntest result: FAILED. 1 failed\n```\n").unwrap();

    let drawn = read_back(&path, true, true);
    assert!(!drawn.contains("## iteration"), "the heading is drawn, not spelled: {drawn:?}");
    assert!(drawn.contains("iteration 1"), "the words survive: {drawn:?}");
    assert!(!drawn.contains("**parser**"), "emphasis is drawn, not spelled: {drawn:?}");
    assert!(drawn.contains('\u{2022}'), "the bullet is a bullet: {drawn:?}");
    assert!(drawn.contains("test result: FAILED. 1 failed"), "the verifier is quoted whole: {drawn:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_pipe_gets_the_file_and_nothing_else() {
    // `@job log > run.md` has to write what was written. Not "roughly", not "rendered
    // plain" — the file.
    let dir = scratch("pipe");
    let path = dir.join("1.md");
    let source = "## iteration 1\n\nFixed the **parser**.\n";
    std::fs::write(&path, source).unwrap();

    assert_eq!(read_back(&path, true, false), source, "byte for byte");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_commands_output_is_never_reflowed() {
    // A shell job's log is not prose. A `#` line is a comment, the alignment is load
    // bearing, and a renderer would turn both into something else.
    let dir = scratch("command");
    let path = dir.join("1.md");
    let source = "$ git status\n# On branch main\n    modified:   src/main.rs\n";
    std::fs::write(&path, source).unwrap();

    assert_eq!(read_back(&path, false, true), source, "untouched on a terminal too");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_diagram_in_a_log_is_drawn_rather_than_shown() {
    // The other half of the promise: our AI features answer in Markdown AND mermaid, and
    // a fence read back as six lines of syntax is the thing this whole change is about.
    std::env::remove_var("TERM_PROGRAM");
    let dir = scratch("diagram");
    let path = dir.join("read.md");
    std::fs::write(&path, "## answered\n\n```mermaid\nflowchart TD\n  A[Start] --> B[Ship]\n```\n").unwrap();

    let drawn = read_back(&path, true, true);
    assert!(drawn.contains("Start") && drawn.contains("Ship"), "the labels are drawn: {drawn:?}");
    assert!(drawn.contains('\u{25bc}'), "and so is the arrow: {drawn:?}");
    assert!(!drawn.contains("-->"), "no diagram syntax reaches the reader: {drawn:?}");
    assert!(!drawn.contains("```"), "and no fence either: {drawn:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_followed_log_draws_each_block_as_it_lands() {
    // What `-f` is for. A document that arrives in pieces has to appear in pieces —
    // waiting for the run to end before drawing anything would make the flag useless.
    let dir = scratch("follow");
    let path = dir.join("1.md");
    std::fs::write(&path, "").unwrap();
    let mut sink = LogSink::open(true, true, &path);
    let screen = Recorder::default();
    let mut w = screen.clone();

    sink.feed(&mut w, "## iteration 1\n\nThe first pass.\n\n");
    let after_one = screen.text();
    assert!(after_one.contains("iteration 1"), "drawn before the second one exists: {after_one:?}");
    assert!(!after_one.contains("iteration 2"));

    sink.feed(&mut w, "## iteration 2\n\nThe second.\n\n");
    sink.close(&mut w);
    let both = screen.text();
    assert!(both.contains("iteration 1") && both.contains("iteration 2"), "{both:?}");
    assert!(both.starts_with(&after_one), "the first is never repainted or repeated: {both:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_node_transcript_reads_back_as_a_document() {
    // Through the real writer: `write_node` composes `# id / ## asked / ## answered`, and
    // that is what `@flow log` puts on the screen.
    let _home = crate::test_home::lock_home("logs-node");
    crate::flowruns::write_node("2000-1", "review", "Look at the diff.", "## Findings\n\n1. A missing bound.\n");
    let path = crate::flowruns::dir("2000-1").unwrap().join("nodes").join("review.md");

    let drawn = read_back(&path, true, true);
    assert!(!drawn.contains("## Findings"), "the heading is drawn: {drawn:?}");
    assert!(drawn.contains("Findings") && drawn.contains("A missing bound"), "{drawn:?}");
    assert!(drawn.contains("Look at the diff"), "what it was asked is still there: {drawn:?}");
}

#[test]
fn an_iteration_log_reads_back_as_a_document_and_keeps_the_verifier_whole() {
    // Through the real writer again: `write_iteration` composes `## iteration n` around
    // the maker's answer and fences what the verifier actually printed. Both halves have
    // to survive — the prose drawn, the command output not.
    let _home = crate::test_home::lock_home("logs-iteration");
    crate::loops::write_iteration("2000-2", 3, 1, "## What I changed\n\n- widened the guard\n", "error[E0308]: mismatched types");
    let path = crate::loops::dir("2000-2").unwrap().join("iterations").join("1.md");

    let drawn = read_back(&path, true, true);
    assert!(!drawn.contains("## What I changed"), "the maker's headings are drawn: {drawn:?}");
    assert!(drawn.contains("What I changed") && drawn.contains('\u{2022}'), "{drawn:?}");
    assert!(drawn.contains("error[E0308]: mismatched types"), "the verifier is quoted exactly: {drawn:?}");
}

#[test]
fn an_answer_is_drawn_on_a_terminal_and_untouched_in_a_pipe() {
    // A flow's answer, and an approval's `show`. Both are content: rendering is for the
    // person, and the pipe gets the Markdown the model wrote so `> review.md` is a
    // Markdown file.
    let answer = "# Review\n\nTwo things:\n\n- a missing bound\n- a stale comment\n";
    let screen = Recorder::default();
    let mut w = screen.clone();
    write_answer(&mut w, answer, true, true);
    let drawn = screen.text();
    assert!(!drawn.contains("# Review"), "drawn: {drawn:?}");
    assert!(drawn.contains("Review") && drawn.contains('\u{2022}'), "{drawn:?}");

    let piped = Recorder::default();
    let mut w = piped.clone();
    write_answer(&mut w, answer, true, false);
    assert_eq!(piped.text(), format!("{answer}\n"), "the source, plus the newline a println has always added");

    // A `run` node's answer is its command's output — the same bytes on both streams.
    let log = "warning: unused variable `x`\n  --> src/main.rs:4:9";
    let raw = Recorder::default();
    let mut w = raw.clone();
    write_answer(&mut w, log, false, true);
    assert_eq!(raw.text(), format!("{log}\n"), "never reflowed, terminal or not");
}
