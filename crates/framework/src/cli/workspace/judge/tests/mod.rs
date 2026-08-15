use super::*;
use crate::guard::Approver;
use std::sync::atomic::AtomicUsize;

/// The human at the keyboard, scripted: answers `answer` and counts being asked.
struct Human {
    answer: bool,
    asked: AtomicUsize,
}

impl Approver for Human {
    fn approve(&self, _act: &str, _reason: &str) -> bool {
        self.asked.fetch_add(1, Ordering::Relaxed);
        self.answer
    }
}

fn settings() -> crate::ai::AiSettings {
    std::env::set_var("TT_TEST_JUDGE_KEY", "k");
    let cat = crate::ai::builtin_default();
    let mut primary = cat.resolve("claude-opus-4-8");
    primary.api_key_env = "TT_TEST_JUDGE_KEY".into();
    crate::ai::AiSettings { pool: crate::ai::ModelPool::single(primary) }
}

/// A Judged approver over scripted verdicts and a scripted human.
fn judged(replies: &[&str], enabled: bool, human_answer: bool) -> (Judged<crate::ai::ScriptedTransport>, Arc<Human>) {
    let turns = replies.iter().map(|r| crate::ai::provider::text_sse(r, 5, 5)).collect();
    let client = crate::ai::Client::new(settings(), crate::ai::ScriptedTransport::new(turns));
    let human = Arc::new(Human { answer: human_answer, asked: AtomicUsize::new(0) });
    let flag = Arc::new(AtomicBool::new(enabled));
    (Judged::new(client, flag, human.clone(), None, "/tmp/proj".into()), human)
}

#[test]
fn a_safe_verdict_approves_without_interrupting_the_human() {
    let (j, human) = judged(&[r#"{"safe": true, "reason": "a local build step"}"#], true, false);
    assert!(j.approve("running `cargo build`", "confirm rule"));
    assert_eq!(human.asked.load(Ordering::Relaxed), 0, "the whole point: no interruption");
}

#[test]
fn an_unsafe_verdict_falls_through_to_the_human_and_their_word_stands() {
    let (j, human) = judged(&[r#"{"safe": false, "reason": "it leaves the workspace"}"#], true, true);
    assert!(j.approve("running `x`", "confirm rule"), "the human said yes");
    assert_eq!(human.asked.load(Ordering::Relaxed), 1);

    let (j, human) = judged(&[r#"{"safe": false, "reason": "it leaves the workspace"}"#], true, false);
    assert!(!j.approve("running `x`", "confirm rule"), "the human said no");
    assert_eq!(human.asked.load(Ordering::Relaxed), 1);
}

#[test]
fn anything_short_of_a_verdict_asks_the_human_too() {
    // Prose, silence — the judge hedging is never an approval.
    for reply in ["Looks fine to me!", "", "{\"safe\": \"yes\"}"] {
        let (j, human) = judged(&[reply], true, false);
        assert!(!j.approve("running `x`", "confirm rule"), "{reply:?} must not approve");
        assert_eq!(human.asked.load(Ordering::Relaxed), 1, "{reply:?} must ask");
    }
}

#[test]
fn disabled_the_decorator_is_a_transparent_pass_through() {
    // A safe verdict IS scripted — if the judge were consulted it would approve
    // without asking. The human being asked proves the judge never spoke.
    let (j, human) = judged(&[r#"{"safe": true, "reason": "would approve"}"#], false, false);
    assert!(!j.approve("running `x`", "confirm rule"));
    assert_eq!(human.asked.load(Ordering::Relaxed), 1, "plan/build behave exactly as before");
}
