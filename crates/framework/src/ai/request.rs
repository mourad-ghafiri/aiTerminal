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

/// The Q&A system prompt — embeds the brand name, so it derives from the one constant.
fn qa_system() -> String {
    format!(
        "You are the AI assistant embedded in {}, a developer terminal. \
Answer concisely and accurately in GitHub-flavored Markdown (use fenced code blocks for commands and code). \
The user's recent terminal context (with secrets redacted) may be provided for grounding — use it when relevant but do not echo it back verbatim.",
        corelib::brand::NAME
    )
}

const COMMAND_SYSTEM: &str = "You translate a natural-language request into a single shell command for the user's platform. \
Output ONLY the command, on the first line, with no Markdown fences and no prose. \
If the request is unsafe, ambiguous, or impossible, output a single line beginning with '# ' that briefly explains why.";


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

/// Build a natural-language → command request on the fast `model`. Commands are
/// short + deterministic (temperature 0) and never use thinking.
pub fn command_request(model: &ModelDef, nl: &str, context: &str) -> ChatRequest {
    ChatRequest {
        model: model.id.clone(),
        max_tokens: 512,
        system: Some(COMMAND_SYSTEM.to_string()),
        messages: user_message(context, &format!("Request: {nl}")),
        // Commands are deterministic: the fast model's creative temperature is
        // intentionally NOT used here.
        temperature: Some(0.0),
        top_p: None,
        top_k: None,
        thinking: false,
        images: Vec::new(),
    }
}


/// Extract the first runnable command line from a streamed command answer
/// (skips blank lines and `# `-prefixed refusals/explanations).
pub fn first_command_line(full: &str) -> Option<String> {
    full.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.trim_start_matches("$ ").to_string())
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
    fn command_request_uses_the_pool_model_and_is_deterministic() {
        let s = AiSettings::default();
        let m = s.choose();
        let req = command_request(&m, "list files", "");
        assert_eq!(req.model, m.id);
        assert_eq!(req.temperature, Some(0.0));
        assert!(req.messages[0].content.contains("Request: list files"));
    }

    

    #[test]
    fn context_is_prepended_to_prompt() {
        let req = qa_request(&AiSettings::default().choose(), "why?", "ctx-block");
        let content = &req.messages[0].content;
        assert!(content.starts_with("ctx-block"));
        assert!(content.contains("why?"));
    }

    #[test]
    fn first_command_line_skips_refusals() {
        assert_eq!(first_command_line("# unsafe\nrm -rf /"), Some("rm -rf /".into()));
        assert_eq!(first_command_line("$ ls -la\n"), Some("ls -la".into()));
        assert_eq!(first_command_line("# nope"), None);
    }
}
