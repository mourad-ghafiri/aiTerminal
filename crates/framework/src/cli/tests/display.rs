use crate::cli::agentloop::MAX_ATTACHMENTS;
use crate::cli::attach::{TEXT_ATTACH_MAX, collect_attachments};
use crate::cli::format::{cost_segment, human_bytes, human_cost, human_tokens, outcome_exit, run_footer_with};
use crate::cli::live::{clamp_tail, erase_seq};
use crate::cli::media::{diagram_output, is_open_diagram_fence};
use crate::cli::observe::{CliObserver, Spinner, is_display_tool_marker};
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

#[test]
fn thinking_bursts_get_one_marker_each() {
    let mut obs = CliObserver::new(Vec::new());
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
fn observer_suppresses_xml_tool_call_from_display() {
    use crate::ai::AgentObserver;
    // A `<tool_call>` (an alternate model format) must never leak into the streamed
    // display — prose before it still shows, the machine protocol does not.
    let mut obs = CliObserver::new(Vec::new());
    obs.on_delta("Let me look.\n<tool_call>fs.list .</tool_call>\n");
    assert!(obs.streamed.contains("Let me look."), "prose kept: {:?}", obs.streamed);
    assert!(!obs.streamed.contains("tool_call"), "the raw tool call is suppressed: {:?}", obs.streamed);
    assert!(!obs.streamed.contains("fs.list"), "the call body is suppressed: {:?}", obs.streamed);
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
fn observer_suppresses_mistral_and_llama_tool_calls() {
    use crate::ai::AgentObserver;
    let mut obs = CliObserver::new(Vec::new());
    obs.on_delta("Checking.\n[TOOL_CALLS] fs.list {\"path\":\".\"}\n");
    assert!(obs.streamed.contains("Checking."), "prose kept: {:?}", obs.streamed);
    assert!(!obs.streamed.contains("TOOL_CALLS"), "mistral marker suppressed: {:?}", obs.streamed);
    assert!(!obs.streamed.contains("fs.list"), "call body suppressed: {:?}", obs.streamed);
}

#[test]
fn spinner_is_inert_off_tty_and_stops_cleanly() {
    // Under `cargo test` stderr is piped → no thread, no frames; stop is a no-op.
    let mut sp = Spinner::start("waiting".into());
    assert!(sp.handle.is_none(), "no animation off-TTY (piped/background runs stay clean)");
    sp.stop();
}

#[test]
fn cli_observer_streams_prose_and_suppresses_the_tool_protocol() {
    use crate::ai::AgentObserver;
    let mut obs = CliObserver::new(Vec::new());
    obs.on_turn_start();
    // Prose streams through (in split chunks, mid-line), the @tool line and the
    // JSON after it never print.
    obs.on_delta("Let me look");
    obs.on_delta(" at the file.\n@to");
    obs.on_delta("ol fs.read {\"path\"");
    obs.on_delta(": \"x\"}\nmore protocol\n");
    obs.on_commit("Let me look at the file.");
    // Next turn: the final answer streams fully.
    obs.on_turn_start();
    obs.on_delta("The file says hello.");
    let out = String::from_utf8(obs.streamed.clone().into_bytes()).unwrap();
    assert!(out.contains("Let me look at the file."), "prose streamed: {out:?}");
    assert!(out.contains("The file says hello."), "final answer streamed: {out:?}");
    assert!(!out.contains("@tool"), "protocol suppressed: {out:?}");
    assert!(!out.contains("more protocol"), "post-tool JSON suppressed: {out:?}");
}

#[test]
fn cli_observer_holds_a_possible_marker_then_flushes_prose() {
    use crate::ai::AgentObserver;
    let mut obs = CliObserver::new(Vec::new());
    obs.on_turn_start();
    // "@toolbox" begins like the marker but isn't one — it must still print.
    obs.on_delta("@toolbox is a word\n");
    // A bare malformed marker never prints.
    obs.on_delta("@tool\n");
    assert!(obs.streamed.contains("@toolbox is a word"));
    assert!(!obs.streamed.contains("\n@tool\n"));
}

#[test]
fn agent_run_streams_live_through_the_cli_observer() {
    // End-to-end with MOCKS: a scripted tool-calling turn then the final answer,
    // driven through run_agent + CliObserver. No model, no network, no tools run
    // (the runner refuses, and the loop feeds the refusal back).
    let client = scripted(&[
        "Checking the file.\n@tool fs.read {\"path\": \"x\"}",
        "Done: the file is fine.",
    ]);
    let spec = crate::ai::AgentSpec {
        system: "You check things.".into(),
        tools: vec![crate::ai::ToolSpec { name: "fs.read".into(), describe: "read".into() }],
        max_steps: 3,
        ..Default::default()
    };
    let mut obs = CliObserver::new(Vec::new());
    let run = crate::ai::run_agent(&client, &spec, "check x", "", &mut NoTools, &mut obs);
    assert_eq!(run.answer, "Done: the file is fine.");
    assert!(obs.streamed.contains("Checking the file."), "turn prose streamed live");
    assert!(obs.streamed.contains("Done: the file is fine."), "answer streamed live");
    assert!(!obs.streamed.contains("@tool"), "protocol never reaches the display");
    assert_eq!(run.steps.len(), 1, "the tool call happened (and was refused by NoTools)");
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
