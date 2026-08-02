//! Writing the pool — one model call, in the background, at most every few weeks.
//!
//! It runs on a **detached thread** and nothing waits for it. The run that triggered it
//! uses whatever was already on disk; the next one gets the benefit. A decoration that
//! could delay an answer would have stopped being a decoration.
//!
//! Tips are the interesting half. A model does not know this tool, so asking it for
//! "tips about aiTerminal" invents commands that do not exist — a line that teaches you
//! something false is worse than no line. It is handed the **real usage text**, the same
//! strings `@flow help` prints and the verb gate reads, so a tip is drawn from what the
//! commands actually are.

use super::{Kind, Line, Pool, MAX_LEN};

/// How many lines one call writes. Enough that a long session does not come round twice;
/// few enough to be one small reply.
const WANTED: usize = 24;

/// Ask for the pool, in the background. Returns immediately.
pub(crate) fn in_background(cfg: &crate::config::Config) {
    let settings = cfg.ai_settings();
    if settings.resolve_key().is_none() {
        return; // no model, no lines, nothing said about it
    }
    let kinds: Vec<Kind> = cfg.motivation().kinds;
    std::thread::spawn(move || {
        let client = crate::ai::Client::new(settings, crate::ai::CurlTransport::default());
        if let Some(pool) = write_with(&client, &kinds, crate::flowruns::now()) {
            pool.save();
        }
    });
}

/// One call, decoded. `None` when the call fails or nothing usable comes back — in which
/// case the old pool stays exactly as it was, which is better than an empty one.
pub(crate) fn write_with<T: platform::transport::Transport>(
    client: &crate::ai::Client<T>,
    kinds: &[Kind],
    now: u64,
) -> Option<Pool> {
    if kinds.is_empty() {
        return None;
    }
    let req = crate::ai::ChatRequest {
        model: client.model().id.clone(),
        max_tokens: 1500,
        system: Some(contract(kinds)),
        messages: vec![crate::ai::Message::user(format!(
            "Write {WANTED} lines. Here is what this tool's commands actually are, so the tips are true:\n\n{}",
            surface()
        ))],
        // The one place in this product that wants variety rather than repeatability.
        temperature: Some(1.0),
        top_p: None,
        top_k: None,
        thinking: false,
        images: Vec::new(),
        cache: crate::ai::CacheHints::none(),
    };
    let reply = client.complete(&req).ok()?;
    let lines = decode(&reply, kinds);
    (!lines.is_empty()).then_some(Pool { lines, written: now })
}

/// What is asked for.
fn contract(kinds: &[Kind]) -> String {
    let mut s = String::from(
        "You write single lines shown to somebody waiting a few seconds for an AI to answer, \
         inside a terminal. Reply with ONE JSON array and nothing else — no prose, no code fence.\n\n\
         [{\"kind\":\"tips\",\"text\":\"…\"}, …]\n\n\
         Rules:\n\
         - Each `text` is ONE line, at most 70 characters. Longer ones are thrown away.\n\
         - No trailing full stop, no emoji, no markdown.\n\
         - Say something. A line that could appear in any product is a wasted line.\n\
         - Vary the kinds evenly across the list.\n\n\
         The kinds:\n",
    );
    for k in kinds {
        s.push_str(&match k {
            Kind::Tip => "- tips: something true about THIS tool, drawn from the command list below. \
                          Never invent a command or a flag.\n",
            Kind::Fact => "- facts: a real, checkable fact about software, computing or language models. \
                           Concrete beats grand.\n",
            Kind::Quote => "- quotes: a short real quote about building things or thinking clearly, \
                            with its author after an em dash.\n",
            Kind::Cheer => "- encouragement: brief and plain. No exclamation marks, no hustle.\n",
        });
    }
    s
}

/// The commands, as the tool itself documents them.
///
/// The same usage strings the help prints — so a tip is about what is really there
/// rather than about what a model imagines a terminal would have.
fn surface() -> String {
    [
        crate::cli::flow::args::flow_usage(),
        crate::cli::agentloop::args::loop_usage(),
        crate::cli::jobs::create::job_usage(),
    ]
    .join("\n")
}

/// Read the reply. Anything that is not a usable line is dropped rather than repaired:
/// there are twenty-odd of them and losing a few costs nothing.
pub(crate) fn decode(reply: &str, kinds: &[Kind]) -> Vec<Line> {
    let Some(json) = crate::ai::plan::extract_array(reply) else { return Vec::new() };
    let Ok(doc) = corelib::wire::Json::parse(&json) else { return Vec::new() };
    let Some(items) = doc.as_array() else { return Vec::new() };
    let mut out: Vec<Line> = Vec::new();
    for item in items {
        let Some(kind) = item.get("kind").and_then(|v| v.as_str()).and_then(Kind::read) else { continue };
        // A kind nobody asked for is dropped here rather than at display time, so the
        // cached pool is exactly what the config says it should be.
        if !kinds.contains(&kind) {
            continue;
        }
        let Some(text) = item.get("text").and_then(|v| v.as_str()) else { continue };
        let Some(line) = Line::new(kind, text) else { continue };
        // The same line twice is the one thing a rotation cannot hide.
        if out.iter().any(|l| l.text.eq_ignore_ascii_case(&line.text)) {
            continue;
        }
        out.push(line);
    }
    let _ = MAX_LEN; // enforced by `Line::new`, named here so the tie is obvious
    out
}
