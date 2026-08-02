use crate::cli::agentloop::MAX_ATTACHMENTS;
use crate::cli::attach::{TEXT_ATTACH_MAX, collect_attachments};
use crate::cli::format::{cost_segment, human_bytes, human_cost, human_tokens, outcome_exit, run_footer_with};
use crate::cli::live::{clamp_tail, erase_seq};
use crate::cli::media::{diagram_output, is_open_diagram_fence};
use crate::cli::observe::{CliObserver, Recorder, RunView, SharedView, Spinner, is_display_tool_marker};
use super::{NoTools, scripted};

#[test]
fn erase_seq_returns_to_the_top_of_the_painted_tail() {
    // Nothing painted → no cursor movement.
    assert_eq!(erase_seq(0), "");
    // One line: return to column 0, clear below (no cursor-up).
    assert_eq!(erase_seq(1), "\r\x1b[0J");
    // N lines: return to column 0, climb N-1 rows, clear below.
    assert_eq!(erase_seq(3), "\r\x1b[2A\x1b[0J");
}

#[test]
fn clamp_tail_keeps_only_the_newest_rows_within_the_viewport() {
    // Fits within the viewport → unchanged, exact line count.
    let (t, n) = clamp_tail("a\nb\nc", 5);
    assert_eq!((t.as_str(), n), ("a\nb\nc", 3));
    // Taller than the viewport → keep the NEWEST `max_rows` lines only.
    let (t, n) = clamp_tail("a\nb\nc\nd\ne", 2);
    assert_eq!((t.as_str(), n), ("d\ne", 2));
    // A zero cap means "no clamp" (paint it all).
    let (_, n) = clamp_tail("a\nb\nc", 0);
    assert_eq!(n, 3);
}

#[test]
fn open_diagram_fence_detected_only_while_unclosed() {
    assert!(is_open_diagram_fence("```mermaid\nflowchart TD"));
    assert!(!is_open_diagram_fence("```mermaid\nflowchart TD\n```"), "closed fence is complete");
    assert!(!is_open_diagram_fence("```rust\nlet x = 1;"), "not a diagram language");
    assert!(!is_open_diagram_fence("plain paragraph"));
}

// ── streaming display + attachments (all mocked / temp files) ────────────

#[test]
fn harness_chrome_formats_are_stable() {
    // Token + byte humanization and the run footer — the glanceable stats line.
    assert_eq!(human_tokens(950), "950");
    assert_eq!(human_tokens(12_345), "12.3k");
    assert_eq!(human_bytes(80), "80B");
    assert_eq!(human_bytes(2048), "2.0KB");
    let spent = |i: u32, o: u32| crate::ai::Usage { input: i, output: o, ..Default::default() };
    let f = run_footer_with("\u{2713}", std::time::Duration::from_millis(4200), 3, spent(12_345, 1_800), None, None);
    assert_eq!(f, "\u{2713} 4.2s \u{b7} 3 tools \u{b7} 12.3k in / 1800 out");
    let f1 = run_footer_with("\u{2713}", std::time::Duration::from_secs(61), 1, spent(100, 5), None, None);
    assert!(f1.contains("61s") && f1.contains("1 tool \u{b7}"), "{f1}");
    let f0 = run_footer_with("\u{2713}", std::time::Duration::from_millis(900), 0, spent(10, 2), None, None);
    assert!(!f0.contains("tool"), "no tool segment when none ran: {f0}");
}

#[test]
fn the_footer_says_how_much_of_the_prompt_was_reused() {
    // The number the whole caching change exists to produce. A run that reused its
    // prefix should be able to show it; one that did not must show nothing, so a
    // prompt that has quietly stopped being stable is visible rather than merely
    // expensive.
    let reused = crate::ai::Usage { input: 900, output: 200, cache_read: 8_100, cache_write: 0 };
    let f = run_footer_with("\u{2713}", std::time::Duration::from_secs(3), 2, reused, None, None);
    assert!(f.contains("9000 in"), "the whole prompt is counted, cached or not: {f}");
    assert!(f.contains("(8100 cached, 90%)"), "{f}");

    let cold = crate::ai::Usage { input: 9_000, output: 200, cache_read: 0, cache_write: 9_000 };
    let f = run_footer_with("\u{2713}", std::time::Duration::from_secs(3), 2, cold, None, None);
    assert!(!f.contains("cached"), "the first turn writes the cache, it does not read one: {f}");
}

#[test]
fn human_cost_and_cost_segment_format() {
    assert_eq!(human_cost(0.0), "");
    assert_eq!(human_cost(-1.0), "");
    assert_eq!(human_cost(0.0002), "<$0.001");
    assert_eq!(human_cost(0.014), "$0.014");
    assert_eq!(human_cost(1.2), "$1.20");
    assert_eq!(human_cost(250.0), "$250");
    // No pricing → no segment.
    assert_eq!(cost_segment(None, None), "");
    assert_eq!(cost_segment(Some(0.0), Some(0.10)), "");
    // Priced, no budget → just the cost.
    assert_eq!(cost_segment(Some(0.014), None), " \u{b7} ~$0.014");
    // Priced + budget → cost + percent.
    let seg = cost_segment(Some(0.014), Some(0.10));
    assert!(seg.contains("~$0.014") && seg.contains("14% of $0.100"), "{seg}");
    // Over budget → ⚠ marker.
    let over = cost_segment(Some(0.20), Some(0.10));
    assert!(over.contains("\u{26a0}") && over.contains("200% of"), "{over}");
}

// ── the display, through BOTH sinks ──────────────────────────────────────
//
// Every suppression test below runs twice: once writing raw text, once through the
// realtime Markdown renderer. That is not thoroughness for its own sake. Every one of
// these tests used to build the observer with no renderer, so the path a person on a
// terminal actually gets — the ONLY path that ships — had no coverage of the one thing
// this module exists to do. It leaked the whole `@tool` machine protocol onto the screen
// and the suite stayed green. A rule that is only checked on the path nobody runs is not
// a rule.

/// A recorded run: the observer, and the bytes that reached the screen.
fn observer(markdown: bool) -> (CliObserver, Recorder) {
    let screen = Recorder::default();
    let md = markdown.then(|| (corelib::md::Style::default(), 80));
    (CliObserver::new(SharedView::new(RunView::new(Box::new(screen.clone()), None, md))), screen)
}

/// Run `feed` through both sinks and hand each result to `check(shown, screen)`.
fn both_sinks(deltas: &[&str], check: impl Fn(&str, &str, bool)) {
    use crate::ai::AgentObserver;
    for markdown in [false, true] {
        let (mut obs, screen) = observer(markdown);
        for d in deltas {
            obs.on_delta(d);
        }
        obs.on_commit("");
        check(&obs.shown(), &screen.text(), markdown);
    }
}

#[test]
fn thinking_bursts_get_one_marker_each() {
    let (mut obs, _) = observer(false);
    // First chunk of a burst carries the ∴ marker; continuations don't.
    let a = obs.thinking_chunk("planning");
    let b = obs.thinking_chunk(" the fix");
    assert!(a.contains("\u{2234}"), "{a:?}");
    assert!(!b.contains("\u{2234}"), "{b:?}");
    // A new turn (on_turn_start resets) opens a fresh burst.
    use crate::ai::AgentObserver;
    obs.on_turn_start();
    obs.wake(); // don't leave the spinner thread running in tests
    let c = obs.thinking_chunk("next turn");
    assert!(c.contains("\u{2234}"), "{c:?}");
}

#[test]
fn display_marker_recognizes_all_dialects() {
    // The display filter is sourced from the parser's TOOL_LINE_MARKERS, so every
    // tolerated line-anchored dialect is suppressed from the live stream.
    for m in ["@tool fs.x {}", "<tool_call>", "```tool", "[TOOL_CALLS] fs.x{}", "<|python_tag|>fs.x()"] {
        assert!(is_display_tool_marker(m), "{m:?} must be suppressed");
    }
    // Plain prose is not a marker.
    assert!(!is_display_tool_marker("Here is the answer."));
}

#[test]
fn no_dialect_of_the_tool_protocol_reaches_either_sink() {
    // One table, both sinks. `keep` must survive; `banish` must never appear in the
    // logical answer OR in the bytes drawn on the screen.
    let cases: &[(&str, &[&str], &str, &[&str])] = &[
        ("xml", &["Let me look.\n<tool_call>fs.list .</tool_call>\n"], "Let me look.", &["tool_call", "fs.list"]),
        ("mistral", &["Checking.\n[TOOL_CALLS] fs.list {\"path\":\".\"}\n"], "Checking.", &["TOOL_CALLS", "fs.list"]),
        ("llama", &["One moment.\n<|python_tag|>fs.read(path=\"x\")\n"], "One moment.", &["python_tag"]),
        ("fenced", &["Reading it.\n```tool\nfs.read {\"path\":\"x\"}\n```\n"], "Reading it.", &["```tool", "fs.read"]),
        // Split mid-marker across chunks — the streamed case, where the filter has to
        // hold an undecided prefix rather than print it and regret it.
        ("split", &["Let me look", " at the file.\n@to", "ol fs.read {\"path\"", ": \"x\"}\nmore protocol\n"], "at the file.", &["@tool", "more protocol"]),
    ];
    for (name, deltas, keep, banish) in cases {
        both_sinks(deltas, |shown, screen, markdown| {
            assert!(shown.contains(keep), "{name}/{markdown}: prose kept — {shown:?}");
            assert!(screen.contains(keep), "{name}/{markdown}: prose drawn — {screen:?}");
            for bad in *banish {
                assert!(!shown.contains(bad), "{name}/{markdown}: {bad:?} suppressed — {shown:?}");
                assert!(!screen.contains(bad), "{name}/{markdown}: {bad:?} never drawn — {screen:?}");
            }
        });
    }
}

#[test]
fn a_word_that_merely_begins_like_the_marker_still_prints() {
    both_sinks(&["@toolbox is a word\n", "@tool\n"], |shown, screen, markdown| {
        assert!(shown.contains("@toolbox is a word"), "{markdown}: {shown:?}");
        assert!(screen.contains("@toolbox is a word"), "{markdown}: {screen:?}");
        assert!(!shown.contains("\n@tool\n"), "a bare malformed marker never prints: {shown:?}");
    });
}

#[test]
fn spinner_is_inert_off_tty_and_stops_cleanly() {
    // Under `cargo test` stderr is piped → no thread, no frames; stop is a no-op.
    let mut sp = Spinner::start(String::from("waiting"));
    assert!(sp.handle.is_none(), "no animation off-TTY (piped/background runs stay clean)");
    sp.stop();
}

#[test]
fn a_tool_only_turn_never_prints_the_protocol_and_never_repeats_itself() {
    // The bug as it was reported. Three turns that are NOTHING but tool calls — no prose
    // at all, which is the common case — driven end to end through `run_agent`.
    //
    // Two things went wrong and both are asserted. The protocol printed verbatim, because
    // the renderer path skipped the filter entirely. And the renderer's block was never
    // finalized (nothing called `on_commit` for a turn with no prose), so every later
    // token re-rendered it — turn 1, then turns 1+2, then turns 1+2+3, growing a
    // duplicate of the whole run down the screen.
    let client = scripted(&[
        "@tool fs.list {\"path\": \".\"}\n@tool fs.search {\"query\": \"README\"}",
        "@tool fs.read {\"path\": \"README.md\"}",
        "The project is a terminal written from scratch.",
    ]);
    let spec = crate::ai::AgentSpec {
        system: "You explore.".into(),
        tools: ["fs.list", "fs.search", "fs.read"]
            .iter()
            .map(|n| crate::ai::ToolSpec { name: (*n).into(), describe: "t".into() })
            .collect(),
        max_steps: 6,
        ..Default::default()
    };
    for markdown in [false, true] {
        let (mut obs, screen) = observer(markdown);
        let run = crate::ai::run_agent(&client, &spec, "explain this project", "", &mut NoTools, &mut obs);
        crate::cli::observe::finish_streamed(&mut obs, &run.answer);
        let out = screen.text();
        assert_eq!(run.answer, "The project is a terminal written from scratch.");
        assert!(out.contains("terminal written from scratch"), "{markdown}: the answer is drawn — {out:?}");
        for bad in ["@tool", "fs.list", "fs.search", "README.md"] {
            assert!(!out.contains(bad), "{markdown}: {bad:?} is machine protocol and must never be drawn — {out:?}");
        }
        for line in screen_of(&out) {
            assert!(!line.contains("@tool"), "{markdown}: nothing on the final screen is protocol — {line:?}");
        }
    }
}

#[test]
fn a_turn_boundary_finalizes_the_tail_so_nothing_is_drawn_twice() {
    // The renderer keeps the in-progress block and re-renders it on every token. A turn
    // that ends without that block being committed leaves it there — so the NEXT turn's
    // tokens re-render it too, and the one after that, growing a duplicate of the whole
    // run down the screen. That is exactly what was reported: block two was turns 1+2,
    // block three was turns 1+2+3.
    //
    // There are two seals, because a turn ends twice as far as a display is concerned:
    // `on_commit` when its words are final, and `on_turn_start` when the next one begins.
    // Each is pinned here on its own, so removing either is caught.
    use crate::ai::AgentObserver;
    for seal in ["on_commit", "on_turn_start"] {
        let (mut obs, screen) = observer(true);
        obs.on_delta("The first thing I found.");
        match seal {
            "on_commit" => obs.on_commit(""),
            _ => {
                obs.on_turn_start();
                obs.wake(); // don't leave the spinner thread running in tests
            }
        }
        obs.on_delta("The second thing I found.");
        obs.on_commit("");
        let rows = screen_of(&screen.text());
        let hits = |what: &str| rows.iter().filter(|l| l.contains(what)).count();
        assert_eq!(hits("The first thing I found."), 1, "{seal}: drawn once — {rows:?}");
        assert_eq!(hits("The second thing I found."), 1, "{seal}: drawn once — {rows:?}");
    }
}

/// What the bytes actually put ON A SCREEN, run through this project's own VT engine.
///
/// A repainting display writes each frame plus the escapes that undo the last one, so
/// counting occurrences in the byte stream measures how many times something was painted,
/// not how many times it ended up visible. The only honest question — "what would a person
/// see?" — is answered by a terminal, and there is one right here.
fn screen_of(bytes: &str) -> Vec<String> {
    let mut term = platform::term::Term::new(100, 60);
    term.feed(bytes.as_bytes());
    term.screen_text()
}

#[test]
fn a_trace_line_survives_the_repaint_that_follows_it() {
    // The other half of the same bug: the trace went to stderr while the answer repainted
    // stdout, so the next frame's cursor-up climbed over the trace and ate half of it —
    //
    //     ⚙ fs.list . · 0ms
    //   · 5 entries
    //
    // Everything goes through the one view now, so a committed line stays whole while the
    // answer keeps streaming underneath it.
    use crate::ai::AgentObserver;
    use crate::flow::board::ToolTrace;
    let screen = Recorder::default();
    let view = SharedView::new(RunView::new(Box::new(screen.clone()), None, Some((corelib::md::Style::default(), 80))));
    let mut obs = CliObserver::new(view.clone());
    obs.on_delta("Looking at it");
    view.tool("\u{2699} fs.list     . \u{b7} 0ms \u{b7} 5 entries");
    obs.on_delta(" now, in detail.");
    obs.on_commit("");
    let rows = screen_of(&screen.text());
    assert!(
        rows.iter().any(|l| l.contains("\u{2699} fs.list     . \u{b7} 0ms \u{b7} 5 entries")),
        "the trace line is whole, on one row: {rows:?}"
    );
    assert!(rows.iter().any(|l| l.contains("now, in detail.")), "and the answer carried on under it: {rows:?}");
}

#[test]
fn a_committed_turn_keeps_its_words_above_what_comes_next() {
    // What `on_commit` is FOR. Once a turn's words are final the tail is sealed, so the
    // tool trace that follows lands under them. Without that seal the trace is written
    // where the tail began and the tail repaints beneath it — the run then reads as
    // "called a tool, then said why", which is backwards.
    use crate::ai::AgentObserver;
    use crate::flow::board::ToolTrace;
    let screen = Recorder::default();
    let view = SharedView::new(RunView::new(Box::new(screen.clone()), None, Some((corelib::md::Style::default(), 80))));
    let mut obs = CliObserver::new(view.clone());
    obs.on_delta("Let me read the file.");
    obs.on_commit("Let me read the file.");
    view.tool("\u{2699} fs.read     README.md \u{b7} 1ms \u{b7} 752B");
    let rows = screen_of(&screen.text());
    let at = |what: &str| rows.iter().position(|l| l.contains(what));
    let (said, did) = (at("Let me read the file."), at("fs.read"));
    assert!(said.is_some() && did.is_some(), "both are on screen: {rows:?}");
    assert!(said < did, "the words come first, then what they led to: {rows:?}");
}

#[test]
fn a_call_reported_while_it_runs_is_replaced_by_what_it_returned() {
    // A forty-second `sys.run cargo test` printed nothing until it was over, which on a
    // screen is indistinguishable from a hang. So a call still going after a moment says
    // so — and then the line it left has to become the finished one, in place. Saying it
    // twice would be worse than the silence it replaced.
    use crate::flow::board::ToolTrace;
    let screen = Recorder::default();
    let view = SharedView::new(RunView::new(Box::new(screen.clone()), None, Some((corelib::md::Style::default(), 80))));
    view.tool_started("\u{22ef} sys.run     cargo test --workspace");
    view.tool_finished("\u{2699} sys.run     cargo test --workspace \u{b7} 4.1s \u{b7} 48 lines");
    let rows = screen_of(&screen.text());
    let hits = |what: &str| rows.iter().filter(|l| l.contains(what)).count();
    assert_eq!(hits("cargo test --workspace"), 1, "one line, not two: {rows:?}");
    assert_eq!(hits("48 lines"), 1, "and it is the finished one: {rows:?}");
    assert_eq!(hits("\u{22ef}"), 0, "the 'still going' mark is gone: {rows:?}");

    // Off a terminal there is no cursor to climb back with, so both lines are printed —
    // a log that shows a call starting and then finishing is telling the truth.
    let piped = Recorder::default();
    let raw = SharedView::new(RunView::new(Box::new(piped.clone()), None, None));
    raw.tool_started("\u{22ef} sys.run     cargo test");
    raw.tool_finished("\u{2699} sys.run     cargo test \u{b7} 4.1s \u{b7} 48 lines");
    assert_eq!(piped.text().lines().count(), 2, "both, in order: {:?}", piped.text());
}

#[test]
fn a_job_log_keeps_the_text_and_none_of_the_cursor_arithmetic() {
    // A foreground `@job` used to tee the live renderer's repaint frames into its log,
    // so `@job log` read back as control codes. The log gets committed text only.
    use crate::ai::AgentObserver;
    use crate::flow::board::ToolTrace;
    let dir = std::env::temp_dir().join(format!("tt-joblog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("run.log");
    let file = std::fs::File::create(&path).unwrap();
    let view = SharedView::new(RunView::new(Box::new(Recorder::default()), Some(file), Some((corelib::md::Style::default(), 80))));
    let mut obs = CliObserver::new(view.clone());
    obs.on_delta("A short answer.\n");
    view.tool("\u{2699} fs.read x \u{b7} 1ms \u{b7} 12B");
    obs.on_commit("");
    let logged = std::fs::read_to_string(&path).unwrap();
    assert!(logged.contains("A short answer."), "the words are in the log: {logged:?}");
    assert!(logged.contains("fs.read x"), "and so is what it did: {logged:?}");
    assert!(!logged.contains('\u{1b}'), "no escape sequences reach a file: {logged:?}");
    let _ = std::fs::remove_dir_all(&dir);
}


#[test]
fn attachments_collect_media_inline_text_and_skip_junk() {
    let dir = std::env::temp_dir().join(format!("tt-attach-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("shot.png"), b"\x89PNG fakebytes").unwrap();
    std::fs::write(dir.join("doc.pdf"), b"%PDF-1.4 fake").unwrap();
    std::fs::write(dir.join("notes.txt"), "remember the milk").unwrap();
    std::fs::write(dir.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
    let p = |n: &str| dir.join(n).display().to_string();
    let prompt = format!("look at @{} and @{} and @{} and @{} and @/no/such/file plus user@host", p("shot.png"), p("doc.pdf"), p("notes.txt"), p("blob.bin"));
    let (clean, media, file_ctx) = collect_attachments(&prompt);
    // Media: the image + the pdf, base64-encoded with the right types.
    assert_eq!(media.len(), 2);
    assert_eq!(media[0].media_type, "image/png");
    assert_eq!(media[1].media_type, "application/pdf");
    assert_eq!(corelib::codec::base64_decode(&media[0].b64).unwrap(), b"\x89PNG fakebytes");
    // Text inlines fenced; binary is skipped; a missing path stays as typed.
    assert!(file_ctx.contains("remember the milk"));
    assert!(file_ctx.contains("notes.txt"));
    assert!(!file_ctx.contains("blob.bin"), "binary skipped from the context");
    assert!(clean.contains("@/no/such/file"), "non-file tokens untouched");
    assert!(clean.contains("user@host"), "mid-word @ untouched");
    assert!(!clean.contains(&format!("@{}", p("shot.png"))), "the @ is dropped from real paths");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn attachments_are_capped_in_count() {
    let dir = std::env::temp_dir().join(format!("tt-attach-count-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut prompt = String::from("summarize");
    for i in 0..20 {
        let f = dir.join(format!("f{i}.txt"));
        std::fs::write(&f, format!("file number {i}")).unwrap();
        prompt.push_str(&format!(" @{}", f.display()));
    }
    let (_, media, file_ctx) = collect_attachments(&prompt);
    assert!(media.is_empty());
    let count = file_ctx.matches("## Attached file:").count();
    assert_eq!(count, MAX_ATTACHMENTS, "attachment count bounded");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn attachments_truncate_large_text_files() {
    let dir = std::env::temp_dir().join(format!("tt-attach-big-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let big = "x".repeat(TEXT_ATTACH_MAX + 1000);
    std::fs::write(dir.join("big.log"), &big).unwrap();
    let (_, media, file_ctx) = collect_attachments(&format!("@{}", dir.join("big.log").display()));
    assert!(media.is_empty());
    assert!(file_ctx.contains("(truncated)"));
    assert!(file_ctx.len() < big.len(), "inlined text is capped");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn diagram_draws_as_text_art_off_our_terminal() {
    // Not our GUI terminal (TERM_PROGRAM unset) → the picture in box art, never the
    // syntax, and never a native OSC the other terminal couldn't read.
    std::env::remove_var("TERM_PROGRAM");
    let out = diagram_output("flowchart TD\n A[Start] --> B[End]");
    assert!(out.contains("Start") && out.contains("End"), "the labels are drawn: {out:?}");
    assert!(out.contains('▼'), "an arrowhead is drawn: {out:?}");
    assert!(!out.contains("-->"), "no diagram syntax reaches the user: {out:?}");
    assert!(!out.contains("\x1b]1338"), "no native OSC off our terminal");
}

#[test]
fn an_unreadable_diagram_still_falls_back_to_a_box() {
    std::env::remove_var("TERM_PROGRAM");
    let out = diagram_output("this is not a diagram at all");
    assert!(out.contains("diagram") && out.contains('╭'), "fallback box: {out:?}");
    assert!(out.contains("this is not a diagram at all"));
}

// ── production-harness guarantees: exit codes, jobs, discovery ───────────

#[test]
fn outcomes_map_to_honest_exit_codes() {
    use crate::ai::RunOutcome;
    assert_eq!(outcome_exit(&RunOutcome::Completed), 0);
    assert_eq!(outcome_exit(&RunOutcome::Error("boom".into())), 1);
    assert_eq!(outcome_exit(&RunOutcome::StepLimit), 1);
    assert_eq!(outcome_exit(&RunOutcome::ToolStall), 1);
    assert_eq!(outcome_exit(&RunOutcome::Cancelled), 130, "the interrupt convention");
}
