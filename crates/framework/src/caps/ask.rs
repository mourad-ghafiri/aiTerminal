//! The `ask.*` native family — the model asks the HUMAN a question and waits for
//! the answer. The interaction seam, as a tool: instead of guessing at an
//! ambiguous requirement, the agent asks; the surface presents the question and
//! the person's words come back as the tool's result.
//!
//! Headless runs have nobody to ask — the default [`Asker`] refuses, exactly as
//! the guard's `Confirm` refuses without an approver. The runner's egress hide
//! redacts the answer like every human-text path.

use corelib::wire::Json;

use super::host::Host;
use super::object::{MethodSpec, NativeObject};
use super::CapCtx;

/// Who answers a model's question — the workspace wires a real person in;
/// everything else keeps [`NobodyToAnswer`].
pub trait Asker: Send + Sync {
    /// Put `question` to the human. `None` = declined, or nobody is there.
    fn ask(&self, question: &str) -> Option<String>;
}

/// The default: no human present — the tool reports that honestly.
pub struct NobodyToAnswer;

impl Asker for NobodyToAnswer {
    fn ask(&self, _question: &str) -> Option<String> {
        None
    }
}

pub struct AskObj;

const SPECS: &[MethodSpec] = &[MethodSpec {
    method: "ask.user",
    describe: "Ask the person a question and wait for their answer — use it when a requirement is genuinely ambiguous",
}];

impl NativeObject for AskObj {
    fn family(&self) -> &'static str {
        "ask"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        if method != "ask.user" {
            return Err(format!("unknown ask method '{method}'"));
        }
        let question = args.iter().find(|(k, _)| k == "question").map(|(_, v)| v.trim()).unwrap_or("");
        if question.is_empty() {
            return Err("ask.user needs a question".into());
        }
        match ctx.asker.ask(question) {
            Some(answer) => Ok(Json::Str(answer)),
            None => Err("nobody answered \u{2014} the user declined, or no one is here to ask".into()),
        }
    }
}

#[cfg(test)]
mod tests;
