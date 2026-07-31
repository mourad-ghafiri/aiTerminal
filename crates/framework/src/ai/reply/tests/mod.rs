use super::*;

#[derive(Default)]
struct Recorder {
    answer: String,
    thinking: String,
}
impl ReplySink for Recorder {
    fn answer(&mut self, text: &str) {
        self.answer.push_str(text);
    }
    fn thinking(&mut self, text: &str) {
        self.thinking.push_str(text);
    }
}

/// Split a reply into chunks the way a stream would, to prove the decision does not
/// depend on where the chunk boundaries fall.
fn stream(chunks: &[&str]) -> Vec<StreamEvent> {
    let mut evs: Vec<StreamEvent> = chunks.iter().map(|c| StreamEvent::Delta((*c).to_string())).collect();
    evs.push(StreamEvent::Done { stop_reason: None, input_tokens: 10, output_tokens: 5, cache_read: 0, cache_write: 0 });
    evs
}

fn classify(chunks: &[&str]) -> (CommandReply, Recorder) {
    let mut rec = Recorder::default();
    let out = classify_command_reply(stream(chunks).into_iter(), &mut rec);
    (out.reply, rec)
}

#[test]
fn a_run_line_becomes_a_command() {
    let (reply, rec) = classify(&["RUN: git status"]);
    assert_eq!(reply, CommandReply::Command("git status".into()));
    assert!(rec.answer.is_empty(), "a command must not also render as prose");
}

#[test]
fn the_decision_does_not_depend_on_chunk_boundaries() {
    // The marker almost never arrives in one delta.
    for split in [
        vec!["R", "U", "N", ":", " git status"],
        vec!["RU", "N: git", " status"],
        vec!["RUN", ": git status"],
        vec!["RUN: gi", "t status"],
    ] {
        assert_eq!(classify(&split).0, CommandReply::Command("git status".into()), "{split:?}");
    }
}

#[test]
fn prose_starts_rendering_before_the_stream_ends() {
    // The point of holding back only the undecided prefix: by the time the second
    // delta has been read, the first is already on screen.
    let mut rec = Recorder::default();
    let evs = vec![StreamEvent::Delta("Rust is ".into()), StreamEvent::Delta("a language".into())];
    let out = classify_command_reply(evs.into_iter(), &mut rec);
    assert_eq!(out.reply, CommandReply::Answer);
    assert_eq!(rec.answer, "Rust is a language");
}

#[test]
fn a_word_that_merely_starts_like_run_is_prose() {
    // "Running", "Run the tests…" — no colon, so not the marker.
    for text in ["Running the tests will…", "Run the test suite first.", "RUNTIME errors happen"] {
        let (reply, rec) = classify(&[text]);
        assert_eq!(reply, CommandReply::Answer, "{text:?}");
        assert_eq!(rec.answer, text);
    }
}

#[test]
fn the_marker_is_matched_case_insensitively_and_after_space() {
    assert_eq!(classify(&["run: ls"]).0, CommandReply::Command("ls".into()));
    assert_eq!(classify(&["  RUN: ls"]).0, CommandReply::Command("ls".into()));
    assert_eq!(classify(&["\n\nRun: ls"]).0, CommandReply::Command("ls".into()));
}

#[test]
fn a_model_that_keeps_talking_cannot_smuggle_a_second_line() {
    // Only the first line is a command. Everything after it is discarded, not
    // concatenated — otherwise a chatty model appends a line to your shell.
    let (reply, _) = classify(&["RUN: ls -la\nrm -rf /\nsudo reboot"]);
    assert_eq!(reply, CommandReply::Command("ls -la".into()));
}

#[test]
fn a_reply_that_is_only_the_marker_yields_an_empty_command() {
    // Nothing to run — the caller decides what to do with it, but it must not be
    // classified as prose and printed as if it were an answer.
    assert_eq!(classify(&["RUN:"]).0, CommandReply::Command(String::new()));
}

#[test]
fn a_reply_with_nothing_in_it_is_not_an_answer() {
    // An empty answer renders as nothing and preloads nothing, so the user who asked
    // for a command would get a bare prompt back with no way to tell it from a no-op.
    for blank in [vec![""], vec![], vec!["  ", "\n"]] {
        let (reply, rec) = classify(&blank);
        assert_eq!(reply, CommandReply::Empty, "{blank:?}");
        assert!(rec.answer.is_empty());
    }
}

#[test]
fn a_reply_shorter_than_the_marker_is_still_classified() {
    // The stream ends while the prefix is still undecided — the buffered head must
    // not be silently dropped.
    let (reply, rec) = classify(&["Ru"]);
    assert_eq!(reply, CommandReply::Answer);
    assert_eq!(rec.answer, "Ru");
}

#[test]
fn an_error_is_neither_a_command_nor_an_answer() {
    let mut rec = Recorder::default();
    let evs = vec![StreamEvent::Error("401 unauthorized".into())];
    let out = classify_command_reply(evs.into_iter(), &mut rec);
    assert_eq!(out.reply, CommandReply::Failed("401 unauthorized".into()));
    assert!(rec.answer.is_empty());
}

#[test]
fn reasoning_is_kept_apart_from_the_answer() {
    let mut rec = Recorder::default();
    let evs = vec![
        StreamEvent::Thinking("the user wants…".into()),
        StreamEvent::Delta("Here you go".into()),
        StreamEvent::Done { stop_reason: None, input_tokens: 1, output_tokens: 2, cache_read: 0, cache_write: 0 },
    ];
    let out = classify_command_reply(evs.into_iter(), &mut rec);
    assert_eq!(out.reply, CommandReply::Answer);
    assert_eq!(rec.thinking, "the user wants…");
    assert_eq!(rec.answer, "Here you go");
}

#[test]
fn usage_is_reported_from_the_done_event() {
    let out = classify_command_reply(stream(&["hello"]).into_iter(), &mut Recorder::default());
    assert_eq!((out.input_tokens, out.output_tokens), (10, 5));
}
