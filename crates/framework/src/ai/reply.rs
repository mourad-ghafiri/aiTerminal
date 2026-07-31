//! The `@ai --command` reply contract: is this a command to run, or an answer to read?
//!
//! `@ai` asks for a deliberately tiny, STREAMABLE reply: either a single line beginning
//! `RUN:` — a command, which the terminal guards and preloads at your prompt — or prose,
//! which renders live as it arrives. Getting this wrong in either direction is bad in a
//! way users notice: a misread answer preloads a nonsense command, and a misread command
//! prints `RUN: rm …` as if it were advice.
//!
//! The hard part is that the decision must be made from the *first few characters*, while
//! the rest is still streaming, so an answer can start rendering block-by-block instead of
//! appearing all at once when the model finishes. So the classifier holds back only the
//! undecided prefix, and hands everything after the decision straight to the sink.
//!
//! This is the protocol, not the presentation: what the reply *is* lives here, where the
//! reply is *shown* lives in the caller's [`ReplySink`]. The CLI renders Markdown to a
//! terminal; a scenario records strings.

use super::{StreamEvent, RUN_PREFIX};

/// What the model actually replied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandReply {
    /// A command to put at the user's prompt, already reduced to the single line the
    /// contract allows. A model that keeps talking after the command does not get to
    /// smuggle a second line into the shell.
    Command(String),
    /// Prose. It was streamed to the sink as it arrived, so there is nothing to carry.
    Answer,
    /// The model said nothing at all — an empty stream, or only whitespace.
    ///
    /// Distinct from [`Answer`](Self::Answer) because an empty answer is invisible: the
    /// caller renders nothing and preloads nothing, so the user who asked for a command
    /// gets a bare prompt back and cannot tell the request from a no-op.
    Empty,
    /// The stream failed. Nothing was rendered, and no command was proposed.
    Failed(String),
}

/// A classified reply, with the usage the footer reports.
#[derive(Debug, Clone)]
pub struct Classified {
    pub reply: CommandReply,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Where a streaming reply's live output goes.
///
/// Default methods mean a caller that only wants the answer text implements one method.
pub trait ReplySink {
    /// Prose, as it streams. Called only once the reply is known to be an answer.
    fn answer(&mut self, text: &str);
    /// The model's reasoning. Called for every reasoning chunk; a sink that hides
    /// reasoning simply drops it, which is the default.
    fn thinking(&mut self, _text: &str) {}
}

/// Read the stream, decide what it is, and stream an answer to `sink` as it arrives.
pub fn classify_command_reply(events: impl Iterator<Item = StreamEvent>, sink: &mut dyn ReplySink) -> Classified {
    // `None` while the reply could still turn out to be either.
    let mut decided: Option<Decision> = None;
    let mut head = String::new();
    let mut command = String::new();
    let (mut input_tokens, mut output_tokens) = (0, 0);

    for ev in events {
        match ev {
            StreamEvent::Delta(s) => {
                match decided {
                    Some(Decision::Command) => command.push_str(&s),
                    Some(Decision::Answer) => sink.answer(&s),
                    None => {
                        head.push_str(&s);
                        if let Some(d) = decide(&head) {
                            decided = Some(d);
                            match d {
                                Decision::Command => command = strip_prefix(&head),
                                Decision::Answer => sink.answer(&head),
                            }
                            head.clear();
                        }
                    }
                }
            }
            StreamEvent::Thinking(t) => sink.thinking(&t),
            StreamEvent::Done { input_tokens: i, output_tokens: o, .. } => {
                input_tokens = i;
                output_tokens = o;
                break;
            }
            StreamEvent::Error(e) => {
                return Classified { reply: CommandReply::Failed(e), input_tokens, output_tokens }
            }
        }
    }

    // A reply short enough that it ended while still undecided — classify what we held.
    let reply = match decided {
        Some(Decision::Command) => CommandReply::Command(first_line(&command)),
        Some(Decision::Answer) => CommandReply::Answer,
        None if is_command(&head) => CommandReply::Command(first_line(&strip_prefix(&head))),
        // Whitespace never decides, so anything blank ends up here.
        None if head.trim().is_empty() => CommandReply::Empty,
        None => {
            sink.answer(&head);
            CommandReply::Answer
        }
    };
    Classified { reply, input_tokens, output_tokens }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Command,
    Answer,
}

/// The decision, or `None` while the prefix is still consistent with `RUN:`.
fn decide(head: &str) -> Option<Decision> {
    let h = normalized(head);
    if h.len() < RUN_PREFIX.len() && RUN_PREFIX.starts_with(&h) {
        return None; // still might be `RUN:` — hold the prefix back
    }
    Some(if h.starts_with(RUN_PREFIX) { Decision::Command } else { Decision::Answer })
}

fn is_command(head: &str) -> bool {
    normalized(head).starts_with(RUN_PREFIX)
}

/// The marker is matched case-insensitively and after leading space, because models are
/// inconsistent about both and a lowercase `run:` is unmistakably the same intent.
fn normalized(head: &str) -> String {
    head.trim_start().to_ascii_uppercase()
}

/// `RUN_PREFIX` is ASCII, so the byte offset is also the char offset.
fn strip_prefix(head: &str) -> String {
    head.trim_start()[RUN_PREFIX.len()..].to_string()
}

fn first_line(command: &str) -> String {
    command.trim().lines().next().unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests;
