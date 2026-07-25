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
}

impl ChatRequest {
    /// Attach vision images to the request (the host gates on the model's `enable_vision`).
    pub fn with_images(mut self, images: Vec<ImageData>) -> Self {
        self.images = images;
        self
    }
}

/// The teacher-persona guidance shared by the `@ai` answer and Q&A prompts: explain like a
/// great, concise teacher and lean on visual diagrams — WITHOUT exposing any of the underlying
/// formatting/diagram technology to the user.
const TEACHER: &str = "Explain like a brilliant, concise teacher: lead with the answer, keep it \
tight, and make it click. **Use a diagram whenever a picture makes the idea clearer** — draw it \
in a fenced ```mermaid code block (flowchart or sequenceDiagram). This terminal renders your text \
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

    #[test]
    fn context_is_prepended_to_prompt() {
        let req = qa_request(&AiSettings::default().choose(), "why?", "ctx-block");
        let content = &req.messages[0].content;
        assert!(content.starts_with("ctx-block"));
        assert!(content.contains("why?"));
    }
}
