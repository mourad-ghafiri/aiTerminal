//! Neutral, provider-independent request shapes + the two intent builders (Q&A
//! and natural-language → command). Each provider adapter encodes a [`ChatRequest`]
//! into its own wire body ([`crate::ai::provider::Provider::encode_body`]).

use crate::ai::provider::ModelDef;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Message { role: Role::User, content: content.into() }
    }
}

/// A base64 image attached to a turn (vision input). `media_type` is the MIME type
/// (`image/png`, `image/jpeg`, …); `b64` is the standard base64 of the file bytes. The
/// provider adapter emits it in its own wire shape; non-vision models drop it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageData {
    pub media_type: String,
    pub b64: String,
}

/// What the caller knows will not change between the turns of one run.
///
/// A **fact about the conversation**, never a vendor mechanism — which is why it lives
/// on the neutral request and each [`Provider`](crate::ai::provider::Provider) decides
/// what to do with it. Anthropic turns it into `cache_control` breakpoints; OpenAI
/// caches a matching prefix automatically and needs nothing but for us not to disturb
/// the order.
///
/// The two facts are true **by construction** rather than by hope: `run_agent` builds
/// its system prompt once per run, and a `Transcript` only ever appends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheHints {
    /// The system block is fixed for the whole run.
    pub system: bool,
    /// How many leading messages are settled — everything but the newest turn.
    pub stable_messages: usize,
}

impl CacheHints {
    /// Nothing is known to be stable: a one-shot request that will never be repeated.
    pub fn none() -> Self {
        CacheHints::default()
    }

    /// One turn of a run: the system block is fixed and every message but the last has
    /// already been sent once.
    pub fn for_turn(messages: usize) -> Self {
        CacheHints { system: true, stable_messages: messages.saturating_sub(1) }
    }
}

/// A chat completion request — provider-independent. Always streamed.
#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    /// Request extended ("adaptive") thinking. Adapters that support it emit the
    /// vendor field; others ignore it.
    pub thinking: bool,
    /// Images attached to the LAST user message (vision input) — emitted by the adapter
    /// as image content blocks; empty for a text-only request.
    pub images: Vec<ImageData>,
    /// What the caller knows is settled, so a provider can reuse it.
    pub cache: CacheHints,
}

impl ChatRequest {
    /// Attach vision images to the request (the host gates on the model's `enable_vision`).
    pub fn with_images(mut self, images: Vec<ImageData>) -> Self {
        self.images = images;
        self
    }

    /// A fingerprint of the part of this request that must not move between turns.
    ///
    /// A cache is worth nothing if the prefix shifts, and a prefix shifts silently:
    /// a tool list in `read_dir` order, a timestamp somebody adds to the system prompt,
    /// a set iterated instead of a vector. This is what a test holds on to, so the
    /// regression is caught here rather than as a bill at the end of the month.
    pub fn prefix_digest(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |bytes: &[u8]| {
            for b in bytes {
                h ^= *b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        eat(self.model.as_bytes());
        eat(self.system.as_deref().unwrap_or("").as_bytes());
        for m in self.messages.iter().take(self.cache.stable_messages) {
            eat(m.role.as_str().as_bytes());
            eat(m.content.as_bytes());
        }
        h
    }
}

/// The teacher-persona guidance shared by the `@ai` answer and Q&A prompts: explain like a
/// great, concise teacher and lean on visual diagrams — WITHOUT exposing any of the underlying
/// formatting/diagram technology to the user.
const TEACHER: &str = "Explain like a brilliant, concise teacher: lead with the answer, keep it \
tight, and make it click. **Use a diagram whenever a picture makes the idea clearer** — draw it \
in a fenced ```mermaid code block. EVERY mermaid diagram type renders natively here — flowchart, \
sequenceDiagram, classDiagram, stateDiagram-v2, erDiagram, gantt, pie, journey, timeline, mindmap, \
kanban, gitGraph, quadrantChart, requirementDiagram, C4Context, xychart-beta, sankey-beta, \
block-beta, packet-beta, radar-beta, treemap-beta, architecture-beta — so pick whichever one fits \
the idea. This terminal renders your text \
and diagrams natively and beautifully, so just include them. NEVER mention formatting or diagram \
technology, never call anything \"markdown\" or \"mermaid\", never show diagram syntax as something \
the user must handle, and never tell the user to paste, open, or render anything elsewhere — the \
diagram simply appears. Present the explanation and its visuals as one seamless answer.";

/// The Q&A system prompt — embeds the brand name, so it derives from the one constant.
fn qa_system() -> String {
    format!(
        "You are the AI assistant embedded in {}, a developer terminal. {TEACHER} \
The user's recent terminal context (with secrets redacted) may be provided for grounding — use it when relevant but do not echo it back verbatim.",
        corelib::brand::NAME
    )
}

/// The `@ai` system prompt: a tiny, STREAMABLE contract. Either propose a shell command with a
/// one-line `RUN:` header (the terminal preloads it for the user to edit/run), or just answer —
/// and the answer streams and renders live. Default (no `RUN:`) is an answer, so an off-contract
/// reply is always shown safely rather than run.
fn command_system() -> String {
    format!(
        "You are the AI at {}'s terminal. The user typed `@ai <request>`. Choose ONE:\n\
         - If a single shell command accomplishes it, reply with EXACTLY one line: `RUN: <command>` \
         and nothing else (the user reviews, edits, and runs it). Quote URLs/globs so the shell \
         won't expand them, e.g. `RUN: curl -s 'https://wttr.in/Paris?format=3'`.\n\
         - Otherwise, answer the request. {TEACHER}\n\
         Never write `RUN:` unless the whole reply is that one command line. Do not run anything yourself.",
        corelib::brand::NAME
    )
}

/// The one-line prefix that marks the `@ai` reply as a shell command to propose.
pub const RUN_PREFIX: &str = "RUN:";

fn user_message(context: &str, body: &str) -> Vec<Message> {
    let content = if context.trim().is_empty() {
        body.to_string()
    } else {
        format!("{context}\n\n{body}")
    };
    vec![Message::user(content)]
}

/// Build a Q&A request (Markdown answer) on the chosen primary `model`. Sampling
/// params come from the model's definition (per-entry config overrides are already
/// folded into `model` by the pool when it was chosen).
pub fn qa_request(model: &ModelDef, prompt: &str, context: &str) -> ChatRequest {
    ChatRequest {
        model: model.id.clone(),
        max_tokens: model.max_tokens,
        system: Some(qa_system()),
        messages: user_message(context, prompt),
        temperature: model.temperature,
        top_p: model.top_p,
        top_k: model.top_k,
        thinking: model.caps.enable_thinking,
        images: Vec::new(),
        cache: CacheHints::none(),
    }
}

/// Build an **agent turn's** request: the agent's own system prompt, and the run's
/// role-tagged conversation.
///
/// Deliberately not [`qa_request`]. That one carries the teacher persona — *"use a
/// diagram whenever a picture makes the idea clearer"* — which is right for `@ai` and
/// actively wrong for a tool-calling agent, whose instructions would then be
/// competing with the system slot for the model's attention. An agent's prompt IS the
/// system prompt.
pub fn agent_request(model: &ModelDef, system: &str, messages: Vec<Message>) -> ChatRequest {
    // The ONE place a turn declares what is settled. Every later turn of the run re-sends
    // this same system block and these same earlier messages, so a provider that can
    // reuse them should be told — and told here, where the fact is known, rather than
    // guessed at by each adapter.
    let cache = CacheHints::for_turn(messages.len());
    ChatRequest {
        model: model.id.clone(),
        max_tokens: model.max_tokens,
        system: (!system.trim().is_empty()).then(|| system.to_string()),
        messages,
        temperature: model.temperature,
        top_p: model.top_p,
        top_k: model.top_k,
        thinking: model.caps.enable_thinking,
        images: Vec::new(),
        cache,
    }
}

/// Build a natural-language → command request. Deterministic (temperature 0), no thinking;
/// room for a short prose answer when the request is a question.
pub fn command_request(model: &ModelDef, nl: &str, context: &str) -> ChatRequest {
    ChatRequest {
        model: model.id.clone(),
        max_tokens: 2048,
        system: Some(command_system()),
        messages: user_message(context, &format!("Request: {nl}")),
        temperature: Some(0.0),
        top_p: None,
        top_k: None,
        thinking: false,
        images: Vec::new(),
        cache: CacheHints::none(),
    }
}

#[cfg(test)]
mod tests {
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
}
