//! The **transcript** — an agent run's conversation, as structure rather than text.
//!
//! A run used to be one growing `String` with `user:` / `assistant:` / `tool_result:`
//! typed into it as plain words, handed to the provider as a single user message.
//! Every provider adapter has always encoded `Vec<Message>` with real roles
//! (`ChatRequest.messages`); the harness simply never sent more than one. Role
//! separation is the strongest structural signal a model gets about who said what,
//! and the weaker the model, the more of the work that signal does.
//!
//! So the conversation is a `Vec<Turn>` and [`messages`](Transcript::messages)
//! renders it. Two consequences fall out for free:
//!
//! - the agent's own system prompt goes in the **system slot**, where instructions
//!   belong, instead of being narrated inside user text;
//! - the turns are **append-only**, so a provider's prefix cache keeps matching as a
//!   run grows. (Compaction rewrites history and gives that up — which is exactly
//!   why it is a last resort rather than a per-turn tidy.)

use crate::ai::budget::TokenEstimator;
use crate::ai::request::{Message, Role};

/// One turn in a run.
#[derive(Clone, Debug, PartialEq)]
pub enum Turn {
    /// Something said TO the model: the task, a correction, a compaction summary.
    User(String),
    /// The model's own words — including the `@tool` line, so the record of what it
    /// asked for survives verbatim.
    Assistant(String),
    /// What a tool returned. Tainted text: it is data the model reads, never an
    /// instruction it obeys, and it is labelled as such on the wire.
    ToolResult { name: String, text: String },
}

impl Turn {
    /// The turn's text, whatever kind it is — for measuring and searching.
    pub fn text(&self) -> &str {
        match self {
            Turn::User(t) | Turn::Assistant(t) => t,
            Turn::ToolResult { text, .. } => text,
        }
    }

    /// How this turn appears on the wire.
    fn wire(&self) -> (Role, String) {
        match self {
            Turn::User(t) => (Role::User, t.clone()),
            Turn::Assistant(t) => (Role::Assistant, t.clone()),
            // No provider-neutral `tool` role exists in `ChatRequest`, and inventing
            // one would mean touching every adapter for no gain: a labelled user turn
            // reads identically to the model. The label is what keeps a tool's output
            // from being mistaken for the user's own words.
            Turn::ToolResult { name, text } => (Role::User, format!("tool_result({name}):\n{text}")),
        }
    }
}

/// An agent run's conversation.
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    /// The system prompt: the agent's instructions plus how to call a tool. Sent in
    /// the request's system slot, never as conversation.
    system: String,
    turns: Vec<Turn>,
}

impl Transcript {
    /// Start a run: the system prompt, then the user's task.
    pub fn new(system: impl Into<String>, task: impl Into<String>) -> Transcript {
        Transcript { system: system.into(), turns: vec![Turn::User(task.into())] }
    }

    pub fn system(&self) -> &str {
        &self.system
    }

    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    pub fn len(&self) -> usize {
        self.turns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    pub fn push(&mut self, turn: Turn) {
        self.turns.push(turn);
    }

    /// Cut the conversation at `at` and hand back everything after it — how a
    /// sitting's `/undo` removes its last exchange (and `/redo` re-pushes it).
    /// An out-of-range index removes nothing, for [`replace_span`]'s reason.
    pub fn split_off(&mut self, at: usize) -> Vec<Turn> {
        if at >= self.turns.len() {
            return Vec::new();
        }
        self.turns.split_off(at)
    }

    /// Replace a span of turns with one turn — how compaction folds history down.
    /// Out-of-range spans are ignored rather than panicking: a compaction stage
    /// working from a stale measurement must never take down the run it was trying
    /// to rescue.
    pub fn replace_span(&mut self, from: usize, to: usize, with: Turn) {
        if from >= to || to > self.turns.len() {
            return;
        }
        self.turns.splice(from..to, std::iter::once(with));
    }

    /// Rewrite one turn in place. Ignores an out-of-range index, for the same reason
    /// as [`replace_span`](Self::replace_span).
    pub fn replace(&mut self, at: usize, with: Turn) {
        if let Some(slot) = self.turns.get_mut(at) {
            *slot = with;
        }
    }

    /// The wire form: role-tagged messages, consecutive same-role turns merged.
    ///
    /// The merge is not cosmetic — several providers reject two user messages in a
    /// row, and a tool result followed by a correction produces exactly that. Merging
    /// here means no adapter needs to know.
    pub fn messages(&self) -> Vec<Message> {
        let mut out: Vec<Message> = Vec::with_capacity(self.turns.len());
        for turn in &self.turns {
            let (role, content) = turn.wire();
            match out.last_mut() {
                Some(prev) if prev.role == role => {
                    prev.content.push_str("\n\n");
                    prev.content.push_str(&content);
                }
                _ => out.push(Message { role, content }),
            }
        }
        out
    }

    /// Approximate tokens for the whole prompt — system plus every turn, including
    /// the per-message framing a provider adds around each one.
    pub fn tokens(&self, est: &dyn TokenEstimator) -> usize {
        /// Roughly what a provider spends framing one message (role tag, delimiters).
        const PER_MESSAGE_OVERHEAD: usize = 4;
        let body: usize = self.turns.iter().map(|t| est.estimate(t.text()) + PER_MESSAGE_OVERHEAD).sum();
        est.estimate(&self.system) + body
    }
}

#[cfg(test)]
mod tests;
