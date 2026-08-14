//! The `workspace` world: a whole `@workspace` sitting, hermetically.
//!
//! The REAL pipeline end to end — trust gate, project overlay, tightened guard,
//! the REPL's router, the persistent transcript, the tool runner — driven through
//! its seams: typed lines are a script, the model is a scripted transport, and the
//! home is the suite's locked test home. No PTY, no network, no raw mode; what a
//! scenario asserts on is what actually crossed each boundary (the request bodies
//! posted, the files written, the questions the trust gate asked).

use corelib::wire::Toml;

use crate::ai;
use crate::cli::workspace::input::ScriptedLines;
use crate::cli::workspace::repl::Repl;
use crate::cli::workspace::trust::{establish, Trust};
use crate::config::overlay::Workspace;
use crate::scenario::world::{self, World};
use platform::transport::ScriptedTransport;

pub struct WorkspaceWorld {
    _home: crate::test_home::HomeGuard,
    /// The project being opened, inside the test home.
    root: std::path::PathBuf,
    /// Scripted model turns (SSE fixtures), consumed in order across the sitting.
    turns: Vec<String>,
    /// Scripted input lines — the conversation AND any approval answers, in order.
    lines: Vec<String>,
    /// What the trust gate is answered with.
    trust_answer: bool,
    /// Every question the trust gate asked.
    asked: Vec<String>,
    /// Every request body the sitting posted, oldest first.
    sent: Vec<String>,
    /// Whether the overlay ended up live.
    overlaid: Option<bool>,
    session_dir: std::path::PathBuf,
}

pub fn build(setup: &Toml) -> Result<Box<dyn World>, String> {
    let tag = world::text(setup, "home").unwrap_or_else(|| "workspace".into());
    let (guard, home) = crate::test_home::lock_home(&format!("scenario-{tag}"));
    crate::config::Config::ensure_default();
    let root = home.join("project");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let session = ai::Session::at(&root, &crate::config::Config::sessions_dir());
    let session_dir = session.memory_dir().parent().map(|p| p.to_path_buf()).ok_or("no session dir")?;
    Ok(Box::new(WorkspaceWorld {
        _home: guard,
        root,
        turns: Vec::new(),
        lines: Vec::new(),
        trust_answer: world::text(setup, "trust").map(|t| t == "y").unwrap_or(true),
        asked: Vec::new(),
        sent: Vec::new(),
        overlaid: None,
        session_dir,
    }))
}

fn settings() -> ai::AiSettings {
    let mut model = ai::provider::builtin_default().resolve("claude-opus-4-8");
    model.api_key = Some("scenario-key-never-sent".into());
    ai::AiSettings { pool: ai::ModelPool::single(model) }
}

impl WorkspaceWorld {
    /// One whole sitting: trust, overlay, REPL over the scripted lines, to the end.
    fn open(&mut self) -> Result<(), String> {
        let mut answer = self.trust_answer;
        let mut asked = Vec::new();
        let mut ask = |q: &str| {
            asked.push(q.to_string());
            answer
        };
        let granted = establish(&self.root, &self.session_dir, &mut ask) == Trust::Granted;
        let _ = &mut answer;
        self.asked.extend(asked);

        let ws = Workspace::open(&self.root, granted);
        self.overlaid = Some(ws.overlaid());
        let base = crate::config::Config::load();
        let cfg = ws.config(&base);
        let registry = crate::plugin::load_registry(&cfg);
        let guard = crate::guard::build_with_project(&cfg, &registry, ws.project_rules().as_ref());
        let guard = std::sync::Arc::new(guard.at(Some(self.root.clone())));
        // No MCP hub in a scenario: nothing may spawn. The mcp declarations still
        // COUNT for the trust prompt, which is exactly the boundary under test.
        let runner = crate::cli::runner::build_runner(&cfg, &settings(), Some(self.root.clone()), guard.clone(), None);
        let client = ai::Client::new(settings(), ScriptedTransport::new(self.turns.clone()));
        let input: crate::cli::workspace::repl::SharedInput =
            std::sync::Arc::new(std::sync::Mutex::new(Box::new(ScriptedLines::new(self.lines.clone()))));
        let mut repl = Repl::new(ws, cfg, settings(), client, guard, runner, input, Some(self.session_dir.clone()));
        let code = repl.drive();
        if code != 0 {
            return Err(format!("the sitting exited {code}"));
        }
        self.sent = repl.client.transport().sent();
        Ok(())
    }

    fn body(&self, turn: usize) -> Result<&String, String> {
        self.sent.get(turn - 1).ok_or_else(|| format!("the sitting posted {} request(s), not {turn}", self.sent.len()))
    }
}

impl World for WorkspaceWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // ── setting the stage ─────────────────────────────────────────────
        if let Some(path) = world::text(step, "project_file") {
            let body = world::text(step, "body").unwrap_or_default();
            let full = self.root.join(&path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            return std::fs::write(full, body).map_err(|e| e.to_string());
        }
        if let Some(turns) = world::list(step, "model_says_in_turn") {
            self.turns = turns.iter().map(|t| ai::provider::text_sse(t, 10, 5)).collect();
            return Ok(());
        }
        if let Some(lines) = world::list(step, "type") {
            self.lines = lines;
            return Ok(());
        }
        if world::text(step, "open").is_some() {
            return self.open();
        }

        // ── what must be true ─────────────────────────────────────────────
        if let Some(want) = world::list(step, "expect_trust_prompt_contains") {
            return world::expect_contains(&self.asked.join("\n"), &want, "the trust prompt");
        }
        if let Some(n) = world::int(step, "expect_trust_asks") {
            return world::expect_eq(&self.asked.len().to_string(), &n.to_string(), "how often trust was asked");
        }
        if let Some(want) = world::text(step, "expect_overlay") {
            let got = match self.overlaid {
                Some(true) => "on",
                Some(false) => "off",
                None => "unopened",
            };
            return world::expect_eq(got, &want, "the project overlay");
        }
        if let Some(want) = world::list(step, "expect_request_contains") {
            let turn = world::int(step, "turn").unwrap_or(1).max(1) as usize;
            return world::expect_contains(self.body(turn)?, &want, &format!("the body of request {turn}"));
        }
        if let Some(bad) = world::list(step, "expect_request_excludes") {
            let turn = world::int(step, "turn").unwrap_or(1).max(1) as usize;
            return world::expect_missing(self.body(turn)?, &bad, &format!("the body of request {turn}"));
        }
        if let Some(n) = world::int(step, "expect_requests") {
            return world::expect_eq(&self.sent.len().to_string(), &n.to_string(), "how many requests the sitting posted");
        }
        if let Some(want) = world::list(step, "expect_chat_log_contains") {
            let dir = self.session_dir.join("chat");
            let mut logs: Vec<_> = std::fs::read_dir(&dir)
                .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md")).collect())
                .unwrap_or_default();
            logs.sort();
            let text = logs.last().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
            return world::expect_contains(&text, &want, "the conversation log");
        }
        Err(world::unknown_verb(step))
    }
}
