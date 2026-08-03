//! Writing the pool — one model call, in a detached process, at most every few weeks.
//!
//! A detached **process**, and that word is the fix. This used to be a detached thread,
//! and a thread dies with the process that spawned it: the CLI exits the moment a run
//! finishes, which is almost always before a model call completes — so the pool was
//! never written, the muse stayed mute forever, and every run re-spawned a thread whose
//! fate was already decided. Confirmed the hard way: a machine with weeks of AI use and
//! no `cache/` directory at all.
//!
//! So the refill is now this binary re-invoked and detached into its own session — the
//! exact shape a `--bg` job takes — and it survives the run that asked for it. Nothing
//! waits for it. The run that triggered it uses whatever was already on disk; the next
//! one gets the benefit. A decoration that could delay an answer would have stopped
//! being a decoration.
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

/// How long one attempt is given before another run may try again. Long enough for any
/// model call; short enough that a child that died (a laptop lid, a crash) is not a
/// feature stuck off for a day.
const ATTEMPT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Ask for the pool, in a detached child. Returns immediately.
pub(crate) fn in_background(cfg: &crate::config::Config) {
    if cfg.ai_settings().resolve_key().is_none() {
        return; // no model, no lines, nothing said about it
    }
    // One attempt at a time: every run whose pool is thin lands here, and without the
    // stamp each would launch its own child until the first one finished.
    let stamp = super::path().with_file_name("refill.stamp");
    if attempted_recently(&stamp) {
        return;
    }
    if let Some(parent) = stamp.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&stamp, b"");
    let (Ok(exe), Ok(out), Ok(err)) = (
        std::env::current_exe(),
        std::fs::OpenOptions::new().write(true).open("/dev/null"),
        std::fs::OpenOptions::new().write(true).open("/dev/null"),
    ) else {
        return; // a decoration never reports its own failure
    };
    let _ = platform::os::spawn_detached(&exe, &["ai".into(), "refill-motivation".into()], out, err);
}

/// Whether an attempt is already in flight (or recently died) — the stamp's mtime is
/// inside [`ATTEMPT`]. Its own function so the suppression window is a fact a test can
/// state with a file in a temp dir, rather than behaviour that needs a spawned child to
/// observe.
pub(crate) fn attempted_recently(stamp: &std::path::Path) -> bool {
    std::fs::metadata(stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < ATTEMPT)
}

/// The child's whole life: one model call, one file, exit. Runs in a process of its own
/// so no foreground run ever waits for it — and so it survives the one that asked.
pub(crate) fn run_now() -> i32 {
    let cfg = crate::config::Config::load();
    let settings = cfg.ai_settings();
    if settings.resolve_key().is_none() {
        return 0;
    }
    let client = crate::ai::Client::new(settings, crate::ai::CurlTransport::default());
    if let Some(pool) = write_with(&client, &cfg.motivation().kinds, crate::flowruns::now()) {
        pool.save();
    }
    0
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
