//! The headless face of the subcommands — `plugin`, `config`, `theme`, `profile`,
//! `ai`. Each takes the raw subcommand argv tail and returns a process exit code;
//! the binary just dispatches the leading word here and propagates the code.
//!
//! ALL AI runs through here — the terminal window never talks to a model. The
//! shell plugin's `@ai` / `@<agent>` / `@flow` handlers call `aiTerminal ai …`,
//! stream to stdout (into the terminal), and background workflows are tracked as
//! job records under `~/.aiTerminal/ai/jobs/` (`aiTerminal ai jobs`).
//!
//! The `ai` subcommand stays offline-capable: it never reads keys off the machine —
//! the API key comes only from the configured env var (or an explicit `[ai] api_key`).

use std::path::Path;

/// `aiTerminal ai …` — the terminal-native AI entry point.
///
/// - `ai "<prompt>"` — stream a Markdown answer to stdout.
/// - `ai --command "<request>"` — natural language → one guarded shell command.
/// - `ai --agent <name> "<task>"` — run an agent's full tool loop (`@<agent>`).
/// - `ai --flow <name> "<input>"` — run a declarative multi-step flow (workflow).
/// - `ai --bg …` — run any of the above detached, tracked as a job.
/// - `ai job [<task> [--agent <name>] [--bg]]` — run a TRACKED task; bare = list.
/// - `ai flow [<name>|<free text>] [--bg]` — run a flow (unknown first word →
///   the default `implement` pipeline over the whole text); bare = list.
pub fn ai(args: &[String]) -> i32 {
    // Word subcommands first. Singular, like every command; both take intuitive
    // free-text forms with optional flags anywhere (`@job build the docs --bg`).
    match args.first().map(String::as_str) {
        Some("job") => return ai_job_cmd(args),
        Some("flow") => return ai_flow_cmd(args),
        _ => {}
    }

    let mut as_command = false;
    let mut agent: Option<String> = None;
    let mut flow: Option<String> = None;
    let mut looped = false;
    let mut opts = LoopOpts::default();
    let mut bg = false;
    let mut job_record: Option<String> = None;
    let mut parts: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--command" | "-c" => as_command = true,
            "--fast" => {} // reserved; Q&A already uses the pool, command uses the fast model
            "--agent" => agent = it.next().cloned(),
            "--flow" => flow = it.next().cloned(),
            "--loop" => looped = true,
            "--check" => opts.check = it.next().cloned(),
            "--max" => opts.max = it.next().and_then(|s| s.parse().ok()).unwrap_or(opts.max),
            "--budget" => opts.budget = it.next().and_then(|s| s.parse().ok()),
            "--bg" => bg = true,
            "--job-record" => job_record = it.next().cloned(),
            other => parts.push(other),
        }
    }
    let prompt = parts.join(" ");
    if prompt.trim().is_empty() && flow.is_none() {
        eprintln!("usage: aiTerminal ai [--command | --agent <name> | --flow <name> | --loop] [--bg] \"<prompt>\"");
        eprintln!("       aiTerminal ai --loop \"<goal>\" [--check \"<cmd>\"] [--max N] [--budget TOKENS] [--agent <name>]");
        eprintln!("       aiTerminal ai job [clear] | flow");
        return 2;
    }

    // `--bg`: relaunch this exact invocation detached, stdout+stderr → the job log,
    // and return immediately with the job id (monitor with `ai jobs` / `tail -f`).
    if bg {
        return spawn_background(args);
    }

    if looped {
        opts.agent = agent.unwrap_or_else(|| "coder".into());
        let code = run_loop_cli(&prompt, opts);
        if let Some(id) = job_record {
            jobs::finish(&id, code);
        }
        return code;
    }

    let code = ai_run(as_command, agent, flow, &prompt);
    // A detached child carries `--job-record <id>`: stamp the job's outcome.
    if let Some(id) = job_record {
        jobs::finish(&id, code);
    }
    code
}

/// The foreground AI run (Q&A / command / agent / flow), streaming to stdout.
fn ai_run(as_command: bool, agent: Option<String>, flow: Option<String>, prompt: &str) -> i32 {
    use std::io::Write;

    // `@<path>` tokens attach files: images/PDFs ride the request (vision/document),
    // text files inline into the context below.
    let (prompt, media, file_ctx) = collect_attachments(prompt);
    let prompt = prompt.as_str();

    let cfg = crate::config::Config::load();
    let settings = cfg.ai_settings();
    if settings.resolve_key().is_none() {
        // Provider-agnostic guidance (no vendor assumed): tells the user to add a model +
        // key in config.toml, or — if a model IS configured — names that model's env var.
        // `@ai --command` discards stderr, so its error must ride the stdout marker line
        // (a comment, single-line) to be seen; the Q&A / `@agent` paths show stderr.
        if as_command {
            println!("{}", error_comment(&crate::ai::setup_hint_short(&settings)));
            return 0;
        }
        eprintln!("aiTerminal: {}", crate::ai::setup_hint(&settings));
        return 2;
    }

    // Ground on cwd + shell + the host's redacted terminal-session file (the focused
    // pane's recent commands + output), so `@ai go into it` / `@<agent>` can resolve
    // "it"/"that". The host writes `$TT_SESSION_LOG` only when sharing is enabled.
    let cwd_path = std::env::current_dir().ok();
    let cwd = cwd_path.as_ref().map(|p| p.display().to_string());
    let shell = std::env::var("SHELL").unwrap_or_default();
    let recent_lines = session_lines();
    let term = crate::ai::capture_context(
        &crate::ai::TermContext { cwd: cwd.as_deref(), shell: &shell, recent_lines: &recent_lines },
        40,
    );
    // This folder's persisted session — its recent-run digest + folder-scoped memory —
    // so a run in a project it has seen before starts with that context restored.
    let session = crate::ai::Session::for_cwd();
    let folder_mem = session.as_ref().map(|s| s.memory_dir());
    // The global aiTerminal.md instructions lead, then this folder's recent-activity
    // digest, auto-recalled memories (folder-first then global, gated by `[ai] memory`),
    // the terminal grounding, and any attached files. Everything is redacted before egress.
    let ctx = format!(
        "{}{}{}{term}{file_ctx}",
        instructions_preamble(),
        session_preamble(session.as_ref()),
        memory_preamble(&cfg, prompt, folder_mem.as_deref()),
    );
    // Apply the user's AI-scope redaction rules (config + plugins) before egress.
    let registry = crate::plugin::load_registry(&cfg);
    let policy = crate::security::build_policy(&cfg, &registry);
    let ctx = policy.redact(&ctx, crate::security::RedactScope::Ai);
    let policy = std::sync::Arc::new(policy);
    let workspace_root = cwd_path.clone();

    // `--flow <name>` runs a declarative multi-step agent sequence.
    if let Some(name) = flow {
        let code = run_flow_cli(&cfg, settings, &name, prompt, &ctx, workspace_root, policy, media);
        record_session_run(session.as_ref(), "@flow", prompt, &outcome_label(code));
        return code;
    }

    // `--agent <name>` runs the agent's full tool loop (tools = native objects via a
    // pure `caps::run` runner), streaming live — no GUI/host needed.
    if let Some(name) = agent {
        let mode = format!("@{name}");
        let code = run_agent_cli(&cfg, settings, &name, prompt, &ctx, workspace_root, policy, media);
        record_session_run(session.as_ref(), &mode, prompt, &outcome_label(code));
        return code;
    }

    let cancel = crate::ai::CancelToken::new();
    let _sigint = wire_sigint(cancel.clone());
    let client = crate::ai::Client::new(settings, crate::ai::CurlTransport::default()).with_images(media).with_cancel(cancel);

    // `@ai` uses a tiny STREAMABLE contract: the reply is EITHER a one-line `RUN: <command>`
    // (guarded + preloaded for the user to edit/run — or auto-run per `[ai] mode`) OR a
    // teacher-style answer that renders LIVE as it streams (Markdown + native diagrams). We hold
    // only the first few chars to detect `RUN:`; anything else starts rendering immediately, so
    // the answer appears block-by-block. Default = answer (safe). Output rides stderr (stdout
    // carries the marker line); rendering is on when stderr is a TTY.
    if as_command {
        let started = std::time::Instant::now();
        let (dim, r) = (muted(), reset());
        let mut sink = TerminalSink::new(cfg.ai_show_reasoning);
        let out = crate::ai::classify_command_reply(client.to_command(prompt, &ctx).into_iter(), &mut sink);
        sink.quiet();
        let (tin, tout) = (out.input_tokens as u64, out.output_tokens as u64);
        let footer = |tin, tout| {
            run_footer_with("\u{2713}", started.elapsed(), 0, tin, tout, Some(client.model().cost(tin, tout)), cfg.ai_budget)
        };

        match out.reply {
            crate::ai::CommandReply::Failed(e) => {
                println!("{}", error_comment(&format!("AI error: {e}")));
            }
            crate::ai::CommandReply::Command(cmd) => {
                eprintln!("{dim}{cmd}{r}");
                eprintln!("{dim}{}{r}", footer(tin, tout));
                let verdict = policy.check_command(&cmd);
                println!("{}", command_marker(Some(&cmd), Some(verdict), &cfg.ai_command_mode, &cmd));
                record_session_run(session.as_ref(), "@ai", prompt, &cmd);
            }
            crate::ai::CommandReply::Answer => {
                sink.finish();
                eprintln!();
                eprintln!("{dim}{}{r}", footer(tin, tout));
                println!("{ANSWER_MARK}");
                record_session_run(session.as_ref(), "@ai", prompt, "answered");
            }
            // Nothing came back. Say so on the marker line — an empty answer would
            // print nothing and preload nothing, which reads as the terminal ignoring you.
            crate::ai::CommandReply::Empty => {
                println!("{}", command_marker(None, None, &cfg.ai_command_mode, ""));
            }
        }
        return 0;
    }

    // Q&A streams straight to stdout; the chrome (spinner, dim thinking, token
    // footer) rides stderr so a piped answer stays clean.
    let started = std::time::Instant::now();
    let mut spinner = Some(Spinner::start("thinking\u{2026}".into()));
    let (dim, r) = (muted(), reset());
    let mut thinking_open = false;
    let (mut tin, mut tout) = (0u64, 0u64);
    let mut out = std::io::stdout();
    for ev in client.ask(prompt, &ctx) {
        match ev {
            crate::ai::StreamEvent::Delta(s) => {
                if let Some(mut sp) = spinner.take() {
                    sp.stop();
                }
                if thinking_open {
                    eprintln!();
                    thinking_open = false;
                }
                let _ = out.write_all(s.as_bytes());
                let _ = out.flush();
            }
            crate::ai::StreamEvent::Thinking(t) => {
                // Hidden by default (see the dual-mode path); the spinner keeps animating.
                if !cfg.ai_show_reasoning {
                    continue;
                }
                if let Some(mut sp) = spinner.take() {
                    sp.stop();
                }
                if !thinking_open {
                    eprint!("{dim}\u{2234} {r}");
                    thinking_open = true;
                }
                eprint!("{dim}{t}{r}");
            }
            crate::ai::StreamEvent::Done { input_tokens, output_tokens, .. } => {
                tin = input_tokens as u64;
                tout = output_tokens as u64;
                break;
            }
            crate::ai::StreamEvent::Error(e) => {
                drop(spinner.take());
                eprintln!("\naiTerminal: AI error: {e}");
                return 1;
            }
        }
    }
    drop(spinner.take());
    if thinking_open {
        eprintln!();
    }
    println!();
    eprintln!("{dim}{}{r}", run_footer_with("\u{2713}", started.elapsed(), 0, tin, tout, Some(client.model().cost(tin, tout)), cfg.ai_budget));
    record_session_run(session.as_ref(), "@ai (q&a)", prompt, "answered");
    0
}

/// A compact outcome label from an exit code, for the folder-session digest.
fn outcome_label(code: i32) -> String {
    match code {
        0 => "ok".into(),
        2 => "setup error".into(),
        130 => "interrupted".into(),
        _ => "failed".into(),
    }
}

/// Append one run to this folder's session digest — best-effort, never blocks/fails a run.
fn record_session_run(session: Option<&crate::ai::Session>, mode: &str, prompt: &str, outcome: &str) {
    if let Some(s) = session {
        s.record_run(mode, prompt, outcome);
    }
}

/// The global AI instructions (`~/.aiTerminal/aiTerminal.md`) — the system-prompt
/// base for every run. Empty when the file is absent/blank.
fn instructions() -> String {
    std::fs::read_to_string(crate::config::Config::instructions_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

/// The context preamble carrying the global instructions (for the Q&A / command
/// paths, which have no system prompt of their own).
fn instructions_preamble() -> String {
    let text = instructions();
    if text.is_empty() {
        String::new()
    } else {
        format!("## Global instructions (aiTerminal.md)\n{text}\n\n")
    }
}

/// The focused terminal pane's recent session lines (commands + output), as the host
/// wrote them — already redacted — to `$TT_SESSION_LOG`. Empty when sharing is off (the
/// host doesn't set the env) or the file is absent, so `@ai` then grounds on cwd alone.
fn session_lines() -> Vec<String> {
    std::env::var("TT_SESSION_LOG")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// The recalled-memory preamble for `query` — the top relevant memories (BM25,
/// read-only) as a fenced block, so `@ai`/agents ground on durable memory. When a folder
/// memory dir is given, recall is folder-first-then-global; else global only. Empty when
/// `[ai] memory` is off or nothing is relevant.
fn memory_preamble(cfg: &crate::config::Config, query: &str, folder_mem: Option<&std::path::Path>) -> String {
    if !cfg.ai_memory || query.trim().is_empty() {
        return String::new();
    }
    let svc = match folder_mem {
        Some(dir) => crate::ai::MemoryService::for_folder(dir.to_path_buf()),
        None => crate::ai::MemoryService::open(),
    };
    let hits = svc.recall(query, 5);
    if hits.is_empty() {
        return String::new();
    }
    let mut s = String::from("## Relevant memory (recalled — use if helpful)\n");
    for m in &hits {
        s.push_str("- ");
        s.push_str(&m.body.replace('\n', " "));
        s.push('\n');
    }
    s.push('\n');
    s
}

/// The folder-session preamble — the recent-run digest for this project, so a returning
/// run "remembers" what was done here. Bounded (the digest is byte-capped on disk).
/// Empty when there's no session yet.
fn session_preamble(session: Option<&crate::ai::Session>) -> String {
    let Some(session) = session else { return String::new() };
    let digest = session.digest();
    let digest = digest.trim();
    if digest.is_empty() {
        return String::new();
    }
    format!("## This folder's recent AI activity (for continuity)\n{digest}\n\n")
}

// The `@ai --command` path emits EXACTLY ONE line to stdout (the shell's pending
// file); the shell plugin dispatches it by prefix. These markers are the contract:
//   #TT-RUN#     → run it now (auto mode, guard-allowed)
//   #TT-EDIT#    → preload for review, press Enter (manual mode, guard-allowed)
//   #TT-CONFIRM# → preload for review with a warning (guard wants confirmation)
//   #...         → a comment shown but NEVER run (a refusal, a guard block, an error)
const RUN_MARK: &str = "#TT-RUN# ";
const EDIT_MARK: &str = "#TT-EDIT# ";
const CONFIRM_MARK: &str = "#TT-CONFIRM# ";
/// A prose answer was already streamed to stderr — the shell preloads nothing.
pub(crate) const ANSWER_MARK: &str = "#TT-ANSWER#";

/// The single line `@ai --command` prints for a suggested command + guard verdict.
/// Pure (no I/O) so the dispatch policy is unit-testable: auto vs manual, the
/// always-review confirm tier, a guard block, and a model refusal / empty answer.
pub(crate) fn command_marker(cmd: Option<&str>, verdict: Option<crate::security::Verdict>, mode: &str, refusal: &str) -> String {
    use crate::security::Verdict;
    match (cmd, verdict) {
        (Some(c), Some(Verdict::Allow)) => {
            if mode.eq_ignore_ascii_case("auto") {
                format!("{RUN_MARK}{c}")
            } else {
                format!("{EDIT_MARK}{c}")
            }
        }
        (Some(c), Some(Verdict::Confirm { .. })) => format!("{CONFIRM_MARK}{c}"),
        (Some(_), Some(Verdict::Deny { reason })) => format!("# blocked by guard: {reason}"),
        // No command: surface the model's refusal text as a comment (never run).
        _ => {
            let t = refusal.trim();
            if t.is_empty() {
                "# the AI did not suggest a command".to_string()
            } else if t.starts_with('#') {
                t.to_string()
            } else {
                format!("# {t}")
            }
        }
    }
}

/// A `#`-comment the shell shows but never runs — used so `@ai` failures (no key, a
/// model/transport error) are VISIBLE instead of swallowed by the `2>/dev/null` capture.
pub(crate) fn error_comment(msg: &str) -> String {
    format!("# \u{26A0} {msg}")
}

/// Turn a tool-call's raw args string into `(key, value)` pairs for `caps::run`.
/// **Model-agnostic**: a JSON object maps by key; a BARE value (a weaker model calling
/// `fs.list .` instead of `fs.list {"path":"."}`) becomes positional arg `0`, which the
/// caps read via `arg(args, 0, "name")`. Empty / `{}` → no args.
fn tool_args_to_pairs(args: &str) -> Vec<(String, String)> {
    match corelib::wire::Json::parse(args) {
        Ok(corelib::wire::Json::Obj(p)) => p.iter().map(|(k, v)| (k.clone(), json_text(v))).collect(),
        _ => {
            let bare = args.trim();
            if bare.is_empty() || bare == "{}" {
                Vec::new()
            } else {
                vec![("0".to_string(), bare.to_string())]
            }
        }
    }
}

/// JSON value as plain text (a string verbatim, else its JSON form).
fn json_text(v: &corelib::wire::Json) -> String {
    match v {
        corelib::wire::Json::Str(s) => s.clone(),
        other => other.to_string(),
    }
}

// ── the live harness display (Claude-Code-style chrome, all on stderr) ───────
//
// stdout stays pure content (the answer / the one marker line); stderr carries
// the experience: a spinner while waiting, dim streamed thinking with a `∴`
// marker, a timed `⚙` tool trace, and a `✓ elapsed · tools · tokens` footer.
// Everything is TTY-aware: piped/background runs get plain, animation-free
// output automatically.

/// Whether stderr is an interactive terminal (spinner + colors allowed).
fn err_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

/// A truecolor escape from a `TT_*_RGB` env var (exported by the shell
/// integration's colors file), so CLI chrome matches the ACTIVE theme; falls
/// back to a plain ANSI code when unset or not a TTY.
fn theme_color(var: &str, ansi_fallback: &str) -> String {
    if !err_is_tty() {
        return String::new();
    }
    match std::env::var(var) {
        Ok(rgb) if rgb.split(';').count() == 3 => format!("\x1b[38;2;{rgb}m"),
        _ => ansi_fallback.to_string(),
    }
}

fn accent() -> String {
    theme_color("TT_ACCENT_RGB", "\x1b[36m")
}
fn muted() -> String {
    theme_color("TT_MUTED_RGB", "\x1b[2m")
}
fn reset() -> &'static str {
    "\x1b[0m"
}

/// Whether stdout is a terminal (agents/flows/loops stream the answer to stdout, so its
/// Markdown rendering + TTY-gating is keyed on this stream, not stderr).
fn out_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// The Markdown render palette, from the active theme's env colors (with sensible defaults).
pub(crate) fn md_style() -> corelib::md::Style {
    let rgb = |var: &str, default: corelib::types::Rgba8| -> corelib::types::Rgba8 {
        std::env::var(var)
            .ok()
            .and_then(|s| {
                let p: Vec<u8> = s.split(';').filter_map(|x| x.trim().parse().ok()).collect();
                (p.len() == 3).then(|| corelib::types::Rgba8::rgb(p[0], p[1], p[2]))
            })
            .unwrap_or(default)
    };
    let d = corelib::md::Style::default();
    let accent = rgb("TT_ACCENT_RGB", d.accent);
    let muted = rgb("TT_MUTED_RGB", d.muted);
    corelib::md::Style { enabled: true, heading: accent, accent, code: d.code, muted, link: accent }
}

/// Wrap width for rendered Markdown — the split's REAL width (via `TIOCGWINSZ`, since the shell
/// doesn't export `$COLUMNS` to us), minus a small right margin. Falls back to `$COLUMNS`, then
/// 80. No low cap: wide splits are used fully (a generous 400 ceiling just guards absurd sizes).
fn md_width() -> usize {
    let cols = platform::os::terminal_size()
        .map(|(c, _)| c as usize)
        .or_else(|| std::env::var("COLUMNS").ok().and_then(|c| c.trim().parse::<usize>().ok()))
        .unwrap_or(80);
    cols.saturating_sub(2).clamp(24, 400)
}

/// The split's height in rows (for the live renderer's overflow guard); 0 if unknown.
fn term_rows() -> usize {
    platform::os::terminal_size().map(|(_, r)| r as usize).unwrap_or(0)
}

/// Markdown render options when writing to a TTY; `None` (raw text) when piped.
fn markdown_opts(is_tty: bool) -> Option<(corelib::md::Style, usize)> {
    is_tty.then(|| (md_style(), md_width()))
}

/// Where `@ai --command` shows a streaming reply: live Markdown on a terminal, plain text
/// when stderr is redirected.
///
/// It owns the spinner because stopping it is the same event as having something to show.
/// Reasoning is dropped unless the user asked for it, and the spinner deliberately keeps
/// animating through hidden reasoning — otherwise a long think looks like a hang.
struct TerminalSink {
    spinner: Option<Spinner>,
    /// `None` when stderr is not a TTY; the raw buffer is emitted instead.
    live: Option<LiveMarkdown>,
    raw: String,
    show_reasoning: bool,
}

impl TerminalSink {
    fn new(show_reasoning: bool) -> Self {
        TerminalSink {
            spinner: Some(Spinner::start("thinking\u{2026}".into())),
            live: err_is_tty().then(|| LiveMarkdown::new(md_style(), md_width(), term_rows().saturating_sub(2))),
            raw: String::new(),
            show_reasoning,
        }
    }

    /// Nothing may be printed while the spinner owns the line.
    fn quiet(&mut self) {
        if let Some(mut sp) = self.spinner.take() {
            sp.stop();
        }
    }

    /// Flush the live tail once the answer is complete.
    fn finish(&mut self) {
        self.quiet();
        match &mut self.live {
            Some(l) => l.flush(&mut std::io::stderr()),
            None => eprint!("{}", self.raw),
        }
    }
}

impl crate::ai::ReplySink for TerminalSink {
    fn answer(&mut self, text: &str) {
        self.quiet();
        match &mut self.live {
            Some(l) => l.push(&mut std::io::stderr(), text),
            None => self.raw.push_str(text),
        }
    }

    fn thinking(&mut self, text: &str) {
        if !self.show_reasoning {
            return;
        }
        self.quiet();
        eprint!("{}{text}{}", muted(), reset());
    }
}

/// A REALTIME Markdown renderer: completed blocks are committed once (they scroll away
/// untouched), while the single in-progress block is continuously re-rendered and repainted in
/// place — so the current line/paragraph styles in as it streams. Only the small trailing region
/// is ever repainted (via cursor-up + clear), so it stays stable and never disturbs committed
/// content. On a non-TTY it isn't used (the caller streams raw).
struct LiveMarkdown {
    sr: corelib::md::StreamRenderer,
    style: corelib::md::Style,
    width: usize,
    /// Max rows the live tail may occupy before it's clamped (viewport-bounded so the
    /// cursor-repaint can never climb above committed content).
    max_rows: usize,
    /// Screen lines the current tail occupies (what the next erase must undo).
    painted: usize,
}

/// The escape sequence to erase a `painted`-line tail: return to its first line, clear below.
fn erase_seq(painted: usize) -> String {
    if painted == 0 {
        return String::new();
    }
    let mut s = String::from("\r");
    if painted > 1 {
        s.push_str(&format!("\x1b[{}A", painted - 1));
    }
    s.push_str("\x1b[0J");
    s
}

/// Clamp a rendered tail to at most `max_rows` screen lines (keeping the newest), returning the
/// text to print and its line count — so the repaint region never exceeds the viewport.
fn clamp_tail(rendered: &str, max_rows: usize) -> (String, usize) {
    let all: Vec<&str> = rendered.split('\n').collect();
    let (start, n) = if max_rows > 0 && all.len() > max_rows { (all.len() - max_rows, max_rows) } else { (0, all.len()) };
    (all[start..].join("\n"), n)
}

impl LiveMarkdown {
    fn new(style: corelib::md::Style, width: usize, max_rows: usize) -> Self {
        LiveMarkdown { sr: corelib::md::StreamRenderer::new(style, width, &[DIAGRAM_LANG]), style, width, max_rows: if max_rows == 0 { 40 } else { max_rows }, painted: 0 }
    }

    fn write_chunk(w: &mut dyn std::io::Write, c: corelib::md::Chunk) {
        match c {
            corelib::md::Chunk::Text(t) => {
                let _ = w.write_all(t.as_bytes());
            }
            corelib::md::Chunk::Diagram(s) => {
                let _ = w.write_all(diagram_output(&s).as_bytes());
            }
        }
    }

    /// Render the in-progress block for the live tail (a placeholder for an open diagram fence
    /// so raw diagram source is never shown).
    fn render_pending(&self) -> String {
        let pend = self.sr.pending();
        if pend.trim().is_empty() {
            return String::new();
        }
        if is_open_diagram_fence(pend) {
            return format!("{}\u{25c8} drawing diagram\u{2026}{}", muted(), reset());
        }
        corelib::md::render(&corelib::md::parse(pend), &self.style, self.width).trim_end_matches('\n').to_string()
    }

    fn paint(&mut self, w: &mut dyn std::io::Write) {
        let rendered = self.render_pending();
        if rendered.is_empty() {
            self.painted = 0;
            return;
        }
        let (text, n) = clamp_tail(&rendered, self.max_rows);
        let _ = w.write_all(text.as_bytes());
        self.painted = n;
    }

    /// Feed a streamed delta: erase the old tail, commit any newly-completed blocks, repaint the
    /// in-progress tail.
    fn push(&mut self, w: &mut dyn std::io::Write, delta: &str) {
        self.adapt_size(w);
        let _ = w.write_all(erase_seq(self.painted).as_bytes());
        self.painted = 0;
        for c in self.sr.push(delta) {
            Self::write_chunk(w, c);
        }
        self.paint(w);
        let _ = w.flush();
    }

    /// Finalize: erase the tail and commit whatever remains as final output.
    fn flush(&mut self, w: &mut dyn std::io::Write) {
        self.adapt_size(w);
        let _ = w.write_all(erase_seq(self.painted).as_bytes());
        self.painted = 0;
        for c in self.sr.finish() {
            Self::write_chunk(w, c);
        }
        let _ = w.flush();
    }

    /// Re-check the terminal size and adapt to a resize. On a **width** change we must NOT do the
    /// usual cursor-up repaint — the terminal has already reflowed the painted tail, so the
    /// up-count would be wrong and could erase committed content. Instead we *seal*: commit the
    /// already-painted tail as-is (a trailing newline moves below it), drop the renderer's pending
    /// block so it isn't re-emitted, and switch to the new width — all subsequent content wraps to
    /// it. A rows-only change just updates the overflow clamp. Committed scrollback can't reflow
    /// (a terminal fundamental); this keeps rendering stable across a resize and adapts new output.
    fn adapt_size(&mut self, w: &mut dyn std::io::Write) {
        let width = md_width();
        let rows = term_rows();
        if rows != 0 {
            self.max_rows = rows.saturating_sub(2).max(1);
        }
        if width != self.width {
            if self.painted > 0 {
                let _ = w.write_all(b"\n");
            }
            self.painted = 0;
            self.width = width;
            self.sr.set_width(width);
            self.sr.clear_pending();
        }
    }
}

/// The fenced-block language the AI uses for diagrams (kept internal — never shown to users).
const DIAGRAM_LANG: &str = "mermaid";

/// True when `pend` begins a diagram fence that hasn't been closed yet.
fn is_open_diagram_fence(pend: &str) -> bool {
    let mut lines = pend.lines();
    let Some(first) = lines.next().map(str::trim) else { return false };
    let is_fence = first.starts_with("```") || first.starts_with("~~~");
    let lang = first.trim_start_matches(['`', '~']).trim().to_ascii_lowercase();
    if !is_fence || lang != DIAGRAM_LANG {
        return false;
    }
    !lines.any(|l| {
        let t = l.trim_start();
        t.starts_with("```") || t.starts_with("~~~")
    })
}

/// Are we inside our OWN GUI terminal (which draws native diagrams via `OSC 1338`)? The PTY
/// exports `TERM_PROGRAM = <brand>` to its children.
fn is_native_terminal() -> bool {
    std::env::var("TERM_PROGRAM").ok().as_deref() == Some(corelib::brand::NAME)
}

/// Grid rows a diagram needs, from its pure layout height (nominal 8×16 cell). Clamped to a
/// sane band. Shared by the inline `OSC 1338` emitter and the `@md edit` preview layout so a
/// diagram reserves the same height everywhere.
pub(crate) fn diagram_rows(source: &str) -> usize {
    corelib::mermaid::parse(source)
        .map(|d| {
            let l = corelib::mermaid::layout(&d, &|s: &str| (corelib::unicode::str_width(s) as u32 * 8, 16));
            l.height.div_ceil(18).clamp(3, 40) as usize
        })
        .unwrap_or(3)
}

/// Turn a diagram's source into terminal output: a native `OSC 1338` placement (with a
/// reserved row count from the pure layout) when our GUI can draw it, else a clean boxed
/// fallback (other terminals / pipes). No jargon is ever shown to the user.
fn diagram_output(source: &str) -> String {
    if is_native_terminal() && corelib::mermaid::parse(source).is_some() {
        let rows = diagram_rows(source);
        return format!("\x1b]1338;{rows};{}\x07", corelib::codec::base64_encode(source.as_bytes()));
    }
    diagram_fallback_box(source)
}

/// A plain boxed rendering of a diagram's source for terminals that can't draw it.
fn diagram_fallback_box(source: &str) -> String {
    let width = source.lines().map(corelib::unicode::str_width).max().unwrap_or(0).clamp(7, 78);
    let (dim, r) = (muted(), reset());
    let mut out = format!("{dim}╭─ diagram {}╮{r}\n", "─".repeat(width.saturating_sub(9)));
    for line in source.lines() {
        let pad = width.saturating_sub(corelib::unicode::str_width(line));
        out.push_str(&format!("{dim}│{r} {line}{} {dim}│{r}\n", " ".repeat(pad)));
    }
    out.push_str(&format!("{dim}╰{}╯{r}\n", "─".repeat(width + 2)));
    out
}

/// `12345` → `12.3k` (token counts stay glanceable).
fn human_tokens(n: u64) -> String {
    if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// `aiTerminal gate <channel> start|stop` / `status` — the `@gate` remote-control
/// gateway. The whole implementation lives in [`crate::gate`]; this is only the
/// argv seam, matching how every other subcommand is wired.
pub fn gate(args: &[String]) -> i32 {
    crate::gate::run(args)
}

/// `@md` — view and edit Markdown files at the prompt. `render <file>` pretty-prints it (styled,
/// full-width, native diagrams); `edit <file>` opens the live split editor. Returns an exit code.
pub fn md(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("render") => md_render(args.get(1)),
        Some("edit") => match args.get(1) {
            Some(path) => crate::mdedit::run(path),
            None => {
                eprintln!("usage: @md edit <file.md>");
                2
            }
        },
        Some("--help") | Some("-h") => {
            eprintln!("{}", md_usage());
            0
        }
        None => {
            eprintln!("{}", md_usage());
            2
        }
        Some(other) => {
            eprintln!("@md: unknown subcommand '{other}'\n{}", md_usage());
            2
        }
    }
}

fn md_usage() -> &'static str {
    "usage:\n  @md render <file.md>   pretty-print a Markdown file (diagrams drawn natively)\n  @md edit <file.md>     live split editor — Markdown left, rendered preview right"
}

/// Render a Markdown file to the terminal. On a TTY it's styled + full-width with native diagrams;
/// content taller than the screen opens a scrollable **pager** (so a long file doesn't just scroll
/// past), while content that fits prints inline (no alt-screen flash). Piped output is plain text +
/// boxed diagrams. Reuses the exact engine `@ai` answers use.
fn md_render(path: Option<&String>) -> i32 {
    use std::io::Write;
    let Some(path) = path else {
        eprintln!("usage: @md render <file.md>");
        return 2;
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("@md: cannot read {path}: {e}");
            return 1;
        }
    };
    let tty = out_is_tty();
    // On a TTY, hand long documents to the scrollable pager (reflows on resize, opens at the top).
    if tty {
        let rows = term_rows();
        let height = crate::mdedit::preview_height(&text, md_width(), md_style());
        if rows > 0 && height > rows.saturating_sub(1) {
            return crate::mdedit::page(path);
        }
    }
    let style = if tty { md_style() } else { corelib::md::Style { enabled: false, ..corelib::md::Style::default() } };
    let mut sr = corelib::md::StreamRenderer::new(style, md_width(), &[DIAGRAM_LANG]);
    let mut out = std::io::stdout().lock();
    let mut emit = |chunks: Vec<corelib::md::Chunk>| {
        for c in chunks {
            match c {
                corelib::md::Chunk::Text(t) => {
                    let _ = out.write_all(t.as_bytes());
                }
                corelib::md::Chunk::Diagram(src) => {
                    let d = if tty { diagram_output(&src) } else { diagram_fallback_box(&src) };
                    let _ = out.write_all(d.as_bytes());
                }
            }
        }
    };
    emit(sr.push(&text));
    emit(sr.finish());
    let _ = out.flush();
    0
}

/// `2048` → `2.0KB` (tool result sizes at a glance).
fn human_bytes(n: usize) -> String {
    if n >= 1024 {
        format!("{:.1}KB", n as f64 / 1024.0)
    } else {
        format!("{n}B")
    }
}

/// A USD amount as a glanceable string: `$1.20`, `$0.014`, `<$0.001`. Empty for ≤ 0
/// (unknown/free pricing — the caller then shows no cost).
fn human_cost(usd: f64) -> String {
    if !usd.is_finite() || usd <= 0.0 {
        String::new()
    } else if usd < 0.001 {
        "<$0.001".to_string()
    } else if usd < 1.0 {
        format!("${usd:.3}")
    } else if usd < 100.0 {
        format!("${usd:.2}")
    } else {
        format!("${usd:.0}")
    }
}

/// The footer's cost tail: ` · ~$0.014` when priced, plus ` · 12% of $0.10` (⚠ when
/// over) when a `[ai] budget` is set. Empty when the model has no pricing.
fn cost_segment(cost: Option<f64>, budget: Option<f64>) -> String {
    let Some(c) = cost.filter(|c| c.is_finite() && *c > 0.0) else { return String::new() };
    let mut s = format!(" \u{b7} ~{}", human_cost(c));
    if let Some(b) = budget.filter(|b| b.is_finite() && *b > 0.0) {
        let pct = (c / b * 100.0).round() as u64;
        let warn = if c > b { "\u{26a0} " } else { "" };
        s.push_str(&format!(" \u{b7} {warn}{pct}% of {}", human_cost(b)));
    }
    s
}

/// Map a run's outcome to the process exit code — the scripting contract:
/// 0 = completed · 1 = failed (error / step limit / stall) · 130 = interrupted.
fn outcome_exit(outcome: &crate::ai::RunOutcome) -> i32 {
    match outcome {
        crate::ai::RunOutcome::Completed => 0,
        crate::ai::RunOutcome::Cancelled => 130,
        _ => 1,
    }
}

/// The footer's status glyph for an outcome.
fn outcome_glyph(outcome: &crate::ai::RunOutcome) -> &'static str {
    match outcome {
        crate::ai::RunOutcome::Completed => "\u{2713}",
        crate::ai::RunOutcome::Cancelled => "\u{23f9}",
        crate::ai::RunOutcome::StepLimit | crate::ai::RunOutcome::ToolStall => "\u{26a0}",
        crate::ai::RunOutcome::Error(_) => "\u{2717}",
    }
}

/// The run footer with an explicit status glyph and optional cost/budget telemetry:
/// `✓ 8.4s · 2 tools · 12.3k in / 1.8k out · ~$0.014 · 14% of $0.10`.
fn run_footer_with(glyph: &str, elapsed: std::time::Duration, tools: usize, tin: u64, tout: u64, cost: Option<f64>, budget: Option<f64>) -> String {
    let secs = elapsed.as_secs_f64();
    let t = if secs >= 10.0 { format!("{secs:.0}s") } else { format!("{secs:.1}s") };
    let mut s = format!("{glyph} {t}");
    if tools > 0 {
        s.push_str(&format!(" \u{b7} {tools} tool{}", if tools == 1 { "" } else { "s" }));
    }
    s.push_str(&format!(" \u{b7} {} in / {} out", human_tokens(tin), human_tokens(tout)));
    s.push_str(&cost_segment(cost, budget));
    s
}

/// A braille spinner on stderr while waiting for the model's first token.
/// TTY-only (a piped/background run gets nothing); `stop()` clears its line.
struct Spinner {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    fn start(label: String) -> Spinner {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if !err_is_tty() {
            return Spinner { stop, handle: None };
        }
        let flag = stop.clone();
        let dim = muted();
        let handle = std::thread::spawn(move || {
            const FRAMES: [char; 10] = ['\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280f}'];
            let mut i = 0usize;
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                eprint!("\r{dim}{} {label}\x1b[0m\x1b[K", FRAMES[i % FRAMES.len()]);
                i += 1;
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            eprint!("\r\x1b[K");
        });
        Spinner { stop, handle: Some(handle) }
    }

    fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A live streaming display for agent/flow/loop runs: answer tokens print to the
/// writer AS THEY ARRIVE; the `@tool …` machine protocol lines are suppressed
/// (the tool trace prints separately); reasoning streams dim to stderr. The
/// engine is line-buffered only as far as needed to decide whether a line is
/// protocol — ordinary prose flushes mid-line, so typing stays live.
struct CliObserver<W: std::io::Write> {
    out: W,
    /// The undecided head of the current line — held only while it is still a
    /// prefix of the `@tool` marker.
    pending: String,
    /// The rest of this line is a decided `@tool` protocol line — swallow it.
    suppress_line: bool,
    /// A tool call was made this turn — everything after it is protocol.
    suppress_turn: bool,
    /// Everything printed so far (so the caller can avoid re-printing the answer).
    streamed: String,
    /// Whether any answer text has printed (for inter-turn spacing).
    printed: bool,
    /// The waiting spinner for the current turn (stopped on the first token).
    spinner: Option<Spinner>,
    /// Whether the current thinking burst already printed its `∴` marker.
    thinking_open: bool,
    /// Print the raw reasoning text (`[ai] show_reasoning`). Default `false`: reasoning is
    /// hidden behind the animated `∴ thinking…` spinner; tools + answer still stream.
    show_reasoning: bool,
    /// When set, the answer renders through a LIVE (realtime) Markdown renderer — the in-progress
    /// block repaints as it streams, completed blocks commit once. Off (piped) → stream raw.
    live: Option<LiveMarkdown>,
}

impl<W: std::io::Write> CliObserver<W> {
    fn new(out: W) -> Self {
        CliObserver { out, pending: String::new(), suppress_line: false, suppress_turn: false, streamed: String::new(), printed: false, spinner: None, thinking_open: false, show_reasoning: false, live: None }
    }

    /// Opt into streaming the model's raw reasoning text (off by default).
    fn with_reasoning(mut self, show: bool) -> Self {
        self.show_reasoning = show;
        self
    }

    /// Render the answer as realtime styled Markdown instead of raw. `None` on a non-TTY (piped)
    /// target so pipes stay clean.
    fn with_markdown(mut self, md: Option<(corelib::md::Style, usize)>) -> Self {
        self.live = md.map(|(style, width)| LiveMarkdown::new(style, width, term_rows().saturating_sub(2)));
        self
    }

    /// First sign of life this turn — clear the waiting spinner.
    fn wake(&mut self) {
        if let Some(mut sp) = self.spinner.take() {
            sp.stop();
        }
    }

    /// What to print for a thinking chunk: the first chunk of a burst gets the
    /// dim `∴ ` marker on its own line start. Pure, so the shape is testable.
    fn thinking_chunk(&mut self, text: &str) -> String {
        let dim = muted();
        let r = reset();
        if self.thinking_open {
            format!("{dim}{text}{r}")
        } else {
            self.thinking_open = true;
            format!("{dim}\u{2234} {text}{r}")
        }
    }

    fn emit(&mut self, s: &str) {
        self.streamed.push_str(s);
        let _ = self.out.write_all(s.as_bytes());
        let _ = self.out.flush();
        if !s.is_empty() {
            self.printed = true;
        }
    }

    /// Feed one streamed chunk through the tool-marker suppression line machine, so the
    /// machine protocol never reaches the display — in ANY tolerated form (`@tool`,
    /// `<tool_call>`, a fenced ```` ```tool ```` block; see `parse_tool_call`).
    fn feed(&mut self, text: &str) {
        for c in text.chars() {
            if self.suppress_turn {
                return;
            }
            if c == '\n' {
                if self.suppress_line {
                    // The whole line was protocol — once a tool line ends, the rest of
                    // the turn is machine JSON; swallow it until the next turn.
                    self.suppress_line = false;
                    self.suppress_turn = true;
                } else {
                    let line = std::mem::take(&mut self.pending);
                    if is_display_tool_marker(line.trim_start()) {
                        self.suppress_turn = true; // a (malformed) bare marker still never prints
                    } else {
                        self.emit(&line);
                        self.emit("\n");
                    }
                }
                continue;
            }
            if self.suppress_line {
                continue;
            }
            self.pending.push(c);
            // Still a possible marker head? Keep holding. Decided marker → suppress. Else flush.
            let t = self.pending.trim_start();
            if is_display_tool_marker_prefix(t) {
                continue; // still a possible marker head — keep holding
            }
            if is_display_tool_marker(t) {
                self.pending.clear();
                self.suppress_line = true;
            } else {
                let line = std::mem::take(&mut self.pending);
                self.emit(&line);
            }
        }
    }
}

/// The line-anchored tool-marker forms suppressed from the live display — sourced from
/// the parser's SINGLE SOURCE OF TRUTH (`ai::agent::TOOL_LINE_MARKERS`) so the display
/// filter can never drift from what `parse_tool_call` actually accepts.
use crate::ai::agent::TOOL_LINE_MARKERS as DISPLAY_TOOL_MARKERS;

/// `t` is (or begins) a tool-call marker line — swallow it from the display.
fn is_display_tool_marker(t: &str) -> bool {
    t == "@tool" || t.starts_with("@tool ") || DISPLAY_TOOL_MARKERS.iter().any(|m| t.starts_with(m))
}

/// `t` could still GROW into a tool marker (a streamed prefix) — keep holding it.
fn is_display_tool_marker_prefix(t: &str) -> bool {
    t == "@tool" || "@tool ".starts_with(t) || DISPLAY_TOOL_MARKERS.iter().any(|m| m.starts_with(t))
}

impl<W: std::io::Write> crate::ai::AgentObserver for CliObserver<W> {
    fn on_turn_start(&mut self) {
        // Flush any held prose from the previous turn and reset the protocol state.
        let held = std::mem::take(&mut self.pending);
        if !held.is_empty() && !self.suppress_line && !self.suppress_turn {
            self.emit(&held);
        }
        if self.printed && !self.streamed.ends_with("\n\n") {
            self.emit(if self.streamed.ends_with('\n') { "\n" } else { "\n\n" });
        }
        self.pending.clear();
        self.suppress_line = false;
        self.suppress_turn = false;
        // A fresh model turn: spin until its first token arrives.
        self.thinking_open = false;
        self.wake();
        self.spinner = Some(Spinner::start("thinking\u{2026}".into()));
    }
    fn on_delta(&mut self, text: &str) {
        // Realtime Markdown: the in-progress block repaints as tokens arrive; completed blocks
        // (and diagrams) commit once. Stop the spinner on the first token.
        if self.live.is_some() {
            self.wake();
            let out = &mut self.out;
            self.live.as_mut().unwrap().push(out, text);
            return;
        }
        self.wake();
        if self.thinking_open {
            self.thinking_open = false;
            eprintln!();
        }
        self.feed(text);
    }
    fn on_thinking(&mut self, text: &str) {
        // By default reasoning is HIDDEN: keep the animated `∴ thinking…` spinner running
        // (do NOT wake it) and print nothing — the user sees the indicator, then tools and
        // the answer. `[ai] show_reasoning = true` restores the dim streamed chain-of-thought.
        if !self.show_reasoning {
            return;
        }
        self.wake();
        let chunk = self.thinking_chunk(text);
        eprint!("{chunk}");
    }
    fn on_commit(&mut self, _prose: &str) {
        self.wake();
        // Realtime mode: finalize the live tail so a following tool trace prints cleanly below it.
        if self.live.is_some() {
            let out = &mut self.out;
            self.live.as_mut().unwrap().flush(out);
            return;
        }
        // Prose lines already streamed; just make sure the tool trace starts clean.
        let held = std::mem::take(&mut self.pending);
        if !held.is_empty() && !self.suppress_line && !self.suppress_turn {
            self.emit(&held);
        }
        if self.printed && !self.streamed.ends_with('\n') {
            self.emit("\n");
        }
    }
    fn on_step_start(&mut self, i: usize, n: usize, label: &str) {
        // A live flow step header on stderr (chrome), so the user watches steps advance.
        self.wake();
        // Realtime mode: finalize the live tail so the step header prints cleanly beneath it.
        if self.live.is_some() {
            let out = &mut self.out;
            self.live.as_mut().unwrap().flush(out);
        }
        if self.printed && !self.streamed.ends_with('\n') {
            self.emit("\n");
        }
        eprintln!("{}\u{25B6} {i}/{n} {label}{}", accent(), reset());
    }
}

/// Finish a streamed run: end the line, and print the returned answer only when
/// it never streamed (an error, a cancel, or an empty stream).
fn finish_streamed<W: std::io::Write>(obs: &mut CliObserver<W>, answer: &str) {
    obs.wake();
    if obs.thinking_open {
        eprintln!();
        obs.thinking_open = false;
    }
    let a = answer.trim();
    // Realtime mode: finalize any trailing tail still buffered in the live renderer.
    if obs.live.is_some() {
        let out = &mut obs.out;
        obs.live.as_mut().unwrap().flush(out);
        let _ = obs.out.write_all(b"\n");
        let _ = obs.out.flush();
        return;
    }
    if !a.is_empty() && !obs.streamed.contains(a) {
        let _ = obs.out.write_all(b"\n");
        let _ = obs.out.write_all(a.as_bytes());
    }
    let _ = obs.out.write_all(b"\n");
    let _ = obs.out.flush();
}

// ── attachments: `@<path>` tokens in the prompt ─────────────────────────────

/// Raw-size cap for an attached image/PDF (base64 grows it ~4/3 on the wire).
const MEDIA_ATTACH_MAX: u64 = 4 * 1024 * 1024;
/// Inline cap for an attached text file.
const TEXT_ATTACH_MAX: usize = 48 * 1024;

/// The attachment media type for a path, by extension: `Some(image/*)`,
/// `Some(application/pdf)`, or `None` (treat as text).
fn media_type_of(path: &std::path::Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("pdf") => Some("application/pdf"),
        _ => None,
    }
}

/// Scan the prompt for `@<path>` tokens naming EXISTING files and turn them into
/// attachments: images + PDFs become request media (vision / document caps),
/// text files inline into the context (fenced, size-capped, skipped if binary).
/// The `@` is dropped from the prompt so the model reads a plain path. Pure over
/// the filesystem — no model, no network.
fn collect_attachments(prompt: &str) -> (String, Vec<crate::ai::ImageData>, String) {
    let mut media = Vec::new();
    let mut file_ctx = String::new();
    let mut out: Vec<String> = Vec::new();
    let mut attached = 0usize;
    for token in prompt.split_whitespace() {
        let Some(path_str) = token.strip_prefix('@').filter(|r| !r.is_empty()) else {
            out.push(token.to_string());
            continue;
        };
        let path = std::path::Path::new(path_str);
        if !path.is_file() {
            out.push(token.to_string()); // not a file — leave the token as typed
            continue;
        }
        // Bound the COUNT too: N × (raw + base64 + request copy) peaks fast.
        if attached >= MAX_ATTACHMENTS {
            eprintln!("aiTerminal: skipping {path_str} (over {MAX_ATTACHMENTS} attachments)");
            out.push(path_str.to_string());
            continue;
        }
        attached += 1;
        match media_type_of(path) {
            Some(mt) => {
                let too_big = std::fs::metadata(path).map(|m| m.len() > MEDIA_ATTACH_MAX).unwrap_or(true);
                if too_big {
                    eprintln!("aiTerminal: skipping {path_str} (over {} MB)", MEDIA_ATTACH_MAX / (1024 * 1024));
                } else if let Ok(bytes) = std::fs::read(path) {
                    media.push(crate::ai::ImageData { media_type: mt.to_string(), b64: corelib::codec::base64_encode(&bytes) });
                }
            }
            None => {
                if let Ok(bytes) = std::fs::read(path) {
                    if bytes.contains(&0) {
                        eprintln!("aiTerminal: skipping {path_str} (binary)");
                    } else {
                        let mut text = String::from_utf8_lossy(&bytes).into_owned();
                        if text.len() > TEXT_ATTACH_MAX {
                            let mut cut = TEXT_ATTACH_MAX;
                            while cut < text.len() && !text.is_char_boundary(cut) {
                                cut += 1;
                            }
                            text.truncate(cut);
                            text.push_str("\n… (truncated)\n");
                        }
                        file_ctx.push_str(&format!("\n## Attached file: {path_str}\n```\n{text}\n```\n"));
                    }
                }
            }
        }
        out.push(path_str.to_string());
    }
    (out.join(" "), media, file_ctx)
}

/// What a [`CliToolRunner`] needs to spawn sub-agents (`task.run` delegation):
/// the model settings + the loadable-definition dirs + the shared guard.
#[derive(Clone)]
struct SubAgentCtx {
    settings: crate::ai::AiSettings,
    agents_dir: std::path::PathBuf,
    skills_dir: std::path::PathBuf,
    prompts_dir: std::path::PathBuf,
}

/// How many levels of `task.run` delegation are allowed (the orchestrating agent
/// may fan out sub-agents; a sub-agent may not delegate further).
const MAX_DELEGATION_DEPTH: u8 = 1;

/// A pure agent tool runner: routes a model tool call to `caps::run` (no live host),
/// intercepts `task.run` (sub-agent delegation), and redacts every result before it
/// re-enters the loop.
struct CliToolRunner {
    ctx: crate::caps::CapCtx,
    mcp: Option<crate::ai::McpHub>,
    sub: SubAgentCtx,
    depth: u8,
}
impl crate::ai::ToolRunner for CliToolRunner {
    fn run(&mut self, name: &str, args: &str) -> Result<String, String> {
        let redact = |this: &CliToolRunner, s: String| this.ctx.policy.redact(&s, crate::security::RedactScope::Ai);
        if name == "task.run" {
            let out = self.run_delegation(args)?;
            return Ok(redact(self, out));
        }
        if name.starts_with("mcp.") {
            let parsed = match corelib::wire::Json::parse(args) {
                Ok(o @ corelib::wire::Json::Obj(_)) => o,
                _ => corelib::wire::Json::Obj(Vec::new()),
            };
            let out = self.mcp.as_mut().ok_or("mcp: no servers are running")?.call(name, parsed)?;
            return Ok(redact(self, out));
        }
        let pairs = tool_args_to_pairs(args);
        // A concise, TIMED tool trace on stderr, so a streaming run shows its work.
        let preview: String = args.chars().take(72).collect();
        let started = std::time::Instant::now();
        let result = crate::caps::run(name, &pairs, &self.ctx);
        let ms = started.elapsed().as_millis();
        let (dim, r) = (muted(), reset());
        match &result {
            Ok(v) => {
                let size = json_text(v).len();
                eprintln!("{dim}  \u{2699} {name} {preview} \u{b7} {ms}ms \u{b7} {}{r}", human_bytes(size));
            }
            Err(e) => {
                let brief: String = e.chars().take(80).collect();
                eprintln!("{dim}  \u{2699} {name} {preview} \u{b7} {ms}ms \u{b7} \u{2717} {brief}{r}");
            }
        }
        result.map(|j| redact(self, json_text(&j)))
    }
}

impl CliToolRunner {
    /// `task.run` — spawn one named sub-agent (`{agent, prompt}`) or fan out a JSON
    /// `tasks` array of `{agent, prompt}` IN PARALLEL (one thread each), and fold the
    /// reports back as markdown. Sub-agents run **safe-tools-only** and may not
    /// delegate further (depth cap), so a delegate can never run an unapproved risky
    /// command — the security model holds across the seam.
    fn run_delegation(&self, args: &str) -> Result<String, String> {
        if self.depth >= MAX_DELEGATION_DEPTH {
            return Err("task.run: sub-agents may not delegate further".into());
        }
        let tasks = parse_delegation(args)?;
        eprintln!("  \u{2514} task.run \u{2192} {} sub-agent(s): {}", tasks.len(), tasks.iter().map(|(a, _)| format!("@{a}")).collect::<Vec<_>>().join(" "));

        let handles: Vec<std::thread::JoinHandle<(String, String)>> = tasks
            .into_iter()
            .map(|(agent, prompt)| {
                let sub = self.sub.clone();
                let ctx = self.ctx.clone();
                let depth = self.depth + 1;
                std::thread::spawn(move || {
                    let report = run_sub_agent(&sub, ctx, depth, &agent, &prompt);
                    (agent, report)
                })
            })
            .collect();
        let mut out = String::new();
        for h in handles {
            let (agent, report) = h.join().unwrap_or_else(|_| ("?".into(), "sub-agent panicked".into()));
            out.push_str(&format!("## @{agent} report\n{report}\n\n"));
        }
        Ok(out.trim_end().to_string())
    }
}

/// Parse `task.run` args into a bounded `(agent, prompt)` list: `{agent, prompt}`
/// for one delegate, or `{tasks: [{agent, prompt}, …]}` to fan out (capped at 6).
/// Pure, so the delegation contract is unit-testable without spawning agents.
fn parse_delegation(args: &str) -> Result<Vec<(String, String)>, String> {
    let parsed = corelib::wire::Json::parse(args).map_err(|e| format!("task.run: bad args: {e}"))?;
    let mut tasks: Vec<(String, String)> = Vec::new();
    let push = |tasks: &mut Vec<(String, String)>, node: &corelib::wire::Json| {
        let agent = node.get("agent").and_then(|v| v.as_str()).unwrap_or("explorer").to_string();
        let prompt = node.get("prompt").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        if !prompt.trim().is_empty() {
            tasks.push((agent, prompt));
        }
    };
    if let Some(arr) = parsed.get("tasks").and_then(|t| t.as_array()) {
        for t in arr {
            push(&mut tasks, t);
        }
    } else {
        push(&mut tasks, &parsed);
    }
    if tasks.is_empty() {
        return Err("task.run: needs `agent`+`prompt`, or a `tasks` array of {agent, prompt}".into());
    }
    tasks.truncate(6); // bound the fan-out
    Ok(tasks)
}

/// Run one sub-agent to completion (safe tools only; no MCP; no further delegation)
/// and return its final answer.
fn run_sub_agent(sub: &SubAgentCtx, ctx: crate::caps::CapCtx, depth: u8, name: &str, prompt: &str) -> String {
    let raw = crate::ai::defs::build_agent(&sub.agents_dir, &sub.skills_dir, &sub.prompts_dir, name);
    let (system, tools) = match raw {
        Some(r) => {
            // Delegates are read/safe-only: keep only the agent's tools that are in the
            // safe set (so a `coder` delegate explores but never writes/executes).
            let safe: Vec<String> = r.tools.into_iter().filter(|t| crate::ai::DEFAULT_SAFE_TOOLS.contains(&t.as_str())).collect();
            (r.system, if safe.is_empty() { default_safe_tools() } else { safe })
        }
        None => (format!("You are `{name}`, a focused read-only sub-agent. Investigate and report concisely."), default_safe_tools()),
    };
    let spec = crate::ai::AgentSpec {
        system,
        tools: tools.into_iter().map(|n| crate::ai::ToolSpec { describe: crate::caps::describe(&n).to_string(), name: n }).collect(),
        max_steps: 12,
    };
    let client = crate::ai::Client::new(sub.settings.clone(), crate::ai::CurlTransport::default());
    let mut runner = CliToolRunner { ctx, mcp: None, sub: sub.clone(), depth };
    let run = crate::ai::run_agent(&client, &spec, prompt, "", &mut runner, &mut crate::ai::NoopObserver);
    run.answer
}

fn default_safe_tools() -> Vec<String> {
    crate::ai::DEFAULT_SAFE_TOOLS.iter().map(|s| s.to_string()).collect()
}

/// Assemble the tool-loop plumbing shared by `--agent` and `--flow`: the MCP hub,
/// the capability context, and the delegation context.
fn build_runner(cfg: &crate::config::Config, settings: &crate::ai::AiSettings, workspace_root: Option<std::path::PathBuf>, policy: std::sync::Arc<crate::security::Policy>, with_mcp: bool) -> CliToolRunner {
    // The agent's file WRITES are confined to the invocation directory; MCP servers
    // come from the global `ai/mcp/` declarations.
    let workspace = workspace_root.or_else(|| std::env::current_dir().ok());
    // Folder-scoped memory: the agent's `memory.*` tools read/write THIS project's session
    // store (recall folder-first, then global). Derived from the same workspace that bounds
    // fs writes, so identity is consistent — no separate plumbing.
    let memory_dir = workspace
        .as_ref()
        .map(|w| crate::ai::Session::at(w, &crate::config::Config::sessions_dir()).memory_dir());
    let mcp = if with_mcp {
        let mcp_dirs = vec![crate::config::Config::mcp_dir()];
        let servers = crate::ai::load_servers(&mcp_dirs);
        if servers.is_empty() {
            None
        } else {
            let hub = crate::ai::McpHub::launch(&servers);
            if hub.is_empty() { None } else { Some(hub) }
        }
    } else {
        None
    };
    CliToolRunner {
        ctx: crate::caps::CapCtx { policy, app_data: None, remote_enabled: cfg.ai_network, origin: "terminal://ai/".into(), sandbox: workspace, memory_dir },
        mcp,
        sub: SubAgentCtx {
            settings: settings.clone(),
            agents_dir: crate::config::Config::agents_dir(),
            skills_dir: crate::config::Config::skills_dir(),
            prompts_dir: crate::config::Config::prompts_dir(),
        },
        depth: 0,
    }
}

/// "try one of: coder, explorer, …" — the installed agent names for not-found errors.
fn available_agents_hint() -> String {
    let names: Vec<String> = crate::ai::defs::load_agents(&crate::config::Config::agents_dir()).into_iter().map(|a| a.name).collect();
    if names.is_empty() {
        format!("no agents installed in {}", crate::config::Config::agents_dir().display())
    } else {
        format!("try one of: {}", names.join(", "))
    }
}

/// Build a full [`AgentSpec`](crate::ai::AgentSpec) for the named on-disk agent
/// (tool descriptions injected from `caps`, the global `aiTerminal.md`
/// instructions prepended to the system prompt), or `None` when it doesn't exist.
fn build_agent_spec(name: &str) -> Option<crate::ai::AgentSpec> {
    let raw = crate::ai::defs::build_agent(&crate::config::Config::agents_dir(), &crate::config::Config::skills_dir(), &crate::config::Config::prompts_dir(), name)?;
    let tools = raw.tools.into_iter().map(|n| crate::ai::ToolSpec { describe: crate::caps::describe(&n).to_string(), name: n }).collect();
    let global = instructions();
    let system = if global.is_empty() { raw.system } else { format!("{global}\n\n{}", raw.system) };
    Some(crate::ai::AgentSpec { system, tools, max_steps: raw.max_steps })
}

/// Wire Ctrl+C to a [`CancelToken`](crate::ai::CancelToken): installs the
/// process SIGINT flag and watches it on a thread — when it fires, the token
/// cancels (the engine stops between turns; a mid-stream curl is killed). The
/// watcher exits when `done` flips. Returns the shared flag for post-run checks.
fn wire_sigint(token: crate::ai::CancelToken) -> SigintWatch {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let flag = platform::os::sigint_flag();
        let done = done.clone();
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    token.cancel();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
    }
    SigintWatch { done }
}

/// The RAII handle for a [`wire_sigint`] watcher: dropping it stops the polling
/// thread on EVERY exit path (an early return can no longer leak a 20 Hz spinner).
/// Interruption itself is observed through the run's `RunOutcome::Cancelled`.
struct SigintWatch {
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for SigintWatch {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// stdout plus an optional file — the foreground-tracked `@job` tees its
/// streamed answer into the job log while it plays live in the terminal.
struct Tee {
    log: Option<std::fs::File>,
}

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = std::io::stdout().write(buf)?;
        if let Some(f) = &mut self.log {
            let _ = f.write_all(&buf[..n]);
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(f) = &mut self.log {
            let _ = f.flush();
        }
        std::io::stdout().flush()
    }
}

/// Run an agent's tool loop headlessly, streaming tokens live (answer → stdout
/// (+ an optional tee into a job log), reasoning → stderr, tool calls → an
/// stderr trace), with the header/footer chrome.
fn run_agent_streaming(cfg: &crate::config::Config, settings: crate::ai::AiSettings, name: &str, prompt: &str, ctx: &str, workspace_root: Option<std::path::PathBuf>, policy: std::sync::Arc<crate::security::Policy>, media: Vec<crate::ai::ImageData>, log: Option<std::fs::File>) -> i32 {
    let Some(mut agent) = build_agent_spec(name) else {
        eprintln!("aiTerminal: no agent '{name}' — {}", available_agents_hint());
        return 2;
    };
    let client = crate::ai::Client::new(settings.clone(), crate::ai::CurlTransport::default()).with_images(media);
    let mut runner = build_runner(cfg, &settings, workspace_root, policy, true);
    if let Some(hub) = &runner.mcp {
        for (name, describe) in hub.tools() {
            agent.tools.push(crate::ai::ToolSpec { name, describe });
        }
    }
    // Ctrl+C cancels cooperatively: the engine finishes the current write-free
    // moment and returns a Cancelled outcome instead of the process dying mid-run.
    let cancel = crate::ai::CancelToken::new();
    let client = client.with_cancel(cancel.clone());
    let _sigint = wire_sigint(cancel);
    eprintln!("{}\u{2726} @{name} \u{b7} {}{}", accent(), client.model().id, reset());
    let started = std::time::Instant::now();
    let mut obs = CliObserver::new(Tee { log }).with_reasoning(cfg.ai_show_reasoning).with_markdown(markdown_opts(out_is_tty()));
    let run = crate::ai::run_agent(&client, &agent, prompt, ctx, &mut runner, &mut obs);
    finish_streamed(&mut obs, &run.answer);
    let glyph = outcome_glyph(&run.outcome);
    let cost = Some(client.model().cost(run.input_tokens as u64, run.output_tokens as u64));
    eprintln!("{}{}{}", muted(), run_footer_with(glyph, started.elapsed(), run.steps.len(), run.input_tokens as u64, run.output_tokens as u64, cost, cfg.ai_budget), reset());
    outcome_exit(&run.outcome)
}

/// The `--agent` flag path (no job record).
fn run_agent_cli(cfg: &crate::config::Config, settings: crate::ai::AiSettings, name: &str, prompt: &str, ctx: &str, workspace_root: Option<std::path::PathBuf>, policy: std::sync::Arc<crate::security::Policy>, media: Vec<crate::ai::ImageData>) -> i32 {
    run_agent_streaming(cfg, settings, name, prompt, ctx, workspace_root, policy, media, None)
}

// ===== flows (declarative multi-step workflows) ==============================

/// One parsed `[[step]]` of a flow file.
struct FlowStep {
    label: String,
    agent: String,
    prompt: String,
}

/// A declarative flow: `ai/flows/<name>.toml` — a named sequence of agent steps,
/// each step's answer chained into the next step's context.
struct Flow {
    name: String,
    description: String,
    chain: bool,
    steps: Vec<FlowStep>,
}

/// Parse a flow document. Each `[[step]]` needs an `agent` + `prompt`; `{{input}}`
/// in a prompt is replaced with the CLI input.
fn parse_flow(name: &str, text: &str) -> Result<Flow, String> {
    let doc = corelib::wire::Toml::parse(text).map_err(|e| format!("flow '{name}': {e}"))?;
    let description = doc.get("description").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let chain = doc.get("chain").and_then(|v| v.as_bool()).unwrap_or(true);
    let mut steps = Vec::new();
    if let Some(arr) = doc.get("step").and_then(|v| v.as_array()) {
        for (i, s) in arr.iter().enumerate() {
            let agent = s.get("agent").and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
            let prompt = s.get("prompt").and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
            if agent.is_empty() || prompt.is_empty() {
                return Err(format!("flow '{name}': step {} needs `agent` and `prompt`", i + 1));
            }
            let label = s.get("label").and_then(|v| v.as_str()).unwrap_or(&agent).to_string();
            steps.push(FlowStep { label, agent, prompt });
        }
    }
    if steps.is_empty() {
        return Err(format!("flow '{name}': no [[step]] entries"));
    }
    Ok(Flow { name: name.to_string(), description, chain, steps })
}

/// Load flow `name` from `~/.aiTerminal/ai/flows/<name>.toml`.
fn load_flow(name: &str) -> Result<Flow, String> {
    let safe = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if name.is_empty() || !safe {
        return Err(format!("'{name}' is not a flow name"));
    }
    let path = crate::config::Config::flows_dir().join(format!("{name}.toml"));
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_flow(name, &text),
        Err(_) => Err(format!("no flow '{name}' in {}", crate::config::Config::flows_dir().display())),
    }
}

/// `aiTerminal ai --flow <name> "<input>"` — run the flow's steps in sequence via
/// [`run_orchestration`](crate::ai::run_orchestration), streaming step progress to
/// stderr and the final answer to stdout.
fn run_flow_cli(cfg: &crate::config::Config, settings: crate::ai::AiSettings, name: &str, input: &str, ctx: &str, workspace_root: Option<std::path::PathBuf>, policy: std::sync::Arc<crate::security::Policy>, media: Vec<crate::ai::ImageData>) -> i32 {
    let flow = match load_flow(name) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            return 2;
        }
    };
    let mut steps = Vec::new();
    for s in &flow.steps {
        let Some(agent) = build_agent_spec(&s.agent) else {
            eprintln!("aiTerminal: flow '{}': no agent '{}' — {}", flow.name, s.agent, available_agents_hint());
            return 2;
        };
        steps.push(crate::ai::OrchestrationStep {
            label: s.label.clone(),
            agent,
            prompt: s.prompt.replace("{{input}}", input),
        });
    }
    // A compact header; each step announces itself live via the observer (▶ i/n label).
    eprintln!("{}\u{25B6} flow '{}' — {} step(s){}", accent(), flow.name, steps.len(), reset());
    let cancel = crate::ai::CancelToken::new();
    let _sigint = wire_sigint(cancel.clone());
    let client = crate::ai::Client::new(settings.clone(), crate::ai::CurlTransport::default()).with_images(media).with_cancel(cancel);
    let mut runner = build_runner(cfg, &settings, workspace_root, policy, true);
    let started = std::time::Instant::now();
    let mut obs = CliObserver::new(std::io::stdout()).with_reasoning(cfg.ai_show_reasoning).with_markdown(markdown_opts(out_is_tty()));
    let result = crate::ai::run_orchestration(&client, &steps, ctx, flow.chain, &mut runner, &mut obs);
    finish_streamed(&mut obs, &result.final_answer);
    let (dim, r) = (muted(), reset());
    // A compact one-line step recap: `✓ explore · ✓ implement · ✓ verify`.
    if !result.steps.is_empty() {
        let recap = result
            .steps
            .iter()
            .map(|s| format!("{} {}", if s.ok { "\u{2713}" } else { "\u{2717}" }, s.label))
            .collect::<Vec<_>>()
            .join(" \u{b7} ");
        eprintln!("{dim}{recap}{r}");
    }
    let complete = result.steps.len() == steps.len() && result.steps.iter().all(|s| s.ok);
    let glyph = if complete { "\u{2713}" } else { "\u{2717}" };
    let cost = Some(client.model().cost(result.input_tokens as u64, result.output_tokens as u64));
    eprintln!("{dim}{}{r}", run_footer_with(glyph, started.elapsed(), result.tools, result.input_tokens as u64, result.output_tokens as u64, cost, cfg.ai_budget));
    if complete { 0 } else { 1 }
}

/// The flow every free-text `@flow <words…>` runs when the first word names no
/// flow file — the bundled explore → implement → verify pipeline.
const DEFAULT_FLOW: &str = "implement";

/// `@flow` — bare lists; `@flow <name> [input]` runs a named flow; any other
/// free text runs the DEFAULT pipeline with the whole text as its input; `--bg`
/// detaches. `args` includes the leading "flow" word.
fn ai_flow_cmd(args: &[String]) -> i32 {
    let mut bg = false;
    let mut record = None;
    let mut words: Vec<String> = Vec::new();
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bg" => bg = true,
            "--job-record" => record = it.next().cloned(),
            w => words.push(w.to_string()),
        }
    }
    if words.is_empty() {
        return ai_flows();
    }
    if bg {
        return spawn_background(args);
    }
    // A known flow name runs with the rest as input; anything else is free text
    // for the default pipeline (the run header names which flow actually ran).
    let (name, input) = if load_flow(&words[0]).is_ok() {
        (words[0].clone(), words[1..].join(" "))
    } else {
        (DEFAULT_FLOW.to_string(), words.join(" "))
    };
    let code = ai_run(false, None, Some(name), &input);
    if let Some(id) = record {
        jobs::finish(&id, code);
    }
    code
}

/// `aiTerminal ai flows` — list the available flows (project + global).
fn ai_flows() -> i32 {
    crate::config::Config::ensure_default();
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    let mut rows: Vec<(String, String, usize)> = Vec::new();
    let dir = crate::config::Config::flows_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml")).collect();
        files.sort();
        for f in files {
            let Some(name) = f.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
            if let Ok(text) = std::fs::read_to_string(&f) {
                if let Ok(flow) = parse_flow(&name, &text) {
                    rows.push((name, flow.description, flow.steps.len()));
                }
            }
        }
    }
    if rows.is_empty() {
        println!("{}", crate::i18n::translate("flow.none", &[crate::config::Config::flows_dir().display().to_string()]));
        return 0;
    }
    println!("{}", crate::i18n::translate("flow.header", &[rows.len().to_string()]));
    for (name, desc, n) in rows {
        println!("  {name:<20} {n} step(s)  {desc}");
    }
    println!("\n{}", crate::i18n::translate("flow.run_hint", &[]));
    0
}

// ===== @loop — an engineered agent loop (iterate until a verifiable goal) =====
//
// Loop engineering in one sentence: don't perfect a single prompt — design the
// loop the agent runs inside. The pieces this implementation supplies:
//
//   1. A VERIFIABLE GOAL.  `--check "<cmd>"` is a deterministic verifier (exit 0
//      = done). Without one, the maker/checker split kicks in: a SEPARATE
//      reviewer agent grades each iteration ("the model that wrote the code is
//      too nice grading its own homework").
//   2. STRUCTURED FEEDBACK.  The verifier's failure output (tail-capped) is fed
//      into the next iteration, so the agent works the failure, not the memory
//      of it.
//   3. STOP RULES.  Success (verifier passes) · `--max N` iteration cap ·
//      no-progress detection (identical verifier output twice in a row = stalled)
//      · an optional `--budget` token ceiling. Never an open-ended run.
//   4. GUARDRAILS.  The check command passes the command guard (deny blocks it;
//      confirm-tier is refused in this non-interactive path); the agent's tools
//      stay gated as in any run.
//   5. EXTERNAL STATE.  Run with `--bg` and the loop is a tracked job
//      (`@job` + a streamed log) like any workflow.

/// Options for `--loop` (parsed alongside the main `ai` flags).
struct LoopOpts {
    /// Deterministic verifier command; exit 0 = goal reached.
    check: Option<String>,
    /// Iteration cap (clamped 1..=25).
    max: u32,
    /// Optional total-token ceiling across all iterations.
    budget: Option<u64>,
    /// The maker agent (default `coder`).
    agent: String,
}

impl Default for LoopOpts {
    fn default() -> Self {
        LoopOpts { check: None, max: 5, budget: None, agent: "coder".into() }
    }
}

/// One iteration's verification outcome.
#[derive(Debug)]
struct Verdict {
    passed: bool,
    /// Feedback fed into the next iteration (failure output / reviewer notes).
    feedback: String,
    /// A signature of the verifier's observation — two identical consecutive
    /// signatures mean the loop is not making progress (stalled).
    signature: u64,
}

/// FNV-1a over a string — the no-progress signature.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Keep the LAST `max` chars of a verifier's output (failures live at the end).
fn tail(s: &str, max: usize) -> &str {
    let start = s.len().saturating_sub(max);
    // don't split a UTF-8 char
    let mut i = start;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

/// The maker prompt for iteration `k`: the goal, plus the previous iteration's
/// verifier feedback (the loop's structured feedback channel).
fn loop_prompt(goal: &str, k: u32, max: u32, check: Option<&str>, feedback: &str) -> String {
    let mut p = format!("## Goal (iteration {k} of at most {max})\n{goal}\n");
    if let Some(c) = check {
        p.push_str(&format!("\nThe goal is DONE when this command exits 0: `{c}`\n"));
    }
    if !feedback.trim().is_empty() {
        p.push_str(&format!(
            "\n## Verifier feedback from the previous iteration (fix this)\n```\n{}\n```\n",
            feedback.trim()
        ));
        p.push_str("Work the failures above. Do not redo work that already passed.\n");
    }
    p
}

/// Whether a reviewer's grade passes: the LAST `VERDICT:` line wins (the reviewer
/// may quote the format while explaining itself before concluding).
fn reviewer_passed(answer: &str) -> bool {
    answer
        .lines()
        .rev()
        .find_map(|l| {
            let t = l.trim().to_ascii_uppercase();
            t.strip_prefix("VERDICT:").map(|v| v.trim().starts_with("PASS"))
        })
        .unwrap_or(false)
}

/// The default wall-clock cap on the @loop verifier command.
const CHECK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(600);
/// Per-stream rolling-tail cap for `--check` output (the verdict reads the tail).
const CHECK_TAIL: usize = 64 * 1024;
/// How many `@<path>` attachments one prompt may carry (memory peaks at
/// N × raw + base64 + the request body copy).
const MAX_ATTACHMENTS: usize = 16;

/// Run the deterministic verifier: guard-check the command, run it via the shell
/// **bounded by `deadline`** (a hung check is killed and reported, never allowed
/// to stall the loop), and fold exit code + output tail into a [`Verdict`].
fn run_check(cmd: &str, policy: &crate::security::Policy, deadline: std::time::Duration) -> Result<Verdict, String> {
    match policy.check_command(cmd) {
        crate::security::Verdict::Deny { reason } => return Err(format!("check command blocked by guard: {reason}")),
        crate::security::Verdict::Confirm { reason } => {
            return Err(format!("check command needs confirmation ({reason}) — pick a safer --check"))
        }
        crate::security::Verdict::Allow => {}
    }
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("check command failed to launch: {e}"))?;
    // Drain both pipes on threads (so a chatty check can't dead-lock on a full
    // pipe), then wait with a deadline — the run_bounded pattern. Each drain keeps
    // only a rolling TAIL: a verifier that streams gigabytes costs constant memory
    // (the verdict only ever reads the last 4000 chars anyway).
    let take = |s: Option<std::process::ChildStdout>, e: Option<std::process::ChildStderr>| {
        let out = std::thread::spawn(move || {
            s.map(|h| crate::procio::read_tail(h, CHECK_TAIL)).unwrap_or_default()
        });
        let err = std::thread::spawn(move || {
            e.map(|h| crate::procio::read_tail(h, CHECK_TAIL)).unwrap_or_default()
        });
        (out, err)
    };
    let (out_h, err_h) = take(child.stdout.take(), child.stderr.take());
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if started.elapsed() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("check command timed out after {}s — pick a faster --check", deadline.as_secs()));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(format!("check command failed: {e}")),
        }
    };
    let mut text = out_h.join().unwrap_or_default();
    text.push_str(&err_h.join().unwrap_or_default());
    let passed = status.success();
    let observed = format!("exit={:?}\n{}", status.code(), tail(&text, 4000));
    Ok(Verdict { passed, feedback: observed.clone(), signature: fnv1a(&observed) })
}

/// The checker-agent verifier (no `--check` given): a SEPARATE reviewer agent
/// grades the maker's iteration against the goal and must conclude with
/// `VERDICT: PASS` or `VERDICT: CONTINUE` + feedback.
fn run_reviewer(sub: &SubAgentCtx, ctx: crate::caps::CapCtx, goal: &str, work: &str) -> Verdict {
    let prompt = format!(
        "You are the independent CHECKER in an agent loop (you did not do the work).\n\
         Goal:\n{goal}\n\nThe maker's latest iteration:\n{work}\n\n\
         Inspect the actual state with your read-only tools where possible — do not \
         trust the report alone. Conclude with EXACTLY one final line:\n\
         `VERDICT: PASS` if the goal is fully met, or `VERDICT: CONTINUE` followed by \
         the concrete gaps to fix (numbered, actionable)."
    );
    let answer = run_sub_agent(sub, ctx, 1, "reviewer", &prompt);
    let passed = reviewer_passed(&answer);
    Verdict { signature: fnv1a(&answer), feedback: answer, passed }
}

/// Why an engineered loop stopped.
#[derive(Debug, PartialEq)]
enum LoopOutcome {
    /// The verifier passed on iteration N.
    Done(u32),
    /// Identical verifier observation twice in a row — no progress.
    Stalled,
    /// The iteration cap was reached without passing.
    Exhausted,
    /// The token budget ran out.
    Budget,
    /// The verifier itself failed (e.g. the check command was guard-blocked).
    Error(String),
    /// The user interrupted (Ctrl+C).
    Cancelled,
}

/// A loop run's outcome plus the telemetry the footer shows: iterations, summed
/// tokens, and total tool calls across every iteration.
#[derive(Debug)]
struct LoopRun {
    outcome: LoopOutcome,
    iters: u32,
    tin: u64,
    tout: u64,
    tools: usize,
}

/// The transport-generic loop engine — the pure heart of `@loop`, separated from
/// the CLI plumbing so tests drive it with a [`ScriptedTransport`](crate::ai::ScriptedTransport)
/// mock and a scripted verifier (no model, no subprocess). `verify` receives the
/// maker's iteration answer and returns the verdict; `check_label` only shapes
/// the maker prompt. Returns the outcome plus accumulated telemetry.
fn drive_loop<T: crate::ai::Transport>(
    client: &crate::ai::Client<T>,
    maker: &crate::ai::AgentSpec,
    runner: &mut dyn crate::ai::ToolRunner,
    observer: &mut dyn crate::ai::AgentObserver,
    goal: &str,
    max: u32,
    budget: Option<u64>,
    check_label: Option<&str>,
    mut verify: impl FnMut(&str) -> Result<Verdict, String>,
) -> LoopRun {
    let mut feedback = String::new();
    let mut last_sig: Option<u64> = None;
    let mut spent: u64 = 0;
    let mut st = LoopRun { outcome: LoopOutcome::Exhausted, iters: 0, tin: 0, tout: 0, tools: 0 };
    for k in 1..=max {
        st.iters = k;
        eprintln!("\u{25B6} {}", crate::i18n::translate("loop.iteration", &[k.to_string(), max.to_string()]));
        let prompt = loop_prompt(goal, k, max, check_label, &feedback);
        let run = crate::ai::run_agent(client, maker, &prompt, "", runner, observer);
        st.tin += run.input_tokens as u64;
        st.tout += run.output_tokens as u64;
        st.tools += run.steps.len();
        spent += (run.input_tokens + run.output_tokens) as u64;
        // An errored/cancelled iteration is NOT work — never hand it to the
        // verifier as if it were; stop the loop with the real cause.
        match &run.outcome {
            crate::ai::RunOutcome::Cancelled => return LoopRun { outcome: LoopOutcome::Cancelled, ..st },
            crate::ai::RunOutcome::Error(e) => return LoopRun { outcome: LoopOutcome::Error(e.clone()), ..st },
            _ => {}
        }

        let verdict = match verify(&run.answer) {
            Ok(v) => v,
            Err(e) => return LoopRun { outcome: LoopOutcome::Error(e), ..st },
        };
        if verdict.passed {
            return LoopRun { outcome: LoopOutcome::Done(k), ..st };
        }
        // Stop rules: no progress (same observation twice), then budget, then cap.
        if last_sig == Some(verdict.signature) {
            return LoopRun { outcome: LoopOutcome::Stalled, ..st };
        }
        last_sig = Some(verdict.signature);
        feedback = verdict.feedback;
        if let Some(b) = budget {
            if spent >= b {
                return LoopRun { outcome: LoopOutcome::Budget, ..st };
            }
        }
    }
    st
}

/// `aiTerminal ai --loop "<goal>" [--check …] [--max N] [--budget N] [--agent …]`
/// — iterate the maker agent until the verifier passes or a stop rule fires.
/// Exit codes: 0 = goal reached; 1 = stalled/exhausted/budget; 2 = setup error.
fn run_loop_cli(goal: &str, opts: LoopOpts) -> i32 {
    // `@<path>` attachments work in loops too (images/PDFs + inlined text files).
    let (goal, media, file_ctx) = collect_attachments(goal);
    let goal = match file_ctx.is_empty() {
        true => goal,
        false => format!("{goal}\n{file_ctx}"),
    };
    let goal = goal.as_str();
    let cfg = crate::config::Config::load();
    crate::i18n::install(cfg.i18n_catalog());
    let settings = cfg.ai_settings();
    if settings.resolve_key().is_none() {
        eprintln!("aiTerminal: {}", crate::ai::setup_hint(&settings));
        return 2;
    }
    let max = opts.max.clamp(1, 25);
    let registry = crate::plugin::load_registry(&cfg);
    let policy = std::sync::Arc::new(crate::security::build_policy(&cfg, &registry));
    let workspace = std::env::current_dir().ok();
    let session = crate::ai::Session::for_cwd();
    let Some(mut maker) = build_agent_spec(&opts.agent) else {
        eprintln!("aiTerminal: no agent '{}' — {}", opts.agent, available_agents_hint());
        return 2;
    };
    // Give the maker this folder's remembered context (recent-run digest + folder-first
    // memory recall on the goal), redacted, folded into its system prompt — so the loop
    // starts knowing the project. `drive_loop`'s per-turn `context` stays empty (unchanged).
    let folder_mem = session.as_ref().map(|s| s.memory_dir());
    let folder_ctx = format!("{}{}", session_preamble(session.as_ref()), memory_preamble(&cfg, goal, folder_mem.as_deref()));
    if !folder_ctx.trim().is_empty() {
        let folder_ctx = policy.redact(&folder_ctx, crate::security::RedactScope::Ai);
        maker.system = format!("{}\n\n{}", maker.system.trim_end(), folder_ctx);
    }
    let cancel = crate::ai::CancelToken::new();
    let _sigint = wire_sigint(cancel.clone());
    let client = crate::ai::Client::new(settings.clone(), crate::ai::CurlTransport::default()).with_images(media).with_cancel(cancel);
    let mut runner = build_runner(&cfg, &settings, workspace, policy.clone(), true);
    if let Some(hub) = &runner.mcp {
        for (name, describe) in hub.tools() {
            maker.tools.push(crate::ai::ToolSpec { name, describe });
        }
    }

    eprintln!("\u{1F501} {}", crate::i18n::translate("loop.start", &[opts.agent.clone(), max.to_string()]));
    let started = std::time::Instant::now();
    // The verifier: the deterministic check wins; else the independent reviewer.
    let check = opts.check.clone();
    let sub = runner.sub.clone();
    let cap_ctx = runner.ctx.clone();
    let verify = |answer: &str| match &check {
        Some(cmd) => run_check(cmd, &cap_ctx.policy, CHECK_DEADLINE),
        None => Ok(run_reviewer(&sub, cap_ctx.clone(), goal, answer)),
    };
    let mut obs = CliObserver::new(std::io::stdout());
    let run = drive_loop(&client, &maker, &mut runner, &mut obs, goal, max, opts.budget, opts.check.as_deref(), verify);
    let _ = { use std::io::Write; std::io::stdout().write_all(b"\n") };
    let (dim, r) = (muted(), reset());
    let (code, glyph, digest) = match run.outcome {
        LoopOutcome::Done(k) => {
            eprintln!("\u{2713} {}", crate::i18n::translate("loop.done", &[k.to_string()]));
            (0, "\u{2713}", format!("goal reached in {k} iteration(s)"))
        }
        LoopOutcome::Stalled => {
            eprintln!("\u{26D4} {}", crate::i18n::translate("loop.stalled", &[]));
            (1, "\u{26a0}", "stalled (no progress)".into())
        }
        LoopOutcome::Budget => {
            eprintln!("\u{26D4} {}", crate::i18n::translate("loop.budget", &[]));
            (1, "\u{26a0}", "hit the token budget".into())
        }
        LoopOutcome::Exhausted => {
            eprintln!("\u{26D4} {}", crate::i18n::translate("loop.exhausted", &[]));
            (1, "\u{26a0}", "exhausted the iteration cap".into())
        }
        LoopOutcome::Error(e) => {
            eprintln!("aiTerminal: {e}");
            (2, "\u{2717}", "error".into())
        }
        LoopOutcome::Cancelled => {
            eprintln!("\u{23f9} interrupted");
            (130, "\u{23f9}", "interrupted".into())
        }
    };
    // The same footer as agent/flow, with iterations in place of a lone elapsed count.
    let cost = Some(client.model().cost(run.tin, run.tout));
    let footer = run_footer_with(glyph, started.elapsed(), run.tools, run.tin, run.tout, cost, cfg.ai_budget);
    eprintln!("{dim}{footer} \u{b7} {} iteration{}{r}", run.iters, if run.iters == 1 { "" } else { "s" });
    record_session_run(session.as_ref(), "@loop", goal, &digest);
    code
}

// ===== background jobs (run + track + monitor from the terminal) =============

/// `--bg`: relaunch this invocation detached with stdout+stderr redirected to the
/// job's log, record it under `ai/jobs/<id>/`, and print how to monitor it.
fn spawn_background(args: &[String]) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aiTerminal: can't resolve the binary path: {e}");
            return 1;
        }
    };
    let id = jobs::new_id();
    let Some(dir) = jobs::dir(&id) else {
        eprintln!("aiTerminal: bad job id");
        return 1;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("aiTerminal: can't create the job dir: {e}");
        return 1;
    }
    let log_path = dir.join("log.md");
    let log = match std::fs::File::create(&log_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("aiTerminal: can't create the job log: {e}");
            return 1;
        }
    };
    let err = log.try_clone().unwrap_or_else(|_| std::fs::File::create(dir.join("log.md")).unwrap());
    // The child re-runs `ai` without `--bg`, plus the record marker it stamps on exit.
    let mut child_args: Vec<String> = vec!["ai".into()];
    child_args.extend(args.iter().filter(|a| a.as_str() != "--bg").cloned());
    child_args.push("--job-record".into());
    child_args.push(id.clone());
    // Detach into its OWN SESSION so closing this terminal never SIGHUPs the job.
    let spawned = platform::os::spawn_detached(&exe, &child_args, log, err);
    match spawned {
        Ok(child_pid) => {
            jobs::record_start(&id, args, child_pid);
            println!("\u{25B6} background job {id}");
            println!("  monitor: aiTerminal ai job     ·  tail -f {}", log_path.display());
            0
        }
        Err(e) => {
            eprintln!("aiTerminal: failed to launch the background job: {e}");
            1
        }
    }
}

/// Current unix time (seconds).
fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Parse a natural delay / clock-time phrase out of a `@job` prompt and return the absolute
/// unix fire-time plus the prompt with that phrase removed — so `@job … in 2 minutes`
/// schedules and the agent sees a clean task. Recognizes "in|after <n> sec|min|hour|day(s)"
/// (or a fused `30s`/`2min`) and "at HH[:MM][am/pm]". No match → `(None, prompt)` (run now).
fn parse_schedule(prompt: &str, now: u64) -> (Option<u64>, String) {
    let words: Vec<&str> = prompt.split_whitespace().collect();
    for i in 0..words.len() {
        let kw = words[i].to_ascii_lowercase();
        if kw == "in" || kw == "after" {
            if let Some((secs, used)) = parse_delay(&words[i + 1..]) {
                return (Some(now + secs), join_excluding(&words, i, i + 1 + used));
            }
        } else if kw == "at" {
            if let Some(word) = words.get(i + 1) {
                if let Some(fire) = parse_clock_at(word, now) {
                    return (Some(fire), join_excluding(&words, i, i + 2));
                }
            }
        }
    }
    (None, prompt.to_string())
}

/// Parse a relative delay from the words after `in`/`after` → `(seconds, words_consumed)`.
fn parse_delay(rest: &[&str]) -> Option<(u64, usize)> {
    let first = rest.first()?;
    if let Some((n, unit)) = split_num_unit(first) {
        return unit_secs(unit, n).map(|s| (s, 1));
    }
    let n: u64 = first.parse().ok()?;
    let unit = rest.get(1)?;
    unit_secs(unit, n).map(|s| (s, 2))
}

/// Split a fused `30s` / `2min` / `1h` into `(number, unit)`; `None` if not that shape.
fn split_num_unit(w: &str) -> Option<(u64, &str)> {
    let split = w.find(|c: char| !c.is_ascii_digit())?;
    if split == 0 {
        return None;
    }
    let n: u64 = w[..split].parse().ok()?;
    Some((n, &w[split..]))
}

/// Seconds for `n` of a time unit (`s/sec/min/m/hour/h/day/d`, plural OK); `None` if unknown.
fn unit_secs(unit: &str, n: u64) -> Option<u64> {
    let mult = match unit.to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => 86400,
        _ => return None,
    };
    Some(n * mult)
}

/// Parse a clock time (`17:30`, `5pm`, `9`, `9am`) → the next unix time it occurs (today,
/// or tomorrow if already past), using the local UTC offset. `None` if not a clock time.
fn parse_clock_at(word: &str, now: u64) -> Option<u64> {
    let w = word.to_ascii_lowercase();
    let (body, ampm) = if let Some(b) = w.strip_suffix("pm") {
        (b, Some(true))
    } else if let Some(b) = w.strip_suffix("am") {
        (b, Some(false))
    } else {
        (w.as_str(), None)
    };
    let (h_str, m_str) = body.split_once(':').unwrap_or((body, "0"));
    let mut hour: i64 = h_str.parse().ok()?;
    let min: i64 = m_str.parse().ok()?;
    if !(0..=23).contains(&hour) || !(0..=59).contains(&min) {
        return None;
    }
    match ampm {
        Some(true) if hour < 12 => hour += 12, // 5pm → 17
        Some(false) if hour == 12 => hour = 0, // 12am → 0
        _ => {}
    }
    let offset = platform::os::utc_offset_secs();
    let local_now = now as i64 + offset;
    let day_start = local_now - local_now.rem_euclid(86400);
    let mut target = day_start + hour * 3600 + min * 60;
    if target <= local_now {
        target += 86400; // already passed today → tomorrow
    }
    Some((target - offset) as u64)
}

/// Rejoin `words` skipping the half-open range `[start, end)` (the schedule phrase).
fn join_excluding(words: &[&str], start: usize, end: usize) -> String {
    words.iter().enumerate().filter(|(i, _)| *i < start || *i >= end).map(|(_, w)| *w).collect::<Vec<_>>().join(" ")
}

/// What an `ai job …` invocation asks for. Pure parse, so the intuitive grammar
/// (`@job <task> --agent x --bg`, flags anywhere) is unit-testable.
#[derive(Debug, PartialEq)]
enum JobCmd {
    List,
    Clear,
    Cancel(String),
    Run { prompt: String, agent: String, bg: bool, record: Option<String>, run_at: Option<u64> },
}

fn parse_job_args(args: &[String]) -> JobCmd {
    match args.first().map(String::as_str) {
        None => return JobCmd::List,
        Some("clear") if args.len() == 1 => return JobCmd::Clear,
        Some("cancel") => return JobCmd::Cancel(args.get(1).cloned().unwrap_or_default()),
        _ => {}
    }
    let mut agent = "coder".to_string();
    let mut bg = false;
    let mut record = None;
    let mut run_at = None;
    let mut words: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--agent" => {
                if let Some(n) = it.next() {
                    agent = n.clone();
                }
            }
            "--bg" => bg = true,
            "--job-record" => record = it.next().cloned(),
            "--run-at" => run_at = it.next().and_then(|s| s.parse().ok()),
            w => words.push(w),
        }
    }
    JobCmd::Run { prompt: words.join(" "), agent, bg, record, run_at }
}

/// `@job` — the tracked-task surface. `@job` lists, `@job clear` prunes, and
/// `@job <task> [--agent <name>] [--bg]` RUNS the task as a recorded job:
/// foreground runs stream live while their answer also lands in the job's log;
/// `--bg` detaches exactly like any run. `args` includes the leading "job" word.
fn ai_job_cmd(args: &[String]) -> i32 {
    match parse_job_args(&args[1..]) {
        JobCmd::List => ai_jobs(&[]),
        JobCmd::Clear => ai_jobs(&["clear".to_string()]),
        JobCmd::Cancel(id) => jobs::cancel(&id),
        JobCmd::Run { prompt, agent, bg, record, run_at } => {
            if prompt.trim().is_empty() {
                eprintln!("usage: @job <task> [--agent <name>] [--bg]  ·  @job [clear]  ·  @job cancel <id>");
                return 2;
            }
            // A detached CHILD carrying a fire-time: wait until then, then run.
            if let (Some(id), Some(at)) = (record.as_ref(), run_at) {
                return run_scheduled_child(id, &agent, &prompt, at);
            }
            // The USER's initial call: a natural "in 2 minutes" / "at 5pm" defers the job.
            if record.is_none() {
                let (at, cleaned) = parse_schedule(&prompt, unix_now());
                if let Some(at) = at {
                    return spawn_scheduled(&cleaned, &agent, at);
                }
            }
            if bg {
                // Re-enter detached: the child comes back through this path
                // (minus --bg, plus --job-record) with its output in the log.
                return spawn_background(args);
            }
            // Foreground, but TRACKED: record the job, tee the streamed answer
            // into its log, and stamp the outcome.
            let (id, log) = match record {
                Some(id) => (id, None), // a detached child logs via stdio redirection
                None => {
                    let id = jobs::new_id();
                    let log = jobs::dir(&id)
                        .and_then(|d| std::fs::create_dir_all(&d).ok().map(|_| d))
                        .and_then(|d| std::fs::File::create(d.join("log.md")).ok());
                    jobs::record_start(&id, &args[1..], std::process::id());
                    (id, log)
                }
            };
            let code = run_prompt_as_agent(&agent, &prompt, log);
            jobs::finish(&id, code);
            code
        }
    }
}

/// Schedule a job to fire at `run_at`: record it as `scheduled` and detach a child that
/// sleeps until the fire-time, then runs (reusing this same `@job` path via `--run-at`).
/// The detached child inherits the current working directory, so the job runs in the folder
/// where `@job` was typed.
fn spawn_scheduled(prompt: &str, agent: &str, run_at: u64) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aiTerminal: can't resolve the binary path: {e}");
            return 1;
        }
    };
    let id = jobs::new_id();
    let Some(dir) = jobs::dir(&id) else {
        eprintln!("aiTerminal: bad job id");
        return 1;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("aiTerminal: can't create the job dir: {e}");
        return 1;
    }
    let log_path = dir.join("log.md");
    let Ok(log) = std::fs::File::create(&log_path) else {
        eprintln!("aiTerminal: can't create the job log");
        return 1;
    };
    let err = log.try_clone().unwrap_or_else(|_| std::fs::File::create(&log_path).unwrap());
    let child_args: Vec<String> = vec![
        "ai".into(),
        "job".into(),
        prompt.into(),
        "--agent".into(),
        agent.into(),
        "--job-record".into(),
        id.clone(),
        "--run-at".into(),
        run_at.to_string(),
    ];
    match platform::os::spawn_detached(&exe, &child_args, log, err) {
        Ok(child_pid) => {
            jobs::record_scheduled(&id, prompt, child_pid, run_at);
            let wait = run_at.saturating_sub(unix_now());
            eprintln!("{}\u{29D6} scheduled job {id} — fires in {}{}", accent(), jobs::human_age(wait), reset());
            eprintln!("  list: aiTerminal ai job  ·  cancel: @job cancel {id}");
            0
        }
        Err(e) => {
            eprintln!("aiTerminal: failed to schedule the job: {e}");
            1
        }
    }
}

/// The detached scheduled child: wait until `run_at` (polling so a cancel is noticed
/// promptly), then flip the record to `running` and execute the task.
fn run_scheduled_child(id: &str, agent: &str, prompt: &str, run_at: u64) -> i32 {
    loop {
        let now = unix_now();
        if now >= run_at {
            break;
        }
        // Cancelled out from under us (`@job cancel`)? Stop without running.
        if jobs::status_of(id).as_deref() != Some("scheduled") {
            return 130;
        }
        std::thread::sleep(std::time::Duration::from_secs((run_at - now).min(2)));
    }
    jobs::mark_running(id);
    let code = run_prompt_as_agent(agent, prompt, None);
    jobs::finish(id, code);
    code
}

/// Run `prompt` through `agent` with the full live chrome; when `log` is set the
/// streamed answer is ALSO written there (the foreground-tracked job's record).
fn run_prompt_as_agent(agent: &str, prompt: &str, log: Option<std::fs::File>) -> i32 {
    let (prompt, media, file_ctx) = collect_attachments(prompt);
    let cfg = crate::config::Config::load();
    crate::i18n::install(cfg.i18n_catalog());
    let settings = cfg.ai_settings();
    if settings.resolve_key().is_none() {
        eprintln!("aiTerminal: {}", crate::ai::setup_hint(&settings));
        return 2;
    }
    let registry = crate::plugin::load_registry(&cfg);
    let policy = std::sync::Arc::new(crate::security::build_policy(&cfg, &registry));
    // A job gets the same grounding as any AI run: global instructions + this folder's
    // session digest + folder-first memory recall + attachments (all redacted). Its
    // `memory.*` writes are folder-scoped via `build_runner`.
    let session = crate::ai::Session::for_cwd();
    let folder_mem = session.as_ref().map(|s| s.memory_dir());
    let ctx = format!(
        "{}{}{}{file_ctx}",
        instructions_preamble(),
        session_preamble(session.as_ref()),
        memory_preamble(&cfg, &prompt, folder_mem.as_deref()),
    );
    let ctx = policy.redact(&ctx, crate::security::RedactScope::Ai);
    let code = run_agent_streaming(&cfg, settings, agent, &prompt, &ctx, std::env::current_dir().ok(), policy, media, log);
    record_session_run(session.as_ref(), "@job", &prompt, &outcome_label(code));
    code
}

/// `aiTerminal ai job [clear]` — list background jobs (newest first), or prune
/// the finished ones.
fn ai_jobs(args: &[String]) -> i32 {
    crate::config::Config::ensure_default();
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    if args.first().map(String::as_str) == Some("clear") {
        let n = jobs::clear_finished();
        println!("{}", crate::i18n::translate("job.cleared", &[n.to_string()]));
        return 0;
    }
    let list = jobs::list();
    if list.is_empty() {
        println!("{}", crate::i18n::translate("job.none", &[]));
        return 0;
    }
    println!("{}", crate::i18n::translate("job.header", &[list.len().to_string()]));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (dim, r) = (muted(), reset());
    for j in &list {
        // One glanceable line per job: glyph · id · status · command (truncated) · timing;
        // the log path rides a dim second line so the row stays scannable.
        let cmd: String = if j.cmd.chars().count() > 44 {
            format!("{}\u{2026}", j.cmd.chars().take(43).collect::<String>())
        } else {
            j.cmd.clone()
        };
        println!("  {} {:<10} {:<9} {}  {dim}({}){r}", j.status_glyph(), j.id, j.status, cmd, j.timing(now));
        println!("      {dim}log: {}{r}", j.log.display());
    }
    0
}

/// The on-disk job records behind `--bg` / `ai jobs`: `ai/jobs/<id>/{job.toml,log.md}`.
mod jobs {
    pub(super) struct Job {
        pub id: String,
        pub status: String,
        pub cmd: String,
        pub started: u64,
        pub finished: Option<u64>,
        pub run_at: Option<u64>,
        pub log: std::path::PathBuf,
    }

    impl Job {
        /// A glanceable timing note: a `scheduled` job shows when it fires; others show
        /// when they started and how long they ran (or have been running).
        pub fn timing(&self, now: u64) -> String {
            if self.status == "scheduled" {
                if let Some(at) = self.run_at {
                    return if at > now {
                        format!("fires in {}", human_age(at - now))
                    } else {
                        "due".to_string()
                    };
                }
            }
            let ago = human_age(now.saturating_sub(self.started));
            let dur = human_age(self.finished.unwrap_or(now).saturating_sub(self.started));
            format!("{ago} ago \u{b7} {dur}")
        }
    }

    /// `95` → `1m`, `4000` → `1h` — coarse, glanceable durations.
    pub(super) fn human_age(secs: u64) -> String {
        if secs >= 3600 {
            format!("{}h", secs / 3600)
        } else if secs >= 60 {
            format!("{}m", secs / 60)
        } else {
            format!("{secs}s")
        }
    }

    impl Job {
        pub fn status_glyph(&self) -> &'static str {
            match self.status.as_str() {
                "running" => "\u{25B6}",
                "done" => "\u{2713}",
                "scheduled" => "\u{29D6}", // ⧖ waiting to fire
                "cancelled" => "\u{23f9}",
                "missed" => "\u{26A0}", // ⚠ its scheduler died before it fired
                _ => "\u{2717}", // failed / died
            }
        }
    }

    fn now() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }

    /// A fresh, sortable job id: `<unix-secs>-<pid>`.
    pub(super) fn new_id() -> String {
        format!("{}-{}", now(), std::process::id())
    }

    /// The job's folder (id is charset-checked so it can't escape the jobs dir).
    pub(super) fn dir(id: &str) -> Option<std::path::PathBuf> {
        let ok = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        ok.then(|| crate::config::Config::jobs_dir().join(id))
    }

    /// Write the initial `job.toml` (status `running`).
    pub(super) fn record_start(id: &str, args: &[String], pid: u32) {
        let Some(dir) = dir(id) else { return };
        let cmd = args.iter().filter(|a| a.as_str() != "--bg").cloned().collect::<Vec<_>>().join(" ");
        let doc = corelib::wire::Toml::Table(vec![
            ("cmd".into(), corelib::wire::Toml::Str(cmd)),
            ("status".into(), corelib::wire::Toml::Str("running".into())),
            ("started".into(), corelib::wire::Toml::Int(now() as i64)),
            ("pid".into(), corelib::wire::Toml::Int(pid as i64)),
        ]);
        let _ = std::fs::write(dir.join("job.toml"), doc.to_string());
    }

    /// Write a `scheduled` record: the sleeper's pid, the fire-time, and the cwd it will
    /// run in (persisted so the list can show it and a later run lands in the right folder).
    pub(super) fn record_scheduled(id: &str, cmd: &str, pid: u32, run_at: u64) {
        let Some(dir) = dir(id) else { return };
        let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
        let doc = corelib::wire::Toml::Table(vec![
            ("cmd".into(), corelib::wire::Toml::Str(cmd.into())),
            ("status".into(), corelib::wire::Toml::Str("scheduled".into())),
            ("started".into(), corelib::wire::Toml::Int(now() as i64)),
            ("pid".into(), corelib::wire::Toml::Int(pid as i64)),
            ("run_at".into(), corelib::wire::Toml::Int(run_at as i64)),
            ("cwd".into(), corelib::wire::Toml::Str(cwd)),
        ]);
        let _ = std::fs::write(dir.join("job.toml"), doc.to_string());
    }

    /// Flip a scheduled record to `running` when its fire-time arrives (no `finished` stamp).
    pub(super) fn mark_running(id: &str) {
        set_status(id, "running", None);
    }

    /// The current status string of a job record, if any (used by the sleeper to notice a cancel).
    pub(super) fn status_of(id: &str) -> Option<String> {
        let dir = dir(id)?;
        let text = std::fs::read_to_string(dir.join("job.toml")).ok()?;
        let doc = corelib::wire::Toml::parse(&text).ok()?;
        doc.get("status").and_then(|v| v.as_str()).map(str::to_string)
    }

    /// Cancel a scheduled or running job: SIGTERM its process and mark it `cancelled`.
    pub(super) fn cancel(id: &str) -> i32 {
        let Some(dir) = dir(id) else {
            eprintln!("aiTerminal: bad job id");
            return 2;
        };
        let Ok(text) = std::fs::read_to_string(dir.join("job.toml")) else {
            eprintln!("aiTerminal: no such job '{id}'");
            return 2;
        };
        let Ok(doc) = corelib::wire::Toml::parse(&text) else { return 2 };
        let status = doc.get("status").and_then(|v| v.as_str()).unwrap_or("?");
        if !matches!(status, "scheduled" | "running") {
            println!("job {id} is already {status}");
            return 0;
        }
        let pid = doc.get("pid").and_then(|v| v.as_int()).unwrap_or(0).max(0) as u32;
        if pid > 0 {
            platform::os::terminate(pid);
        }
        set_status(id, "cancelled", Some(130));
        println!("\u{23f9} cancelled job {id}");
        0
    }

    /// Stamp a job's outcome. Exit 130 (interrupt) records as `cancelled`.
    pub(super) fn finish(id: &str, code: i32) {
        set_status(id, if code == 0 { "done" } else if code == 130 { "cancelled" } else { "failed" }, Some(code));
    }

    /// Rewrite a job record's status (keeping cmd/started/pid, and run_at/cwd when present).
    /// A terminal status stamps `finished`; a transition to `running` does not.
    pub(super) fn set_status(id: &str, status: &str, code: Option<i32>) {
        let Some(dir) = dir(id) else { return };
        let path = dir.join("job.toml");
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(doc) = corelib::wire::Toml::parse(&text) else { return };
        let get = |k: &str| doc.get(k).cloned();
        let mut pairs = vec![
            ("cmd".into(), get("cmd").unwrap_or(corelib::wire::Toml::Str(String::new()))),
            ("status".into(), corelib::wire::Toml::Str(status.into())),
            ("started".into(), get("started").unwrap_or(corelib::wire::Toml::Int(0))),
            ("pid".into(), get("pid").unwrap_or(corelib::wire::Toml::Int(0))),
        ];
        if let Some(v) = get("run_at") {
            pairs.push(("run_at".into(), v));
        }
        if let Some(v) = get("cwd") {
            pairs.push(("cwd".into(), v));
        }
        // Only a finished/terminal state carries a `finished` stamp; `running` is in-flight.
        if status != "running" && status != "scheduled" {
            pairs.push(("finished".into(), corelib::wire::Toml::Int(now() as i64)));
        }
        if let Some(c) = code {
            pairs.push(("exit".into(), corelib::wire::Toml::Int(c as i64)));
        }
        let _ = std::fs::write(path, corelib::wire::Toml::Table(pairs).to_string());
    }

    /// Every recorded job, newest first — RECONCILED: a "running" record whose
    /// pid is no longer alive (crash, SIGKILL, reboot) is healed to `died` on
    /// the spot, so the list never lies and `clear` can prune it.
    pub(super) fn list() -> Vec<Job> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(crate::config::Config::jobs_dir()) else { return out };
        for e in entries.flatten() {
            let dir = e.path();
            let Some(id) = e.file_name().to_str().map(str::to_string) else { continue };
            let Ok(text) = std::fs::read_to_string(dir.join("job.toml")) else { continue };
            let Ok(doc) = corelib::wire::Toml::parse(&text) else { continue };
            let mut status = doc.get("status").and_then(|v| v.as_str()).unwrap_or("?").to_string();
            // Reconcile: a `running` or `scheduled` record whose owner/sleeper pid is gone
            // (crash, kill, reboot) is healed so the list never lies — a dead running job is
            // `died`, a dead scheduled job will never fire so it is `missed`.
            if status == "running" || status == "scheduled" {
                let pid = doc.get("pid").and_then(|v| v.as_int()).unwrap_or(0).max(0) as u32;
                if !platform::os::pid_alive(pid) {
                    let healed = if status == "scheduled" { "missed" } else { "died" };
                    set_status(&id, healed, None);
                    status = healed.into();
                }
            }
            out.push(Job {
                id,
                status,
                cmd: doc.get("cmd").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                started: doc.get("started").and_then(|v| v.as_int()).unwrap_or(0).max(0) as u64,
                finished: doc.get("finished").and_then(|v| v.as_int()).map(|n| n.max(0) as u64),
                run_at: doc.get("run_at").and_then(|v| v.as_int()).map(|n| n.max(0) as u64),
                log: dir.join("log.md"),
            });
        }
        out.sort_by(|a, b| b.started.cmp(&a.started).then(b.id.cmp(&a.id)));
        out
    }

    /// Remove every job in a terminal state; keeps in-flight `running` and pending
    /// `scheduled` jobs. Returns how many were pruned.
    pub(super) fn clear_finished() -> usize {
        let mut n = 0;
        for j in list() {
            if !matches!(j.status.as_str(), "running" | "scheduled") {
                if let Some(d) = dir(&j.id) {
                    if std::fs::remove_dir_all(d).is_ok() {
                        n += 1;
                    }
                }
            }
        }
        n
    }
}

// ===== profiles ==============================================================

/// `aiTerminal profile <list|current|create|rename|delete|edit|switch|<id>>` —
/// manage the named terminal profiles (config overlay + saved workspace) entirely
/// from the prompt. `@profile <id>` switches directly; `@profile edit [id]` opens
/// the profile's config overlay in `$EDITOR`. A running window follows switches
/// AND overlay edits live (it polls the pointer + config mtimes each second).
/// Resolve a user-typed profile reference — an exact id, or a display name
/// (case-insensitive) — to the profile id.
fn resolve_profile(word: &str) -> Option<String> {
    crate::profile::list()
        .into_iter()
        .find(|p| p.id == word || p.name.eq_ignore_ascii_case(word))
        .map(|p| p.id)
}

/// Switch to a profile by id-or-name, with the shared success/error reporting.
fn profile_switch(word: &str) -> i32 {
    let Some(id) = resolve_profile(word) else {
        eprintln!("no profile '{word}' — see them with: @profile");
        return 2;
    };
    match crate::profile::set_active(&id) {
        Ok(()) => {
            println!("{}", crate::i18n::translate("profile.switched", &[id]));
            0
        }
        Err(e) => {
            eprintln!("switch failed: {e}");
            1
        }
    }
}

pub fn profile(args: &[String]) -> i32 {
    crate::config::Config::ensure_default();
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => {
            let active = crate::profile::active_id();
            let all = crate::profile::list();
            println!("{}", crate::i18n::translate("profile.list_header", &[crate::config::Config::profiles_dir().display().to_string(), all.len().to_string()]));
            for p in all {
                let mark = if p.id == active { "\u{25CF}" } else { "\u{25CB}" };
                println!("  {mark} {} {:<16} ({})", p.emoji, p.name, p.id);
            }
            println!("\n{}", crate::i18n::translate("profile.switch_hint", &[]));
            0
        }
        "current" => {
            let id = crate::profile::active_id();
            println!("{id}");
            0
        }
        "create" => match args.get(1) {
            Some(name) => {
                let emoji = args.get(2).map(String::as_str).unwrap_or("");
                match crate::profile::create(name, emoji) {
                    Ok(p) => {
                        println!("created profile '{}' ({}) — switch with: aiTerminal profile switch {}", p.name, p.id, p.id);
                        println!("its config overlay: {}", crate::profile::config_path(&p.id).unwrap().display());
                        0
                    }
                    Err(e) => {
                        eprintln!("create failed: {e}");
                        1
                    }
                }
            }
            None => {
                eprintln!("usage: aiTerminal profile create <name> [emoji]");
                2
            }
        },
        "rename" => match (args.get(1), args.get(2)) {
            (Some(id), Some(name)) => {
                let emoji = args.get(3).map(String::as_str).unwrap_or("");
                match crate::profile::update(id, name, emoji) {
                    Ok(()) => {
                        println!("renamed profile '{id}'");
                        0
                    }
                    Err(e) => {
                        eprintln!("rename failed: {e}");
                        1
                    }
                }
            }
            _ => {
                eprintln!("usage: aiTerminal profile rename <id> <new-name> [emoji]");
                2
            }
        },
        "delete" => match args.get(1) {
            Some(id) => match crate::profile::delete(id) {
                Ok(()) => {
                    println!("deleted profile '{id}'");
                    0
                }
                Err(e) => {
                    eprintln!("delete failed: {e}");
                    1
                }
            },
            None => {
                eprintln!("usage: aiTerminal profile delete <id>");
                2
            }
        },
        // `@profile edit [id]` — open the profile's config overlay in $EDITOR. The
        // window applies the saved changes live (config-mtime polling), so this IS
        // the profile settings surface: a TOML file in your editor, nothing else.
        "edit" => {
            let id = args.get(1).cloned().unwrap_or_else(crate::profile::active_id);
            let Some(path) = crate::profile::config_path(&id).filter(|p| p.exists()) else {
                eprintln!("no profile '{id}' (list them with: aiTerminal profile list)");
                return 2;
            };
            let editor = std::env::var("EDITOR").ok().filter(|e| !e.trim().is_empty()).unwrap_or_else(|| "vi".into());
            // $EDITOR may carry flags (e.g. "code --wait") — split words.
            let mut parts = editor.split_whitespace();
            let bin = parts.next().unwrap_or("vi").to_string();
            let status = std::process::Command::new(&bin).args(parts).arg(&path).status();
            match status {
                Ok(st) if st.success() => {
                    println!("{}", path.display());
                    println!("saved — a running window applies it within a second");
                    0
                }
                Ok(_) => 1,
                Err(e) => {
                    eprintln!("couldn't launch {bin}: {e}\nedit the file directly: {}", path.display());
                    1
                }
            }
        }
        "switch" => match args.get(1) {
            Some(word) => profile_switch(word),
            None => {
                eprintln!("usage: @profile <id>   (or: @profile switch <id>)");
                2
            }
        },
        // `@profile <id-or-name>` switches directly (the switch verb still works).
        other => {
            if resolve_profile(other).is_none() {
                eprintln!("no profile '{other}'. try: list, current, create, rename, delete, edit — or a profile id/name to switch");
                return 2;
            }
            profile_switch(other)
        }
    }
}

/// `aiTerminal plugin <list|install|enable|disable|remove|info>`.
pub fn plugin(args: &[String]) -> i32 {
    let store = match crate::plugin::store::PluginStore::open_default() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("plugin store error: {e}");
            return 1;
        }
    };
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => {
            // Every plugin is loaded dynamically from ~/.aiTerminal/plugins/;
            // nothing is built into the binary. Install by copying a plugin folder
            // into the plugins dir.
            let installed = store.installed();
            println!("plugins in {} ({}):", crate::config::Config::plugins_dir().display(), installed.len());
            if installed.is_empty() {
                println!("  (none) — copy a plugin folder into the plugins dir");
            }
            for p in installed {
                let mark = if p.enabled { "●" } else { "○" };
                println!("  {mark} {:<16} {:<8} {}", p.name, p.version, p.description);
            }
            0
        }
        "install" => match args.get(1) {
            Some(path) => match store.install(Path::new(path)) {
                Ok(name) => {
                    println!("installed plugin '{name}' (restart to load)");
                    0
                }
                Err(e) => {
                    eprintln!("install failed: {e}");
                    1
                }
            },
            None => {
                eprintln!("usage: aiTerminal plugin install <path-to.toml | path-to.tplugin>");
                1
            }
        },
        "enable" | "disable" => match args.get(1) {
            Some(name) => {
                let on = sub == "enable";
                match store.set_enabled(name, on) {
                    Ok(()) => {
                        println!("{} plugin '{name}'", if on { "enabled" } else { "disabled" });
                        0
                    }
                    Err(e) => {
                        eprintln!("failed: {e}");
                        1
                    }
                }
            }
            None => {
                eprintln!("usage: aiTerminal plugin {sub} <name>");
                1
            }
        },
        "remove" => match args.get(1) {
            Some(name) if store.remove(name) => {
                println!("removed plugin '{name}'");
                0
            }
            Some(name) => {
                eprintln!("plugin '{name}' not found");
                1
            }
            None => {
                eprintln!("usage: aiTerminal plugin remove <name>");
                1
            }
        },
        "info" => match args.get(1) {
            Some(name) => match store.installed().into_iter().find(|p| &p.name == name) {
                Some(p) => {
                    println!("{}  v{}\n{}\nenabled: {}", p.name, p.version, p.description, p.enabled);
                    0
                }
                None => {
                    eprintln!("plugin '{name}' not installed");
                    1
                }
            },
            None => {
                eprintln!("usage: aiTerminal plugin info <name>");
                1
            }
        },
        other => {
            eprintln!("unknown subcommand '{other}'. try: list, install, enable, disable, remove, info");
            1
        }
    }
}

/// `aiTerminal config [path]` — show config location + current values.
pub fn config(args: &[String]) -> i32 {
    let created = crate::config::Config::ensure_default();
    let path = crate::config::Config::path();
    if args.first().map(String::as_str) == Some("path") {
        println!("{}", path.display());
        return 0;
    }
    let c = crate::config::Config::load();
    if created {
        println!("created default config at {}", path.display());
    }
    println!("config: {}", path.display());
    println!("  theme       = {}", c.theme);
    println!("  font_family = {}", c.font_family);
    println!("  font_size   = {}", c.font_size);
    println!("  zoom        = {}", c.zoom);
    println!("  tab_bar     = {}", c.tab_bar);
    println!("  shell       = {}", if c.shell.is_empty() { "$SHELL".to_string() } else { c.shell.clone() });
    println!("  scrollback  = {}", c.scrollback);
    println!("\nedit the file, then reload in the app with Cmd-, (or restart)");
    0
}

/// `aiTerminal theme [<name> | list | path | export <name>]` — list themes, or
/// SWITCH the active profile's theme (`@theme nord`): the name is validated, the
/// profile's config overlay is updated, and a running window applies it live
/// (it follows config-file changes each second).
pub fn theme(args: &[String]) -> i32 {
    crate::config::Config::ensure_default();
    let cfg = crate::config::Config::load();
    crate::i18n::install(cfg.i18n_catalog());
    match args.first().map(String::as_str) {
        Some("path") => {
            println!("{}", crate::config::Config::themes_dir().display());
            return 0;
        }
        // `theme export <name>` — print the COMPLETE, normalized theme TOML (every token
        // resolved, including the derived depth + file-type colors), so the file is a full
        // editable reference. Curated values are preserved; only missing tokens are filled.
        Some("export") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: aiTerminal theme export <name>");
                return 2;
            };
            print!("{}", crate::config::Config::resolve_theme(name).to_toml());
            return 0;
        }
        // `theme <name>` (or `theme set <name>`) — switch the active profile's theme.
        Some(word) if word != "list" => {
            let name = if word == "set" {
                match args.get(1) {
                    Some(n) => n.clone(),
                    None => {
                        eprintln!("usage: aiTerminal theme set <name>");
                        return 2;
                    }
                }
            } else {
                word.to_string()
            };
            return theme_set(&name);
        }
        _ => {}
    }
    let active = cfg.theme;
    let user = crate::config::Config::user_theme_names();
    println!("themes in {} ({}):", crate::config::Config::themes_dir().display(), user.len());
    for n in &user {
        let mark = if n.eq_ignore_ascii_case(&active) { "\u{25CF}" } else { "\u{25CB}" };
        println!("  {mark} {n}");
    }
    println!("\n{}", crate::i18n::translate("theme.switch_hint", &[]));
    0
}

/// Switch the ACTIVE profile's theme (its config overlay — so each profile keeps
/// its own look). The name must exist; a running window follows within a second.
fn theme_set(name: &str) -> i32 {
    let available = crate::config::Config::user_theme_names();
    let Some(canonical) = available.iter().find(|n| n.eq_ignore_ascii_case(name)) else {
        eprintln!("{}", crate::i18n::translate("theme.unknown", &[name.to_string(), available.join(", ")]));
        return 2;
    };
    let active = crate::profile::active_id();
    let rendered = format!("\"{}\"", canonical.replace('\\', "\\\\").replace('"', "\\\""));
    if let Err(e) = crate::profile::config_set(&active, "appearance", "theme", &rendered) {
        eprintln!("aiTerminal: {e}");
        return 1;
    }
    println!("{}", crate::i18n::translate("theme.switched", &[canonical.clone(), active]));
    0
}

#[cfg(test)]
mod tests {
    use super::{clamp_tail, command_marker, erase_seq, error_comment, fnv1a, is_open_diagram_fence, loop_prompt, parse_flow, reviewer_passed, session_lines, tail, CONFIRM_MARK, EDIT_MARK, RUN_MARK};
    use crate::security::Verdict;

    #[test]
    fn erase_seq_returns_to_the_top_of_the_painted_tail() {
        // Nothing painted → no cursor movement.
        assert_eq!(erase_seq(0), "");
        // One line: return to column 0, clear below (no cursor-up).
        assert_eq!(erase_seq(1), "\r\x1b[0J");
        // N lines: return to column 0, climb N-1 rows, clear below.
        assert_eq!(erase_seq(3), "\r\x1b[2A\x1b[0J");
    }

    #[test]
    fn clamp_tail_keeps_only_the_newest_rows_within_the_viewport() {
        // Fits within the viewport → unchanged, exact line count.
        let (t, n) = clamp_tail("a\nb\nc", 5);
        assert_eq!((t.as_str(), n), ("a\nb\nc", 3));
        // Taller than the viewport → keep the NEWEST `max_rows` lines only.
        let (t, n) = clamp_tail("a\nb\nc\nd\ne", 2);
        assert_eq!((t.as_str(), n), ("d\ne", 2));
        // A zero cap means "no clamp" (paint it all).
        let (_, n) = clamp_tail("a\nb\nc", 0);
        assert_eq!(n, 3);
    }

    #[test]
    fn open_diagram_fence_detected_only_while_unclosed() {
        assert!(is_open_diagram_fence("```mermaid\nflowchart TD"));
        assert!(!is_open_diagram_fence("```mermaid\nflowchart TD\n```"), "closed fence is complete");
        assert!(!is_open_diagram_fence("```rust\nlet x = 1;"), "not a diagram language");
        assert!(!is_open_diagram_fence("plain paragraph"));
    }

    #[test]
    fn command_marker_honours_mode_and_guard() {
        let allow = || Some(Verdict::Allow);
        // Allowed: manual reviews, auto runs.
        assert_eq!(command_marker(Some("ls -la"), allow(), "manual", ""), format!("{EDIT_MARK}ls -la"));
        assert_eq!(command_marker(Some("ls -la"), allow(), "auto", ""), format!("{RUN_MARK}ls -la"));
        // A confirm-tier command ALWAYS reviews, even in auto mode (safety).
        let confirm = Some(Verdict::Confirm { reason: "x".into() });
        assert_eq!(command_marker(Some("rm -rf build"), confirm, "auto", ""), format!("{CONFIRM_MARK}rm -rf build"));
        // A denied command is a comment, never run.
        let deny = Some(Verdict::Deny { reason: "fork bomb".into() });
        assert_eq!(command_marker(Some(":(){ :|:& };:"), deny, "auto", ""), "# blocked by guard: fork bomb");
        // No command → the model's refusal text becomes a comment.
        assert_eq!(command_marker(None, None, "manual", "I can't help with that"), "# I can't help with that");
        assert_eq!(command_marker(None, None, "manual", "# already a comment"), "# already a comment");
        assert_eq!(command_marker(None, None, "manual", "   "), "# the AI did not suggest a command");
    }

    #[test]
    fn error_comment_is_a_visible_comment() {
        let c = error_comment("AI isn't set up — add an [[ai.model]] in ~/.aiTerminal/config.toml");
        assert!(c.starts_with("# "), "shows as a shell comment, not silence");
        assert!(c.contains("set up"));
    }

    #[test]
    fn session_lines_reads_the_env_file_else_empty() {
        std::env::remove_var("TT_SESSION_LOG");
        assert!(session_lines().is_empty(), "no env → no session lines");
        let f = std::env::temp_dir().join(format!("tt-session-test-{}.txt", std::process::id()));
        std::fs::write(&f, "mkdir hamid\nls\nhamid  Desktop\n").unwrap();
        std::env::set_var("TT_SESSION_LOG", &f);
        let lines = session_lines();
        assert_eq!(lines, vec!["mkdir hamid".to_string(), "ls".to_string(), "hamid  Desktop".to_string()]);
        // The same assembly the CLI does: the session flows into capture_context, so the
        // model sees the recent terminal (`@ai go into it` can resolve "it").
        let ctx = crate::ai::capture_context(
            &crate::ai::TermContext { cwd: Some("/home/x"), shell: "zsh", recent_lines: &lines },
            40,
        );
        assert!(ctx.contains("mkdir hamid"), "context grounds on the recent session");
        std::env::remove_var("TT_SESSION_LOG");
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn flow_files_parse_and_validate() {
        let f = parse_flow(
            "review",
            "description = \"explore then review\"\nchain = true\n\
             [[step]]\nlabel = \"map\"\nagent = \"explorer\"\nprompt = \"Map: {{input}}\"\n\
             [[step]]\nagent = \"reviewer\"\nprompt = \"Review the findings\"\n",
        )
        .unwrap();
        assert_eq!(f.steps.len(), 2);
        assert_eq!(f.steps[0].label, "map");
        assert_eq!(f.steps[1].label, "reviewer", "label defaults to the agent name");
        assert!(f.chain);
        assert_eq!(f.description, "explore then review");
        // Placeholder substitution happens at run time (verify the raw prompt survives).
        assert!(f.steps[0].prompt.contains("{{input}}"));
        // Invalid flows are rejected with a clear error.
        assert!(parse_flow("x", "chain = true\n").is_err(), "no steps");
        assert!(parse_flow("x", "[[step]]\nagent = \"a\"\n").is_err(), "missing prompt");
    }

    #[test]
    fn example_flow_and_agent_parse() {
        // The shipped examples are the templates users copy — they must always
        // match the live schemas.
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
        let flow_text = std::fs::read_to_string(format!("{root}/ai/flow.toml")).unwrap();
        let flow = parse_flow("ship", &flow_text).expect("examples/ai/flow.toml parses");
        assert_eq!(flow.steps.len(), 4);
        assert!(flow.chain);
        // The example agent's frontmatter loads through the real agent loader.
        let dir = std::env::temp_dir().join(format!("tt-example-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(format!("{root}/ai/agent.md"), dir.join("docs-writer.md")).unwrap();
        let raw = crate::ai::defs::build_agent(&dir, &dir, &dir, "docs-writer").expect("examples/ai/agent.md loads");
        assert!(raw.tools.iter().any(|t| t == "fs.search"), "frontmatter tools parsed");
        assert!(raw.system.contains("technical writer"), "body becomes the system prompt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loop_prompt_carries_goal_check_and_feedback() {
        let p = loop_prompt("make tests pass", 3, 8, Some("cargo test"), "assertion failed: left == right");
        assert!(p.contains("iteration 3 of at most 8"));
        assert!(p.contains("exits 0: `cargo test`"));
        assert!(p.contains("assertion failed"), "verifier feedback is fed forward");
        // First iteration: no feedback section.
        let first = loop_prompt("goal", 1, 5, None, "");
        assert!(!first.contains("Verifier feedback"));
    }

    #[test]
    fn reviewer_verdict_parses_last_line() {
        assert!(reviewer_passed("looks good\nVERDICT: PASS"));
        assert!(reviewer_passed("the format is `VERDICT: CONTINUE`…\nVERDICT: PASS"), "last verdict wins");
        assert!(!reviewer_passed("VERDICT: CONTINUE\n1. fix x"));
        assert!(!reviewer_passed("no verdict at all"));
        assert!(reviewer_passed("verdict: pass"), "case-insensitive");
    }

    #[test]
    fn loop_stop_signature_detects_no_progress() {
        // Identical verifier observations hash identically (→ stalled); any change moves on.
        let a = fnv1a("exit=Some(1)\nassertion failed");
        let b = fnv1a("exit=Some(1)\nassertion failed");
        let c = fnv1a("exit=Some(1)\nDIFFERENT failure");
        assert_eq!(a, b);
        assert_ne!(a, c);
        // tail keeps the END of long output (failures print last) without splitting UTF-8.
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("héllo", 20), "héllo");
    }

    #[test]
    fn run_check_verifies_and_respects_the_guard() {
        // Pass/fail flow: exit 0 passes; a failure carries the output tail + a
        // stable signature for no-progress detection.
        let policy = crate::security::Policy::new();
        let long = std::time::Duration::from_secs(30);
        let ok = super::run_check("true", &policy, long).unwrap();
        assert!(ok.passed);
        let bad = super::run_check("echo boom; exit 3", &policy, long).unwrap();
        assert!(!bad.passed);
        assert!(bad.feedback.contains("boom") && bad.feedback.contains("exit=Some(3)"));
        let bad2 = super::run_check("echo boom; exit 3", &policy, long).unwrap();
        assert_eq!(bad.signature, bad2.signature, "same observation → same signature (stalled detection)");
        // The guard gates the check command itself: deny blocks, confirm refuses
        // (this path is non-interactive — no one to ask).
        let mut p = crate::security::Policy::new();
        p.add_deny("^rm\\b").unwrap();
        p.add_confirm("\\bsudo\\b").unwrap();
        assert!(super::run_check("rm -rf /tmp/x", &p, long).unwrap_err().contains("blocked"));
        assert!(super::run_check("sudo make check", &p, long).unwrap_err().contains("confirmation"));
    }

    #[test]
    fn run_check_kills_a_hung_command_at_the_deadline() {
        // A check that never finishes must not stall the loop forever: the
        // deadline kills it and surfaces a clear, actionable error.
        let policy = crate::security::Policy::new();
        let err = super::run_check("sleep 5", &policy, std::time::Duration::from_secs(1)).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
    }

    // ── the @loop engine, driven end-to-end by MOCKS ─────────────────────────
    // ScriptedTransport replays canned SSE responses (no model, no network); the
    // verifier is a scripted closure (no subprocess). This exercises the real
    // run_agent → verify → feedback → stop-rule pipeline.

    /// A runner that refuses every tool (the scripted maker never calls one).
    struct NoTools;
    impl crate::ai::ToolRunner for NoTools {
        fn run(&mut self, name: &str, _args: &str) -> Result<String, String> {
            Err(format!("no tool '{name}'"))
        }
    }

    /// Settings with a DUMMY test key (value "k" behind a test env var — never a
    /// real credential); the transport is scripted, so nothing ever egresses.
    fn keyed_settings() -> crate::ai::AiSettings {
        std::env::set_var("TT_TEST_LOOP_KEY", "k");
        let cat = crate::ai::builtin_default();
        let mut primary = cat.resolve("claude-opus-4-8");
        primary.api_key_env = "TT_TEST_LOOP_KEY".into();
        crate::ai::AiSettings { pool: crate::ai::ModelPool::single(primary) }
    }

    fn maker() -> crate::ai::AgentSpec {
        crate::ai::AgentSpec { system: "You fix things.".into(), tools: Vec::new(), max_steps: 3 }
    }

    /// A scripted client with one canned answer per expected iteration.
    fn scripted(answers: &[&str]) -> crate::ai::Client<crate::ai::ScriptedTransport> {
        let fixtures = answers.iter().map(|a| crate::ai::text_sse(a, 10, 4)).collect();
        crate::ai::Client::new(keyed_settings(), crate::ai::ScriptedTransport::new(fixtures))
    }

    fn verdict(passed: bool, feedback: &str) -> super::Verdict {
        super::Verdict { passed, feedback: feedback.into(), signature: fnv1a(feedback) }
    }

    #[test]
    fn loop_passes_when_the_verifier_passes_and_feeds_feedback_forward() {
        let client = scripted(&["attempt one", "attempt two"]);
        let mut iterations = 0;
        let outcome = super::drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "fix it", 5, None, Some("cargo test"), |answer| {
            iterations += 1;
            // The maker's scripted answers arrive in order — the loop really ran.
            match iterations {
                1 => {
                    assert_eq!(answer, "attempt one");
                    Ok(verdict(false, "2 tests failed"))
                }
                _ => {
                    assert_eq!(answer, "attempt two");
                    Ok(verdict(true, ""))
                }
            }
        }).outcome;
        assert_eq!(outcome, super::LoopOutcome::Done(2));
        assert_eq!(iterations, 2, "stopped exactly when the verifier passed");
    }

    #[test]
    fn loop_stalls_on_identical_verifier_observations() {
        // Same failure output twice in a row = no progress → stop, don't burn tokens.
        let client = scripted(&["a", "b", "c"]);
        let mut n = 0;
        let outcome = super::drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", 10, None, None, |_| {
            n += 1;
            Ok(verdict(false, "exit=1 same failure"))
        }).outcome;
        assert_eq!(outcome, super::LoopOutcome::Stalled);
        assert_eq!(n, 2, "detected on the second identical observation");
    }

    #[test]
    fn loop_exhausts_at_the_iteration_cap() {
        let client = scripted(&["a", "b", "c"]);
        let mut n = 0;
        let outcome = super::drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", 3, None, None, |_| {
            n += 1;
            Ok(verdict(false, &format!("different failure {n}"))) // always progressing
        }).outcome;
        assert_eq!(outcome, super::LoopOutcome::Exhausted);
        assert_eq!(n, 3, "ran exactly --max iterations");
    }

    #[test]
    fn loop_stops_at_the_token_budget() {
        // Each scripted turn reports 10 in + 4 out tokens; budget 1 → stop after
        // the first (still-failing) iteration.
        let client = scripted(&["a", "b"]);
        let outcome = super::drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", 10, Some(1), None, |_| {
            Ok(verdict(false, "still failing"))
        }).outcome;
        assert_eq!(outcome, super::LoopOutcome::Budget);
    }

    #[test]
    fn loop_surfaces_a_verifier_error() {
        // A guard-blocked check command aborts the loop as a setup error.
        let client = scripted(&["a"]);
        let outcome = super::drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", 5, None, None, |_| {
            Err("check command blocked by guard: rm".into())
        }).outcome;
        assert_eq!(outcome, super::LoopOutcome::Error("check command blocked by guard: rm".into()));
    }

    #[test]
    fn delegation_args_parse_bounded_and_validated() {
        // Single delegate.
        let one = super::parse_delegation(r#"{"agent": "tester", "prompt": "run the tests"}"#).unwrap();
        assert_eq!(one, vec![("tester".into(), "run the tests".into())]);
        // Agent defaults to explorer.
        let d = super::parse_delegation(r#"{"prompt": "map the code"}"#).unwrap();
        assert_eq!(d[0].0, "explorer");
        // Parallel fan-out keeps order and caps at 6.
        let many: Vec<String> = (0..9).map(|i| format!(r#"{{"agent": "a{i}", "prompt": "p{i}"}}"#)).collect();
        let arr = format!(r#"{{"tasks": [{}]}}"#, many.join(","));
        let tasks = super::parse_delegation(&arr).unwrap();
        assert_eq!(tasks.len(), 6, "fan-out bounded");
        assert_eq!(tasks[0], ("a0".into(), "p0".into()));
        // Empty / junk → clear errors, never a silent no-op.
        assert!(super::parse_delegation(r#"{"tasks": []}"#).is_err());
        assert!(super::parse_delegation(r#"{"agent": "x"}"#).is_err(), "missing prompt");
        assert!(super::parse_delegation("not json").is_err());
    }

    // ── streaming display + attachments (all mocked / temp files) ────────────

    #[test]
    fn harness_chrome_formats_are_stable() {
        // Token + byte humanization and the run footer — the glanceable stats line.
        assert_eq!(super::human_tokens(950), "950");
        assert_eq!(super::human_tokens(12_345), "12.3k");
        assert_eq!(super::human_bytes(80), "80B");
        assert_eq!(super::human_bytes(2048), "2.0KB");
        let f = super::run_footer_with("\u{2713}", std::time::Duration::from_millis(4200), 3, 12_345, 1_800, None, None);
        assert_eq!(f, "\u{2713} 4.2s \u{b7} 3 tools \u{b7} 12.3k in / 1800 out");
        let f1 = super::run_footer_with("\u{2713}", std::time::Duration::from_secs(61), 1, 100, 5, None, None);
        assert!(f1.contains("61s") && f1.contains("1 tool \u{b7}"), "{f1}");
        let f0 = super::run_footer_with("\u{2713}", std::time::Duration::from_millis(900), 0, 10, 2, None, None);
        assert!(!f0.contains("tool"), "no tool segment when none ran: {f0}");
    }

    #[test]
    fn tool_args_to_pairs_handles_json_and_bare() {
        // JSON object → keyed pairs.
        assert_eq!(super::tool_args_to_pairs("{\"path\":\"x\"}"), vec![("path".to_string(), "x".to_string())]);
        // Bare value (a weak model calling `fs.list .`) → positional arg 0.
        assert_eq!(super::tool_args_to_pairs("."), vec![("0".to_string(), ".".to_string())]);
        assert_eq!(super::tool_args_to_pairs("src/main.rs"), vec![("0".to_string(), "src/main.rs".to_string())]);
        // Empty / no-args → nothing.
        assert!(super::tool_args_to_pairs("").is_empty());
        assert!(super::tool_args_to_pairs("{}").is_empty());
    }

    #[test]
    fn human_cost_and_cost_segment_format() {
        assert_eq!(super::human_cost(0.0), "");
        assert_eq!(super::human_cost(-1.0), "");
        assert_eq!(super::human_cost(0.0002), "<$0.001");
        assert_eq!(super::human_cost(0.014), "$0.014");
        assert_eq!(super::human_cost(1.2), "$1.20");
        assert_eq!(super::human_cost(250.0), "$250");
        // No pricing → no segment.
        assert_eq!(super::cost_segment(None, None), "");
        assert_eq!(super::cost_segment(Some(0.0), Some(0.10)), "");
        // Priced, no budget → just the cost.
        assert_eq!(super::cost_segment(Some(0.014), None), " \u{b7} ~$0.014");
        // Priced + budget → cost + percent.
        let seg = super::cost_segment(Some(0.014), Some(0.10));
        assert!(seg.contains("~$0.014") && seg.contains("14% of $0.100"), "{seg}");
        // Over budget → ⚠ marker.
        let over = super::cost_segment(Some(0.20), Some(0.10));
        assert!(over.contains("\u{26a0}") && over.contains("200% of"), "{over}");
    }

    #[test]
    fn thinking_bursts_get_one_marker_each() {
        let mut obs = super::CliObserver::new(Vec::new());
        // First chunk of a burst carries the ∴ marker; continuations don't.
        let a = obs.thinking_chunk("planning");
        let b = obs.thinking_chunk(" the fix");
        assert!(a.contains("\u{2234}"), "{a:?}");
        assert!(!b.contains("\u{2234}"), "{b:?}");
        // A new turn (on_turn_start resets) opens a fresh burst.
        use crate::ai::AgentObserver;
        obs.on_turn_start();
        obs.wake(); // don't leave the spinner thread running in tests
        let c = obs.thinking_chunk("next turn");
        assert!(c.contains("\u{2234}"), "{c:?}");
    }

    #[test]
    fn observer_suppresses_xml_tool_call_from_display() {
        use crate::ai::AgentObserver;
        // A `<tool_call>` (an alternate model format) must never leak into the streamed
        // display — prose before it still shows, the machine protocol does not.
        let mut obs = super::CliObserver::new(Vec::new());
        obs.on_delta("Let me look.\n<tool_call>fs.list .</tool_call>\n");
        assert!(obs.streamed.contains("Let me look."), "prose kept: {:?}", obs.streamed);
        assert!(!obs.streamed.contains("tool_call"), "the raw tool call is suppressed: {:?}", obs.streamed);
        assert!(!obs.streamed.contains("fs.list"), "the call body is suppressed: {:?}", obs.streamed);
    }

    #[test]
    fn display_marker_recognizes_all_dialects() {
        // The display filter is sourced from the parser's TOOL_LINE_MARKERS, so every
        // tolerated line-anchored dialect is suppressed from the live stream.
        for m in ["@tool fs.x {}", "<tool_call>", "```tool", "[TOOL_CALLS] fs.x{}", "<|python_tag|>fs.x()"] {
            assert!(super::is_display_tool_marker(m), "{m:?} must be suppressed");
        }
        // Plain prose is not a marker.
        assert!(!super::is_display_tool_marker("Here is the answer."));
    }

    #[test]
    fn observer_suppresses_mistral_and_llama_tool_calls() {
        use crate::ai::AgentObserver;
        let mut obs = super::CliObserver::new(Vec::new());
        obs.on_delta("Checking.\n[TOOL_CALLS] fs.list {\"path\":\".\"}\n");
        assert!(obs.streamed.contains("Checking."), "prose kept: {:?}", obs.streamed);
        assert!(!obs.streamed.contains("TOOL_CALLS"), "mistral marker suppressed: {:?}", obs.streamed);
        assert!(!obs.streamed.contains("fs.list"), "call body suppressed: {:?}", obs.streamed);
    }

    #[test]
    fn spinner_is_inert_off_tty_and_stops_cleanly() {
        // Under `cargo test` stderr is piped → no thread, no frames; stop is a no-op.
        let mut sp = super::Spinner::start("waiting".into());
        assert!(sp.handle.is_none(), "no animation off-TTY (piped/background runs stay clean)");
        sp.stop();
    }

    #[test]
    fn cli_observer_streams_prose_and_suppresses_the_tool_protocol() {
        use crate::ai::AgentObserver;
        let mut obs = super::CliObserver::new(Vec::new());
        obs.on_turn_start();
        // Prose streams through (in split chunks, mid-line), the @tool line and the
        // JSON after it never print.
        obs.on_delta("Let me look");
        obs.on_delta(" at the file.\n@to");
        obs.on_delta("ol fs.read {\"path\"");
        obs.on_delta(": \"x\"}\nmore protocol\n");
        obs.on_commit("Let me look at the file.");
        // Next turn: the final answer streams fully.
        obs.on_turn_start();
        obs.on_delta("The file says hello.");
        let out = String::from_utf8(obs.streamed.clone().into_bytes()).unwrap();
        assert!(out.contains("Let me look at the file."), "prose streamed: {out:?}");
        assert!(out.contains("The file says hello."), "final answer streamed: {out:?}");
        assert!(!out.contains("@tool"), "protocol suppressed: {out:?}");
        assert!(!out.contains("more protocol"), "post-tool JSON suppressed: {out:?}");
    }

    #[test]
    fn cli_observer_holds_a_possible_marker_then_flushes_prose() {
        use crate::ai::AgentObserver;
        let mut obs = super::CliObserver::new(Vec::new());
        obs.on_turn_start();
        // "@toolbox" begins like the marker but isn't one — it must still print.
        obs.on_delta("@toolbox is a word\n");
        // A bare malformed marker never prints.
        obs.on_delta("@tool\n");
        assert!(obs.streamed.contains("@toolbox is a word"));
        assert!(!obs.streamed.contains("\n@tool\n"));
    }

    #[test]
    fn agent_run_streams_live_through_the_cli_observer() {
        // End-to-end with MOCKS: a scripted tool-calling turn then the final answer,
        // driven through run_agent + CliObserver. No model, no network, no tools run
        // (the runner refuses, and the loop feeds the refusal back).
        let client = scripted(&[
            "Checking the file.\n@tool fs.read {\"path\": \"x\"}",
            "Done: the file is fine.",
        ]);
        let spec = crate::ai::AgentSpec {
            system: "You check things.".into(),
            tools: vec![crate::ai::ToolSpec { name: "fs.read".into(), describe: "read".into() }],
            max_steps: 3,
        };
        let mut obs = super::CliObserver::new(Vec::new());
        let run = crate::ai::run_agent(&client, &spec, "check x", "", &mut NoTools, &mut obs);
        assert_eq!(run.answer, "Done: the file is fine.");
        assert!(obs.streamed.contains("Checking the file."), "turn prose streamed live");
        assert!(obs.streamed.contains("Done: the file is fine."), "answer streamed live");
        assert!(!obs.streamed.contains("@tool"), "protocol never reaches the display");
        assert_eq!(run.steps.len(), 1, "the tool call happened (and was refused by NoTools)");
    }

    #[test]
    fn attachments_collect_media_inline_text_and_skip_junk() {
        let dir = std::env::temp_dir().join(format!("tt-attach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("shot.png"), b"\x89PNG fakebytes").unwrap();
        std::fs::write(dir.join("doc.pdf"), b"%PDF-1.4 fake").unwrap();
        std::fs::write(dir.join("notes.txt"), "remember the milk").unwrap();
        std::fs::write(dir.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
        let p = |n: &str| dir.join(n).display().to_string();
        let prompt = format!("look at @{} and @{} and @{} and @{} and @/no/such/file plus user@host", p("shot.png"), p("doc.pdf"), p("notes.txt"), p("blob.bin"));
        let (clean, media, file_ctx) = super::collect_attachments(&prompt);
        // Media: the image + the pdf, base64-encoded with the right types.
        assert_eq!(media.len(), 2);
        assert_eq!(media[0].media_type, "image/png");
        assert_eq!(media[1].media_type, "application/pdf");
        assert_eq!(corelib::codec::base64_decode(&media[0].b64).unwrap(), b"\x89PNG fakebytes");
        // Text inlines fenced; binary is skipped; a missing path stays as typed.
        assert!(file_ctx.contains("remember the milk"));
        assert!(file_ctx.contains("notes.txt"));
        assert!(!file_ctx.contains("blob.bin"), "binary skipped from the context");
        assert!(clean.contains("@/no/such/file"), "non-file tokens untouched");
        assert!(clean.contains("user@host"), "mid-word @ untouched");
        assert!(!clean.contains(&format!("@{}", p("shot.png"))), "the @ is dropped from real paths");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn attachments_are_capped_in_count() {
        let dir = std::env::temp_dir().join(format!("tt-attach-count-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut prompt = String::from("summarize");
        for i in 0..20 {
            let f = dir.join(format!("f{i}.txt"));
            std::fs::write(&f, format!("file number {i}")).unwrap();
            prompt.push_str(&format!(" @{}", f.display()));
        }
        let (_, media, file_ctx) = super::collect_attachments(&prompt);
        assert!(media.is_empty());
        let count = file_ctx.matches("## Attached file:").count();
        assert_eq!(count, super::MAX_ATTACHMENTS, "attachment count bounded");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn attachments_truncate_large_text_files() {
        let dir = std::env::temp_dir().join(format!("tt-attach-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let big = "x".repeat(super::TEXT_ATTACH_MAX + 1000);
        std::fs::write(dir.join("big.log"), &big).unwrap();
        let (_, media, file_ctx) = super::collect_attachments(&format!("@{}", dir.join("big.log").display()));
        assert!(media.is_empty());
        assert!(file_ctx.contains("(truncated)"));
        assert!(file_ctx.len() < big.len(), "inlined text is capped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_switch_resolves_id_and_name_in_both_forms() {
        let (_h, _home) = crate::test_home::lock_home("cli-profile-switch");
        crate::config::Config::ensure_default();
        let p = crate::profile::create("Hamid", "🚀").unwrap();
        // By display name, case-insensitive — the exact confusion users hit.
        assert_eq!(super::profile_switch("Hamid"), 0);
        assert_eq!(crate::profile::active_id(), p.id);
        assert_eq!(super::profile_switch("default"), 0);
        // The `switch` verb goes through the SAME resolver (name works there too).
        assert_eq!(super::profile(&["switch".to_string(), "HAMID".to_string()]), 0);
        assert_eq!(crate::profile::active_id(), p.id);
        // Unknown → clear error pointing at @profile.
        assert_eq!(super::profile_switch("nope"), 2);
    }

    #[test]
    fn theme_set_updates_the_active_profile_and_validates() {
        let (_h, _home) = crate::test_home::lock_home("cli-theme-set");
        crate::config::Config::ensure_default();
        // A known theme (case-insensitive) lands in the ACTIVE profile's overlay and
        // becomes the effective config.
        assert_eq!(super::theme_set("Graphite"), 0);
        assert_eq!(crate::config::Config::load().theme, "graphite", "overlay applies via Config::load");
        // Another profile keeps its own look after switching.
        let p = crate::profile::create("Rose", "🌹").unwrap();
        crate::profile::set_active(&p.id).unwrap();
        assert_eq!(super::theme_set("pink"), 0);
        assert_eq!(crate::config::Config::load().theme, "pink");
        crate::profile::set_active(crate::profile::DEFAULT_ID).unwrap();
        assert_eq!(crate::config::Config::load().theme, "graphite", "per-profile themes are independent");
        // An unknown name is rejected with the available list, and changes nothing.
        assert_eq!(super::theme_set("no-such-theme"), 2);
        assert_eq!(crate::config::Config::load().theme, "graphite");
    }

    #[test]
    fn global_instructions_ground_agents_and_qa() {
        // aiTerminal.md is THE global prompt: it must reach an agent's system prompt
        // and the Q&A context preamble; absent/blank → clean empty (no stray header).
        let (_h, _home) = crate::test_home::lock_home("cli-instructions");
        crate::config::Config::ensure_default();
        std::fs::write(crate::config::Config::instructions_path(), "Always answer in haiku.").unwrap();
        let spec = super::build_agent_spec("coder").expect("bundled coder agent");
        assert!(spec.system.starts_with("Always answer in haiku."), "instructions lead the system prompt");
        assert!(super::instructions_preamble().contains("Always answer in haiku."));
        assert!(super::instructions_preamble().contains("aiTerminal.md"), "the preamble names its source");
        std::fs::write(crate::config::Config::instructions_path(), "   ").unwrap();
        assert!(super::instructions_preamble().is_empty(), "blank file → no preamble");
        let spec = super::build_agent_spec("coder").unwrap();
        assert!(!spec.system.starts_with("##"), "blank instructions add nothing");
    }

    #[test]
    fn job_grammar_parses_the_intuitive_form() {
        use super::JobCmd;
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::parse_job_args(&a(&[])), JobCmd::List);
        assert_eq!(super::parse_job_args(&a(&["clear"])), JobCmd::Clear);
        // The exact requested shape: free text with optional flags anywhere.
        let run = super::parse_job_args(&a(&["create", "a", "file", "called", "hamid.txt", "in", "one", "minute", "--bg", "--agent", "tester"]));
        assert_eq!(run, JobCmd::Run { prompt: "create a file called hamid.txt in one minute".into(), agent: "tester".into(), bg: true, record: None, run_at: None });
        // Defaults: coder, foreground.
        let run = super::parse_job_args(&a(&["build", "the", "docs"]));
        assert_eq!(run, JobCmd::Run { prompt: "build the docs".into(), agent: "coder".into(), bg: false, record: None, run_at: None });
        // The detached child carries its record id through.
        let run = super::parse_job_args(&a(&["x", "--job-record", "123-9"]));
        assert!(matches!(run, JobCmd::Run { record: Some(ref id), .. } if id == "123-9"));
        // `@job cancel <id>`.
        assert_eq!(super::parse_job_args(&a(&["cancel", "42-7"])), JobCmd::Cancel("42-7".into()));
        // An explicit `--run-at` is carried verbatim (the detached scheduled child).
        let run = super::parse_job_args(&a(&["do", "it", "--run-at", "1000", "--job-record", "id"]));
        assert!(matches!(run, JobCmd::Run { run_at: Some(1000), .. }));
    }

    #[test]
    fn parse_schedule_reads_natural_time() {
        // "in N unit" fires N later, with the phrase stripped from the prompt.
        let (at, cleaned) = super::parse_schedule("create a file named hamid in 2 minutes", 1_000);
        assert_eq!(at, Some(1_120));
        assert_eq!(cleaned, "create a file named hamid");
        // Fused unit + "after".
        assert_eq!(super::parse_schedule("build after 30s", 0).0, Some(30));
        assert_eq!(super::parse_schedule("build in 1 hour", 0).0, Some(3600));
        // A middle phrase is removed too.
        let (at, cleaned) = super::parse_schedule("ping the server in 5 minutes please", 0);
        assert_eq!(at, Some(300));
        assert_eq!(cleaned, "ping the server please");
        // No schedule → run now, prompt untouched.
        assert_eq!(super::parse_schedule("just do it now", 0), (None, "just do it now".to_string()));
        // "in" as ordinary prose (not a delay) does not misfire.
        assert_eq!(super::parse_schedule("look in the src folder", 0).0, None);
        // "at HH:MM" resolves to a future unix time.
        assert!(super::parse_schedule("email me at 17:30", 0).0.is_some());
    }

    #[test]
    fn diagram_output_falls_back_to_a_box_off_our_terminal() {
        // Not our GUI terminal (TERM_PROGRAM unset) → a clean fallback box, no jargon, no OSC.
        std::env::remove_var("TERM_PROGRAM");
        let out = super::diagram_output("flowchart TD\n A --> B");
        assert!(out.contains("diagram") && out.contains('╭'), "fallback box: {out:?}");
        assert!(!out.contains("\x1b]1338"), "no native OSC off our terminal");
        assert!(out.contains("A --> B"));
    }

    #[test]
    fn flow_free_text_falls_back_to_the_default_pipeline() {
        // A known flow name runs by name; unknown first words become input to the
        // default `implement` pipeline (the resolution `ai_flow_cmd` applies).
        let (_h, _home) = crate::test_home::lock_home("cli-flow-default");
        crate::config::Config::ensure_default();
        assert!(super::load_flow("review").is_ok(), "bundled flow resolves by name");
        assert!(super::load_flow("add").is_err(), "free text is not a flow name");
        assert!(super::load_flow(super::DEFAULT_FLOW).is_ok(), "the default pipeline ships");
    }

    #[test]
    fn job_ids_are_contained() {
        assert!(super::jobs::dir("1234-99").is_some());
        assert!(super::jobs::dir("../etc").is_none(), "traversal rejected");
        assert!(super::jobs::dir("").is_none());
    }

    // ── production-harness guarantees: exit codes, jobs, discovery ───────────

    #[test]
    fn outcomes_map_to_honest_exit_codes() {
        use crate::ai::RunOutcome;
        assert_eq!(super::outcome_exit(&RunOutcome::Completed), 0);
        assert_eq!(super::outcome_exit(&RunOutcome::Error("boom".into())), 1);
        assert_eq!(super::outcome_exit(&RunOutcome::StepLimit), 1);
        assert_eq!(super::outcome_exit(&RunOutcome::ToolStall), 1);
        assert_eq!(super::outcome_exit(&RunOutcome::Cancelled), 130, "the interrupt convention");
    }

    #[test]
    fn loop_never_verifies_an_errored_iteration() {
        // An empty script → the maker run errors. The verifier must NEVER see that
        // non-answer as if it were work — it panics if called.
        let client = crate::ai::Client::new(keyed_settings(), crate::ai::ScriptedTransport::new(vec![]));
        let outcome = super::drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", 5, None, None, |_| {
            panic!("the verifier must not run on an errored iteration")
        }).outcome;
        assert!(matches!(outcome, super::LoopOutcome::Error(_)), "{outcome:?}");
    }

    #[test]
    fn loop_stops_cleanly_on_cancellation() {
        // A pre-cancelled client (what the Ctrl+C watcher produces) → the loop
        // reports Cancelled (exit 130), and the verifier never runs.
        let cancel = crate::ai::CancelToken::new();
        cancel.cancel();
        let client = crate::ai::Client::new(keyed_settings(), crate::ai::ScriptedTransport::new(vec![])).with_cancel(cancel);
        let outcome = super::drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", 5, None, None, |_| {
            panic!("the verifier must not run on a cancelled iteration")
        }).outcome;
        assert_eq!(outcome, super::LoopOutcome::Cancelled);
    }

    #[test]
    fn job_records_heal_dead_pids_and_clear_prunes_them() {
        let (_h, _home) = crate::test_home::lock_home("cli-job-liveness");
        crate::config::Config::ensure_default();
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // A record whose pid is long gone (pid 999999 is far above macOS's default
        // pid ceiling) must heal from `running` to `died` on the next list.
        let dead_id = "1000-1";
        std::fs::create_dir_all(super::jobs::dir(dead_id).unwrap()).unwrap();
        super::jobs::record_start(dead_id, &a(&["ghost", "work"]), 999_999);
        // A record pointing at THIS process stays running.
        let live_id = "2000-2";
        std::fs::create_dir_all(super::jobs::dir(live_id).unwrap()).unwrap();
        super::jobs::record_start(live_id, &a(&["real", "work"]), std::process::id());
        let jobs = super::jobs::list();
        let by_id = |id: &str| jobs.iter().find(|j| j.id == id).unwrap();
        assert_eq!(by_id(dead_id).status, "died", "zombie healed on list");
        assert_eq!(by_id(live_id).status, "running", "a live pid is untouched");
        // The healing is persisted (a second list re-reads the healed record)…
        assert_eq!(super::jobs::list().iter().find(|j| j.id == dead_id).unwrap().status, "died");
        // …and `clear` prunes the healed zombie while keeping the live job.
        assert_eq!(super::jobs::clear_finished(), 1);
        let left = super::jobs::list();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, live_id);
    }

    #[test]
    fn job_finish_stamps_the_outcome_status() {
        let (_h, _home) = crate::test_home::lock_home("cli-job-finish");
        crate::config::Config::ensure_default();
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for (id, code, want) in [("3000-1", 0, "done"), ("3000-2", 1, "failed"), ("3000-3", 130, "cancelled")] {
            std::fs::create_dir_all(super::jobs::dir(id).unwrap()).unwrap();
            super::jobs::record_start(id, &a(&["w"]), std::process::id());
            super::jobs::finish(id, code);
            assert_eq!(super::jobs::list().iter().find(|j| j.id == id).unwrap().status, want, "exit {code}");
        }
        let jobs = super::jobs::list();
        let by_id = |id: &str| jobs.iter().find(|j| j.id == id).unwrap();
        assert_eq!(by_id("3000-3").status_glyph(), "\u{23f9}");
        assert!(by_id("3000-1").finished.is_some(), "finish stamps the end time");
    }

    #[test]
    fn job_timing_reads_at_a_glance() {
        assert_eq!(super::jobs::human_age(45), "45s");
        assert_eq!(super::jobs::human_age(95), "1m");
        assert_eq!(super::jobs::human_age(4000), "1h");
        let j = super::jobs::Job {
            id: "x".into(),
            status: "done".into(),
            cmd: String::new(),
            started: 1000,
            finished: Some(1045),
            run_at: None,
            log: std::path::PathBuf::new(),
        };
        assert_eq!(j.timing(1180), "3m ago \u{b7} 45s");
        // A scheduled job shows when it fires instead of run duration.
        let s = super::jobs::Job {
            id: "y".into(),
            status: "scheduled".into(),
            cmd: String::new(),
            started: 1000,
            finished: None,
            run_at: Some(1300),
            log: std::path::PathBuf::new(),
        };
        assert_eq!(s.timing(1180), "fires in 2m");
    }

    #[test]
    fn folder_session_flows_into_context_and_runner() {
        // End-to-end wiring (no network): a folder's session digest + folder memory feed
        // the context preamble, and `build_runner` scopes the agent's memory tools to the
        // folder store — so a returning run "remembers" the project.
        let (_h, _home) = crate::test_home::lock_home("cli-folder-session");
        let cfg = crate::config::Config::load();
        let ws = crate::config::Config::dir().join("proj-x");
        std::fs::create_dir_all(&ws).unwrap();
        let session = crate::ai::Session::at(&ws, &crate::config::Config::sessions_dir());

        // 1) A prior run's digest shows up in the session preamble.
        session.record_run("@ai", "list rust files", "fd -e rs");
        let pre = super::session_preamble(Some(&session));
        assert!(pre.contains("list rust files") && pre.contains("fd -e rs"), "digest injected: {pre:?}");
        assert!(super::session_preamble(None).is_empty(), "no session → no preamble");

        // 2) A folder-scoped memory is recalled by the folder-aware memory preamble.
        crate::ai::MemoryService::for_folder(session.memory_dir())
            .add("decision", vec![], "this project ships via scripts/release.sh").unwrap();
        let mem = super::memory_preamble(&cfg, "how do we release?", Some(session.memory_dir().as_path()));
        assert!(mem.contains("release.sh"), "folder memory recalled: {mem:?}");

        // 3) build_runner scopes the agent's memory.* tools to THIS folder's session store.
        let settings = cfg.ai_settings();
        let policy = std::sync::Arc::new(crate::security::Policy::new());
        let runner = super::build_runner(&cfg, &settings, Some(ws.clone()), policy, false);
        assert_eq!(runner.ctx.memory_dir.as_deref(), Some(session.memory_dir().as_path()), "runner memory is folder-scoped");
    }
}
