//! The job planner: one model call that reads a request the way a person wrote it and
//! answers *when* to run and *what* to run.
//!
//! `@job "check the logs at midnight"` should mean what it says, and no hand-written word
//! matcher ever finishes that job — "tonight", "each weekday morning", "on the 1st",
//! "every 15 minutes past the hour" are endless. So the model reads the sentence **once, at
//! creation**, and writes its answer into the record as a cron expression (or an interval,
//! or an absolute time). Every occurrence after that is arithmetic: run #47 of an hourly
//! job never consults a model, and behaves exactly like run #1.
//!
//! The contract is deliberately tiny and strictly decoded — one JSON object, no prose, or
//! the caller falls back to the deterministic parser.

use crate::jobs::{Cmd, Cron, Schedule};

/// What the planner understood.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan {
    /// When to run; `None` means now, once.
    pub(crate) schedule: Option<Schedule>,
    /// The task for an agent, with the timing words removed.
    pub(crate) task: String,
    /// Set when the request names a command to run instead of an agent task.
    pub(crate) cmd: Option<Cmd>,
    /// One line a person can check at a glance: `every day at 00:00 — check the logs`.
    pub(crate) says: String,
}

/// The reply we ask for — one JSON object and nothing else.
fn system_prompt(now_local: &str, offset_hours: f64) -> String {
    format!(
        "You turn a user's request into a scheduled job. Reply with ONE JSON object and nothing else \
         — no prose, no code fence.\n\
         Local time is {now_local} (UTC{offset_hours:+}).\n\n\
         {{\"when\":{{\"kind\":\"now\"|\"once\"|\"every\"|\"cron\", \"in_seconds\":N, \"every_seconds\":N, \"cron\":\"m h dom mon dow\"}},\n\
         \"task\":\"what to do, with the timing words removed\",\n\
         \"command\":\"a shell command, ONLY if the user clearly asked to run one\",\n\
         \"says\":\"a short human sentence: when — what\"}}\n\n\
         Rules:\n\
         - Clock-anchored repeats are CRON: \"at midnight\" -> \"0 0 * * *\"; \"weekdays at 6pm\" -> \"0 18 * * 1-5\"; \
         \"every monday at 8\" -> \"0 8 * * 1\"; \"the 1st at 3am\" -> \"0 3 1 * *\".\n\
         - Plain repeats with no clock are `every` with `every_seconds`: \"every 15 minutes\" -> 900.\n\
         - A single future moment is `once` with `in_seconds` from now.\n\
         - No timing words at all -> {{\"kind\":\"now\"}}.\n\
         - Set `command` only for an explicit command (\"run ./backup.sh\", \"execute make test\"). \
         Anything descriptive (\"summarize the logs\", \"check for errors\") is a `task` for an agent, not a command.\n\
         - `task` must not contain the timing words; `says` must state the schedule in plain words."
    )
}

/// What came of asking the model to read a request.
///
/// Three answers, not two. This used to be an `Option<Plan>`, where `None` meant both
/// "no model is configured" — instant, and nothing was ever promised — and "it was asked
/// and could not answer", which is a model round trip somebody sat through for nothing.
/// The caller falls back to the word parser either way, but only one of them is worth
/// saying out loud, and with one value it could not tell which had happened.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Reading {
    /// Nothing was asked: no model, or nothing to read.
    Unasked,
    /// It was asked, and what came back was not a plan.
    Unread,
    Read(Plan),
}

/// Ask the model to read `request`.
///
/// Blocking, and the only step in creating a job that takes longer than a millisecond —
/// so the caller runs it under [`waiting_on`](crate::cli::observe::waiting_on) rather than
/// leaving the terminal dead until it returns.
pub(crate) fn read_request(request: &str, now: u64) -> Reading {
    if request.trim().is_empty() {
        return Reading::Unasked;
    }
    let cfg = crate::config::Config::load();
    let settings = cfg.ai_settings();
    if settings.resolve_key().is_none() {
        return Reading::Unasked;
    }
    let client = crate::ai::Client::new(settings, crate::ai::CurlTransport::default());
    read_with(&client, request, now)
}

/// The planner against a given client — the seam tests and scenarios drive with a scripted
/// transport, so the request travels the real wire format and comes back through the real
/// decoder.
///
/// It returns [`Reading::Unread`] rather than nothing when the call fails or the reply is
/// not a plan: **this** is the function that knows a call was made, and a caller told only
/// "no plan" cannot tell that from having asked nobody.
pub(crate) fn read_with<T: platform::transport::Transport>(
    client: &crate::ai::Client<T>,
    request: &str,
    now: u64,
) -> Reading {
    let offset = platform::os::utc_offset_secs();
    let stamp = corelib::datetime::format(now as i64, "%Y-%m-%d %H:%M", offset);
    let model = client.model().clone();
    let req = crate::ai::ChatRequest {
        model: model.id.clone(),
        max_tokens: 512,
        system: Some(system_prompt(&stamp, offset as f64 / 3600.0)),
        messages: vec![crate::ai::Message::user(format!("Request: {request}"))],
        temperature: Some(0.0),
        top_p: None,
        top_k: None,
        thinking: false,
        images: Vec::new(),
        // One question, asked once — there is no later turn to reuse anything.
        cache: crate::ai::CacheHints::none(),
    };
    let Ok(reply) = client.complete(&req) else { return Reading::Unread };
    match decode(&reply, request, now) {
        Some(plan) => Reading::Read(plan),
        None => Reading::Unread,
    }
}

/// Decode the model's reply into a plan. Strict: the object must be readable and its
/// schedule must make sense, or this is `None` and the caller falls back.
pub(crate) fn decode(reply: &str, request: &str, now: u64) -> Option<Plan> {
    let json = extract_object(reply)?;
    let doc = corelib::wire::Json::parse(&json).ok()?;
    let when = doc.get("when")?;
    let kind = when.get("kind").and_then(|v| v.as_str()).unwrap_or("now");
    let num = |v: Option<&corelib::wire::Json>| v.and_then(|v| v.as_f64()).filter(|n| n.is_finite() && *n >= 0.0);
    let schedule = match kind {
        "cron" => {
            let expr = when.get("cron").and_then(|v| v.as_str())?;
            Some(Schedule::Cron(Cron::parse(expr)?))
        }
        "every" => {
            // A repeat faster than a minute is a mistake, not a schedule.
            let secs = num(when.get("every_seconds"))? as u64;
            (secs >= 60).then_some(Schedule::Every(secs))?.into()
        }
        "once" => {
            let secs = num(when.get("in_seconds"))? as u64;
            Some(Schedule::Once(now + secs.max(1)))
        }
        _ => None,
    };
    // The task falls back to the original request rather than being dropped.
    let task = doc
        .get("task")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(request)
        .to_string();
    let cmd = doc
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Cmd::Line(s.to_string()));
    let says = doc
        .get("says")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match &schedule {
            Some(s) => format!("{} \u{2014} {task}", s.describe()),
            None => format!("now \u{2014} {task}"),
        });
    Some(Plan { schedule, task, cmd, says })
}

/// The first balanced `{ … }` in a reply — models like to wrap JSON in a fence or a
/// sentence, and the object is the only part that matters.
pub(crate) fn extract_object(reply: &str) -> Option<String> {
    extract_balanced(reply, '{', '}')
}

/// The first balanced `[ … ]` in a reply.
///
/// A `map` node splits an upstream answer into items, and an agent asked for a list
/// tends to deliver it with a sentence of introduction. Without this, that sentence
/// becomes an item and gets its own agent run.
pub(crate) fn extract_array(reply: &str) -> Option<String> {
    extract_balanced(reply, '[', ']')
}

fn extract_balanced(reply: &str, open: char, close: char) -> Option<String> {
    let bytes = reply.as_bytes();
    let start = reply.find(open)?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(reply[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests;
