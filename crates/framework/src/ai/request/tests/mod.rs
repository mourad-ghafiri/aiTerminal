use super::*;
use crate::ai::model::AiSettings;

#[test]
fn qa_request_uses_the_given_model_and_carries_params() {
    let mut m = ModelDef::default();
    m.temperature = Some(0.5);
    let req = qa_request(&m, "hello", "");
    assert_eq!(req.model, m.id);
    assert_eq!(req.temperature, Some(0.5)); // straight from the chosen model
    assert_eq!(req.messages[0].role, Role::User);
}

#[test]
fn command_request_is_a_streamable_teacher_contract() {
    let s = AiSettings::default();
    let m = s.choose();
    let req = command_request(&m, "list files", "");
    assert_eq!(req.temperature, Some(0.0));
    assert!(req.messages[0].content.contains("Request: list files"));
    let sys = req.system.unwrap();
    // The streamable command header + the teacher/no-jargon guidance are both present.
    assert!(sys.contains("RUN:"), "command header: {sys}");
    assert!(sys.contains("teacher"));
    assert!(sys.contains("never call anything \"markdown\" or \"mermaid\""), "no-jargon rule present");
}

/// The turns of one run: the same system prompt, an ever-growing conversation.
fn turns(system: &str, messages: &[&str]) -> Vec<ChatRequest> {
    let m = AiSettings::default().choose();
    (1..=messages.len())
        .map(|n| {
            let so_far = messages[..n]
                .iter()
                .enumerate()
                .map(|(i, c)| Message {
                    role: if i % 2 == 0 { Role::User } else { Role::Assistant },
                    content: (*c).to_string(),
                })
                .collect();
            agent_request(&m, system, so_far)
        })
        .collect()
}

#[test]
fn a_run_declares_its_prefix_settled_so_a_provider_can_reuse_it() {
    // The whole caching change in one assertion: the system block is fixed for the
    // run, and every message but the newest has already been sent once.
    let run = turns("you are a careful engineer", &["do the thing", "@tool fs.list {}", "1 file"]);
    assert_eq!(run[0].cache, CacheHints { system: true, stable_messages: 0 }, "turn one has nothing settled yet");
    assert_eq!(run[1].cache, CacheHints { system: true, stable_messages: 1 });
    assert_eq!(run[2].cache, CacheHints { system: true, stable_messages: 2 });
    // A one-shot request claims nothing: there is no later turn to reuse it.
    assert_eq!(qa_request(&AiSettings::default().choose(), "hi", "").cache, CacheHints::none());
}

#[test]
fn the_prefix_never_moves_while_a_run_grows() {
    // A cache pays out only on a prefix that matches token for token, so what has
    // already been sent must never be edited — only added to. This is the assertion
    // that keeps that true after somebody rewrites the transcript in six months.
    let run = turns("you are a careful engineer", &["do the thing", "@tool fs.list {}", "1 file", "done"]);
    for pair in run.windows(2) {
        let (before, after) = (&pair[0], &pair[1]);
        assert_eq!(before.system, after.system, "the system block was rewritten mid-run");
        for (i, m) in before.messages.iter().enumerate() {
            assert_eq!(m, &after.messages[i], "message {i} changed between turns");
        }
        assert_eq!(after.messages.len(), before.messages.len() + 1, "a turn adds exactly one message");
    }
}

#[test]
fn the_digest_catches_a_prefix_that_stopped_being_stable() {
    // `prefix_digest` exists to be held on to by a test. A prompt that grows a
    // timestamp, a tool list in directory order, a set iterated instead of a vector
    // — each one silently voids the cache, and none of them looks like a bug.
    let m = AiSettings::default().choose();
    let one = agent_request(&m, "system A", vec![Message::user("a"), Message::user("b")]);
    let two = agent_request(&m, "system A", vec![Message::user("a"), Message::user("b")]);
    assert_eq!(one.prefix_digest(), two.prefix_digest(), "the same run rebuilt is the same prefix");

    // Anything in the settled part moving is a different prefix, and a cache miss.
    let changed = agent_request(&m, "system A (built at 12:04)", vec![Message::user("a"), Message::user("b")]);
    assert_ne!(one.prefix_digest(), changed.prefix_digest(), "a system prompt that varies is caught");
    let reordered = agent_request(&m, "system A", vec![Message::user("b"), Message::user("a")]);
    assert_ne!(one.prefix_digest(), reordered.prefix_digest(), "a reordered history is caught");

    // A growing run has a growing prefix — that is the point, and the digest says so.
    // What must hold is that the new prefix EXTENDS the old one rather than replacing
    // it: measured over the earlier turn's length, the two are the same bytes.
    let next = agent_request(&m, "system A", vec![Message::user("a"), Message::user("b"), Message::user("c")]);
    assert_ne!(one.prefix_digest(), next.prefix_digest(), "turn three settles more than turn two did");
    let rewound = ChatRequest { cache: one.cache, ..next.clone() };
    assert_eq!(one.prefix_digest(), rewound.prefix_digest(), "and everything turn two sent is still there, unchanged");

    // Whereas a run that edited its history does NOT extend — which is the failure
    // this whole digest exists to name.
    let edited = agent_request(&m, "system A", vec![Message::user("a EDITED"), Message::user("b"), Message::user("c")]);
    let rewound = ChatRequest { cache: one.cache, ..edited };
    assert_ne!(one.prefix_digest(), rewound.prefix_digest());
}

#[test]
fn context_is_prepended_to_prompt() {
    let req = qa_request(&AiSettings::default().choose(), "why?", "ctx-block");
    let content = &req.messages[0].content;
    assert!(content.starts_with("ctx-block"));
    assert!(content.contains("why?"));
}
