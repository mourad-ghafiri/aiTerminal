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

use std::path::{Path, PathBuf};

/// `aiTerminal ai …` — the terminal-native AI entry point.
///
/// - `ai "<prompt>"` — stream a Markdown answer to stdout.
/// - `ai --command "<request>"` — natural language → one guarded shell command.
/// - `ai --agent <name> "<task>"` — run an agent's full tool loop (`@<agent>`).
/// - `ai --bg …` — run any of the above detached, tracked as a job.
/// - `ai job [<task> [--agent <name>] [--bg]]` — run a TRACKED task; bare = list.
/// - `ai flow <name> "<input>"` — run a declarative **graph** of agent/command/approval
///   nodes; bare = list, and `check`/`graph`/`runs`/`show`/`log`/`resume` operate on it.
pub fn ai(args: &[String]) -> i32 {
    // Word subcommands first. Singular, like every command; both take intuitive
    // free-text forms with optional flags anywhere (`@job build the docs --bg`).
    match args.first().map(String::as_str) {
        Some("agent") | Some("agents") => return ai_agent_cmd(args),
        Some("job") => return ai_job_cmd(args),
        Some("flow") => return ai_flow_cmd(args),
        Some("loop") => return ai_loop_cmd(args),
        _ => {}
    }

    let mut as_command = false;
    let mut agent: Option<String> = None;
    let mut bg = false;
    let mut job_record: Option<String> = None;
    let mut parts: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--command" | "-c" => as_command = true,
            "--fast" => {} // reserved; Q&A already uses the pool, command uses the fast model
            "--agent" => agent = it.next().cloned(),
            "--bg" => bg = true,
            "--job-record" => job_record = it.next().cloned(),
            other => parts.push(other),
        }
    }
    let prompt = parts.join(" ");
    if prompt.trim().is_empty() {
        eprintln!("usage: aiTerminal ai [--command | --agent <name>] [--bg] \"<prompt>\"");
        eprintln!("       aiTerminal ai flow <name> \"<input>\"   |  ai flow check|graph|runs|show|log|resume");
        eprintln!("       aiTerminal ai loop \"<goal>\" [--check \"<cmd>\"] [--max N] [--timeout 30m]");
        eprintln!("       aiTerminal ai agent [<name>]                 # the installed agents");
        eprintln!("       aiTerminal ai job [clear]");
        return 2;
    }

    // `--bg`: relaunch this exact invocation detached, stdout+stderr → the job log,
    // and return immediately with the job id (monitor with `ai jobs` / `tail -f`).
    if bg {
        return spawn_background(args);
    }

    let code = ai_run(as_command, agent, &prompt);
    // A detached child carries `--job-record <id>`: stamp the job's outcome.
    if let Some(id) = job_record {
        crate::jobs::finish(&id, code);
    }
    code
}

/// The foreground AI run (Q&A / command / agent / flow), streaming to stdout.
fn ai_run(as_command: bool, agent: Option<String>, prompt: &str) -> i32 {
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
    // Trimmed to the model's window before it is sent. The preamble is assembled from
    // sources that grow on their own — a session digest, a terminal scrollback — and
    // on a small-window model the grounding could otherwise crowd out the question it
    // was meant to ground.
    let ctx = fit_context(
        &crate::ai::ContextBudget::for_model(&settings.primary(), cfg.ai_context_window, cfg.ai_compact_at),
        prompt,
        &[
            ("instructions", instructions_preamble()),
            ("attachments", file_ctx.clone()),
            ("memory", memory_preamble(&cfg, prompt, folder_mem.as_deref())),
            ("session", session_preamble(session.as_ref())),
            ("terminal", term.clone()),
        ],
    );
    // Apply the user's AI-scope redaction rules (config + plugins) before egress.
    let registry = crate::plugin::load_registry(&cfg);
    let policy = crate::security::build_policy(&cfg, &registry);
    let ctx = policy.redact(&ctx, crate::security::RedactScope::Ai);
    let policy = std::sync::Arc::new(policy);
    let workspace_root = cwd_path.clone();

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

pub(crate) fn accent() -> String {
    theme_color("TT_ACCENT_RGB", "\x1b[36m")
}
pub(crate) fn muted() -> String {
    theme_color("TT_MUTED_RGB", "\x1b[2m")
}
pub(crate) fn reset() -> &'static str {
    // Gated exactly like the colours it closes: with `accent`/`muted` empty off a
    // terminal, an ungated reset is a stray escape in every redirected line.
    if err_is_tty() {
        "\x1b[0m"
    } else {
        ""
    }
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
    // The alert hues come from the theme's own semantic tokens, so a callout is the same
    // green/amber/red the rest of the UI uses.
    corelib::md::Style {
        enabled: true,
        heading: accent,
        accent,
        code: d.code,
        muted,
        link: accent,
        success: rgb("TT_SUCCESS_RGB", d.success),
        warn: rgb("TT_WARN_RGB", d.warn),
        error: rgb("TT_ERROR_RGB", d.error),
    }
}

/// Wrap width for rendered Markdown — the split's REAL width (via `TIOCGWINSZ`, since the shell
/// doesn't export `$COLUMNS` to us), minus a small right margin. Falls back to `$COLUMNS`, then
/// 80. No low cap: wide splits are used fully (a generous 400 ceiling just guards absurd sizes).
fn md_width() -> usize {
    term_cols().saturating_sub(2).clamp(24, 400)
}

/// The terminal's width in columns — `TIOCGWINSZ`, then `$COLUMNS`, then 80.
///
/// ONE definition, because anything that repaints in place has to agree with the
/// terminal about where a line ends: a row wider than the window wraps to two VISUAL
/// rows, and a cursor-up count measured in logical lines then climbs too few of them.
pub(crate) fn term_cols() -> usize {
    platform::os::terminal_size()
        .map(|(c, _)| c as usize)
        .or_else(|| std::env::var("COLUMNS").ok().and_then(|c| c.trim().parse::<usize>().ok()))
        .unwrap_or(80)
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
pub(crate) fn erase_seq(painted: usize) -> String {
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
            // A streamed answer has no document directory; only absolute paths and
            // (when allowed) remote images can resolve.
            corelib::md::Chunk::Image { src, fallback, .. } => {
                let _ = w.write_all(image_output(&src, &fallback, Path::new(".")).as_bytes());
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
pub(crate) fn is_native_terminal() -> bool {
    std::env::var("TERM_PROGRAM").ok().as_deref() == Some(corelib::brand::NAME)
}

/// Grid rows a diagram needs, from its pure layout height (nominal 8×16 cell). Clamped to a
/// sane band. Shared by the inline `OSC 1338` emitter and the `@md edit` preview layout so a
/// diagram reserves the same height everywhere.
pub(crate) fn diagram_rows(source: &str) -> usize {
    corelib::mermaid::parse(source)
        .map(|d| {
            let l = corelib::mermaid::layout(&d, &|s: &str| (corelib::unicode::str_width(s) as u32 * 8, 16));
            l.height.div_ceil(18).clamp(3, 120) as usize
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
    diagram_text(source)
}

/// A diagram for terminals that can't draw pixels: the real picture in Unicode box art,
/// or — only when it can't be read or won't fit the width — the source in a box. The user
/// never has to look at diagram syntax if we can avoid it.
fn diagram_text(source: &str) -> String {
    let width = md_width();
    match corelib::mermaid::art(source, width) {
        Some(rows) if !rows.is_empty() => {
            let mut out = String::new();
            for r in rows {
                out.push_str(&r);
                out.push('\n');
            }
            out
        }
        _ => diagram_fallback_box(source),
    }
}

/// An image for a terminal that can draw pixels: an `OSC 1339` placement over reserved
/// rows. Anywhere else — or for an image we can't get hold of — the caller's `fallback`
/// (the ordinary `▣ alt` placeholder) is what shows.
///
/// `base` is the document's own directory, so a README's `img/logo.png` resolves the way
/// the document meant it.
pub(crate) fn image_output(src: &str, fallback: &str, base: &Path) -> String {
    if !is_native_terminal() {
        return fallback.to_string();
    }
    match image_placement(src, base) {
        Some((path, rows)) => format!("\x1b]1339;{rows};{}\x07", corelib::codec::base64_encode(path.as_bytes())),
        None => fallback.to_string(),
    }
}

/// Resolve an image source to a local file and the grid rows it should occupy — what a
/// host reserves before asking the app to draw it.
pub(crate) fn image_placement(src: &str, base: &Path) -> Option<(String, usize)> {
    let cfg = crate::config::Config::load();
    let path = resolve_image(src, base, &cfg)?;
    let bytes = std::fs::read(&path).ok()?;
    let img = platform::os::image_decoder().decode(&bytes)?;
    if img.width == 0 || img.height == 0 {
        return None;
    }
    // A grid cell is about twice as tall as it is wide, so a square image needs about
    // half as many rows as it does columns.
    let cols = md_width() as f32;
    let rows = (img.height as f32 / img.width as f32 * cols * 0.5).round() as usize;
    Some((path.to_string_lossy().into_owned(), rows.clamp(2, cfg.md_image_max_rows)))
}

/// A local path for `src`: a file beside the document, or — only when `[md]
/// remote_images` says so — a cached download.
fn resolve_image(src: &str, base: &Path, cfg: &crate::config::Config) -> Option<PathBuf> {
    let src = src.trim();
    if src.is_empty() || src.starts_with("data:") {
        return None;
    }
    if src.starts_with("http://") || src.starts_with("https://") {
        return cfg.md_remote_images.then(|| cached_download(src)).flatten();
    }
    let path = match src.strip_prefix("file://") {
        Some(p) => PathBuf::from(p),
        None => {
            let p = PathBuf::from(src);
            if p.is_absolute() {
                p
            } else {
                base.join(p)
            }
        }
    };
    path.is_file().then_some(path)
}

/// Fetch a remote image once and keep it under `~/.<brand>/cache/images/`.
fn cached_download(url: &str) -> Option<PathBuf> {
    let dir = crate::config::Config::dir().join("cache").join("images");
    let name = format!("{:016x}{}", url_hash(url), image_ext(url));
    let path = dir.join(name);
    if path.is_file() {
        return Some(path);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let bytes = platform::transport::fetch(url).ok()?;
    // Something that isn't an image is not worth keeping, and not worth drawing.
    if bytes.is_empty() || platform::os::image_decoder().decode(&bytes).is_none() {
        return None;
    }
    std::fs::write(&path, &bytes).ok()?;
    Some(path)
}

/// A stable file name for a URL (FNV-1a — a cache key, not a security decision).
fn url_hash(url: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The extension a URL implies, so the cached file is recognizable on disk.
fn image_ext(url: &str) -> String {
    let tail = url.rsplit('/').next().unwrap_or("");
    let ext = tail.rsplit_once('.').map(|(_, e)| e.split(['?', '#']).next().unwrap_or("")).unwrap_or("");
    if (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        format!(".{}", ext.to_ascii_lowercase())
    } else {
        String::new()
    }
}

/// A diagram's drawn rows for a preview `width` columns wide, without styling — the art
/// when it can be drawn, else the boxed source. Shared by the `@md` pager and editor so a
/// diagram occupies exactly the rows it paints.
pub(crate) fn diagram_lines(source: &str, width: usize) -> Vec<String> {
    if let Some(rows) = corelib::mermaid::art(source, width) {
        return rows;
    }
    let w = source.lines().map(corelib::unicode::str_width).max().unwrap_or(0).clamp(7, width.saturating_sub(2).max(7));
    let mut out = vec![format!("╭─ diagram {}╮", "─".repeat(w.saturating_sub(9)))];
    for line in source.lines() {
        let pad = w.saturating_sub(corelib::unicode::str_width(line));
        out.push(format!("│ {line}{} │", " ".repeat(pad)));
    }
    out.push(format!("╰{}╯", "─".repeat(w + 2)));
    out
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
    // Relative image paths in a document are relative to the document itself.
    let doc_dir = Path::new(path).parent().unwrap_or(Path::new(".")).to_path_buf();
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
    // The whole file is in hand, so every reference and footnote resolves wherever it is
    // defined — including below the text that uses it.
    sr.seed(corelib::md::scan_defs(&text));
    let mut out = std::io::stdout().lock();
    let mut emit = |chunks: Vec<corelib::md::Chunk>| {
        for c in chunks {
            match c {
                corelib::md::Chunk::Text(t) => {
                    let _ = out.write_all(t.as_bytes());
                }
                corelib::md::Chunk::Diagram(src) => {
                    let d = if tty { diagram_output(&src) } else { diagram_text(&src) };
                    let _ = out.write_all(d.as_bytes());
                }
                corelib::md::Chunk::Image { src, fallback, .. } => {
                    let d = if tty { image_output(&src, &fallback, &doc_dir) } else { fallback };
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
    /// The same context settings the parent run uses — a delegate that ignored the
    /// window would fail on exactly the models the delegation was meant to help.
    context: (u32, f32),
}

/// How many levels of `task.run` delegation are allowed (the orchestrating agent
/// may fan out sub-agents; a sub-agent may not delegate further).
const MAX_DELEGATION_DEPTH: u8 = 1;

/// The two context settings a run carries: the `[ai] context_window` override (`0` =
/// use the serving model's own) and the `[ai] compact_at` threshold.
///
/// Deliberately NOT a finished budget. The pool picks which model serves a run when
/// the run starts, so a budget resolved here would belong to whichever model happened
/// to be representative — not to the one answering. `run_agent` resolves it against
/// the model it pins.
///
/// ONE place reads these, so `@agent`, `@flow`, `@loop`, `@job` and sub-agents can
/// never drift into budgeting differently from one another.
fn context_settings(cfg: &crate::config::Config) -> (u32, f32) {
    (cfg.ai_context_window, cfg.ai_compact_at)
}

/// A private directory for one run's offloaded tool output.
///
/// The counter is not decoration. `record::new_id()` is `<unix-secs>-<pid>`, which is
/// the SAME string for four `@flow` nodes that start in the same second inside one
/// process — and offloaded files are named by turn index, so two nodes would each
/// write `003-fs-read.txt` into the same directory and one would read back the
/// other's output. The suffix is what makes the isolation real.
fn run_scratch() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    crate::config::Config::offload_dir().join(format!("{}-{n}", crate::record::new_id()))
}

/// Assemble the grounding preamble, dropping whole blocks from the END of `blocks`
/// until it fits alongside `prompt`.
///
/// `blocks` is ordered **most valuable first**, and that order is the design: the
/// user's standing instructions and the files they explicitly attached are the last
/// things to go, while the terminal scrollback — which grows on its own and which
/// nobody asked for — is the first. A block goes whole rather than being cut in half,
/// because half a session digest is a misleading digest.
///
/// Dropping is announced on stderr. Grounding that vanishes silently is how "why
/// didn't it know that?" becomes unanswerable.
fn fit_context(budget: &crate::ai::ContextBudget, prompt: &str, blocks: &[(&str, String)]) -> String {
    use crate::ai::TokenEstimator;
    let est = crate::ai::HeuristicEstimator;
    let mut kept: Vec<&(&str, String)> = blocks.iter().filter(|(_, text)| !text.trim().is_empty()).collect();
    let mut dropped: Vec<&str> = Vec::new();
    let room = budget.compact_threshold().saturating_sub(est.estimate(prompt));
    while !kept.is_empty() {
        let used: usize = kept.iter().map(|(_, t)| est.estimate(t)).sum();
        if used <= room {
            break;
        }
        // Safe: the loop only runs while `kept` is non-empty.
        dropped.push(kept.pop().expect("non-empty").0);
    }
    if !dropped.is_empty() {
        dropped.reverse();
        eprintln!("{}  \u{2139} context trimmed to fit the model's window \u{2014} dropped: {}{}", muted(), dropped.join(", "), reset());
    }
    kept.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("")
}

/// A pure agent tool runner: routes a model tool call to `caps::run` (no live host),
/// intercepts `task.run` (sub-agent delegation), and redacts every result before it
/// re-enters the loop.
struct CliToolRunner {
    ctx: crate::caps::CapCtx,
    mcp: Option<crate::ai::McpHub>,
    sub: SubAgentCtx,
    depth: u8,
    /// Where a tool trace goes. `None` is stderr, which is right for a single agent
    /// run and wrong inside a graph: four nodes calling tools at once produce four
    /// interleaved streams with nothing to say which is which.
    trace: Option<std::sync::Arc<dyn crate::flow::board::ToolTrace>>,
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
        let line = match &result {
            Ok(v) => format!("\u{2699} {name} {preview} \u{b7} {ms}ms \u{b7} {}", human_bytes(json_text(v).len())),
            Err(e) => {
                let brief: String = e.chars().take(80).collect();
                format!("\u{2699} {name} {preview} \u{b7} {ms}ms \u{b7} \u{2717} {brief}")
            }
        };
        match &self.trace {
            Some(sink) => sink.tool(&line),
            None => eprintln!("{dim}  {line}{r}"),
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
        // Through the SAME sink as every other tool trace. A bare `eprintln!` here
        // wrote straight into the middle of a `@flow` board's painted region — the
        // board then erased the wrong rows, because its line count no longer matched
        // what was on screen.
        let line = format!(
            "\u{2514} task.run \u{2192} {} sub-agent(s): {}",
            tasks.len(),
            tasks.iter().map(|(a, _)| format!("@{a}")).collect::<Vec<_>>().join(" ")
        );
        match &self.trace {
            Some(sink) => sink.tool(&line),
            None => eprintln!("  {line}"),
        }

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
        context_window: sub.context.0,
        compact_at: sub.context.1,
        scratch: run_scratch(),
    };
    let client = crate::ai::Client::new(sub.settings.clone(), crate::ai::CurlTransport::default());
    let mut runner = CliToolRunner { ctx, mcp: None, sub: sub.clone(), depth, trace: None };
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
    let session = workspace.as_ref().map(|w| crate::ai::Session::at(w, &crate::config::Config::sessions_dir()));
    let memory_dir = session.as_ref().map(|s| s.memory_dir());
    // …and the same session backs `todo.*` / `data.*` / `queue.*` / `store.*`. Those four
    // families key everything off `app_data`, and this is the only place in the product
    // that builds a `CapCtx` — so while it was `None`, all nineteen of their methods
    // answered "only available to installed apps" everywhere, including the four `todo.*`
    // tools `coder` declares and its prompt tells it to use.
    let app_data = session.as_ref().map(|s| s.data_dir());
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
        ctx: crate::caps::CapCtx { policy, app_data, remote_enabled: cfg.ai_network, origin: "terminal://ai/".into(), sandbox: workspace, memory_dir },
        mcp,
        trace: None,
        sub: SubAgentCtx {
            settings: settings.clone(),
            agents_dir: crate::config::Config::agents_dir(),
            skills_dir: crate::config::Config::skills_dir(),
            prompts_dir: crate::config::Config::prompts_dir(),
            context: context_settings(cfg),
        },
        depth: 0,
    }
}

// ===== @agent — what you have, and what each one does ========================
//
// An agent is a Markdown file with frontmatter: the tools it may call, the skills spliced into
// its prompt, and a step cap. Until now you could only find out an agent existed by misspelling
// one and reading the error. Two things fix that: this listing, and `defs::validate` — because
// a roster you can read is only useful if the entries in it are real.

/// `ai agent [<name>]` — the installed agents, or one in full.
fn ai_agent_cmd(args: &[String]) -> i32 {
    crate::config::Config::ensure_default();
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    let wanted = args.get(1).filter(|a| !a.starts_with('-'));
    let agents = crate::ai::defs::load_agents(&crate::config::Config::agents_dir());
    // The validator already knows when a file is wrong; it just never reached the person
    // who edited it. A malformed agent used to be listed as if it were fine — blank
    // description, default toolset, runnable — with no sign that its frontmatter had not
    // parsed.
    let problems = crate::ai::defs::validate(
        &crate::config::Config::agents_dir(),
        &crate::config::Config::skills_dir(),
        &crate::config::Config::prompts_dir(),
        &crate::caps::is_method,
    );
    let faults = |name: &str| -> Vec<String> {
        let head = format!("agent '{name}': ");
        problems.iter().filter_map(|p| p.strip_prefix(&head).map(str::to_string)).collect()
    };
    let (dim, r) = (muted(), reset());
    if agents.is_empty() {
        println!("{}", crate::i18n::translate("agent.none", &[crate::config::Config::agents_dir().display().to_string()]));
        return 0;
    }
    let Some(name) = wanted else {
        println!("{}", crate::i18n::translate("agent.header", &[agents.len().to_string()]));
        for a in &agents {
            let bad = faults(&a.name);
            if bad.is_empty() {
                println!(
                    "  {:<12} {dim}{:>2} tools \u{b7} {:>2} steps{r}  {}",
                    a.name,
                    a.tools.len(),
                    a.max_steps,
                    clip_tail(&a.description, 58)
                );
            } else {
                println!("  {:<12} {}\u{26a0} {}{r}", a.name, accent(), clip_tail(&bad.join(" \u{b7} "), 62));
            }
        }
        println!("\n{}", crate::i18n::translate("agent.run_hint", &[]));
        return 0;
    };
    let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
    let Some(a) = agents.iter().find(|a| a.name == *name) else {
        eprintln!("aiTerminal: no agent '{name}'{}", crate::flow::verify::nearest(name, &names));
        eprintln!("  installed: {}", names.join(", "));
        return 2;
    };
    let bad = faults(&a.name);
    if !bad.is_empty() {
        println!("{}\u{26a0} this file has problems{r}", accent());
        for b in &bad {
            println!("  \u{2022} {b}");
        }
        println!();
    }
    println!("{}@{}{r} {dim}\u{b7} {} step(s){r}", accent(), a.name, a.max_steps);
    println!("  {}", a.description);
    if !a.skills.is_empty() {
        println!("\n  {dim}skills{r}  {}", a.skills.join(", "));
    }
    if !a.prompts.is_empty() {
        println!("  {dim}prompts{r} {}", a.prompts.join(", "));
    }
    if a.tools.is_empty() {
        println!("\n  {dim}no tools \u{2014} it answers from the conversation alone{r}");
    } else {
        println!("\n  {dim}tools{r}");
        for t in &a.tools {
            // A tool that is not in the registry is shown as such rather than
            // quietly given the catalog's generic description.
            let known = crate::caps::is_method(t);
            let mark = if known { " " } else { "\u{2717}" };
            let what = if known { crate::caps::describe(t) } else { "not a real capability" };
            println!("   {mark} {t:<20} {dim}{}{r}", clip_tail(what, 60));
        }
    }
    // The last section of an agent's prompt is its output contract, and a flow node
    // chains on exactly that — so it is worth being able to read without opening the file.
    if let Some(at) = a.system.rfind("## What you return") {
        println!("\n  {dim}returns{r}");
        for line in a.system[at..].lines().skip(1).filter(|l| !l.trim().is_empty()).take(8) {
            println!("   {dim}{}{r}", clip_tail(line.trim(), 76));
        }
    }
    println!("\n  {dim}{}{r}", crate::config::Config::agents_dir().join(format!("{}.md", a.name)).display());
    0
}

/// Make this process behave like a command rather than a server.
///
/// One thing so far: the Rust runtime ignores `SIGPIPE` before `main`, so a write to a
/// pipe whose reader has gone returns `EPIPE` and `println!` panics. That turns
/// `aiTerminal ai flow | head -2` into an intermittent backtrace where `ls | head` is
/// silent — and the docs promise that piping stays clean.
pub fn install_command_defaults() {
    platform::os::restore_sigpipe();
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
fn build_agent_spec(name: &str, context: (u32, f32)) -> Option<crate::ai::AgentSpec> {
    let raw = crate::ai::defs::build_agent(&crate::config::Config::agents_dir(), &crate::config::Config::skills_dir(), &crate::config::Config::prompts_dir(), name)?;
    let tools = raw.tools.into_iter().map(|n| crate::ai::ToolSpec { describe: crate::caps::describe(&n).to_string(), name: n }).collect();
    let global = instructions();
    let system = if global.is_empty() { raw.system } else { format!("{global}\n\n{}", raw.system) };
    Some(crate::ai::AgentSpec { system, tools, max_steps: raw.max_steps, context_window: context.0, compact_at: context.1, scratch: run_scratch() })
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
    let Some(mut agent) = build_agent_spec(name, context_settings(cfg)) else {
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

// ===== @flow — a workflow declared as a graph ================================
//
// Graph engineering in one sentence: stop writing a chain of prompts and start declaring the
// graph the work actually is. Six pieces make that real here:
//
//   1. A DAG, NOT A LINE.  `needs` is a dependency, so nodes that need nothing from each other
//      run AT THE SAME TIME. Three reviews cost one round of wall clock instead of three.
//   2. ROUTING ON THE EDGE.  `when` is data this tool parses, not an instruction a model
//      interprets — because an agent asked to decide what happens next decides differently
//      each time, and nothing about the run can be audited afterwards.
//   3. A DETERMINISTIC BACKBONE.  A `run` node is a command through the same guard everything
//      else uses, and costs no tokens. The model is spent only where judgement is needed.
//   4. BOUNDED CYCLES.  `goto` points one edge backwards with a `max`, so "test, fix, test
//      again" is a flow rather than something you sit and supervise.
//   5. PROVED BEFORE IT SPENDS.  Everything checkable without a model is checked first
//      (`flow::verify`): a dangling edge, a reference to a node that does not run first, an
//      agent that is not installed, a command the guard refuses. Exit 2, zero tokens.
//   6. STATE THAT SURVIVES.  Every node's result is written to `ai/flow-runs/<id>/` the moment
//      it lands, so `@flow show` reads the shape, `@flow log` reads a node, and `@flow resume`
//      runs only what did not complete — the fix for the old chain's all-or-nothing failure.

/// Load flow `name` from `~/.aiTerminal/ai/flows/<name>.toml`.
fn load_flow(name: &str) -> Result<crate::flow::Flow, String> {
    if !crate::flow::tmpl::id_ok(name) {
        return Err(format!("'{name}' is not a flow name"));
    }
    let path = crate::config::Config::flows_dir().join(format!("{name}.toml"));
    match std::fs::read_to_string(&path) {
        Ok(text) => crate::flow::parse(name, &text),
        Err(_) => {
            let installed = flow_names();
            let refs: Vec<&str> = installed.iter().map(String::as_str).collect();
            // A typo must never quietly become a different flow: this used to fall
            // through to the `implement` pipeline, so a misspelling ran a
            // code-editing graph over the repository.
            Err(format!(
                "no flow '{name}'{}{}",
                crate::flow::verify::nearest(name, &refs),
                if installed.is_empty() {
                    format!(" — add one to {}", crate::config::Config::flows_dir().display())
                } else {
                    String::new()
                }
            ))
        }
    }
}

/// Every installed flow's name, sorted.
fn flow_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(crate::config::Config::flows_dir())
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
                .filter_map(|p| p.file_stem()?.to_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// What the verifier needs from outside itself: the installed agents and the guard.
struct FlowWorld {
    policy: std::sync::Arc<crate::security::Policy>,
    agents: Vec<crate::ai::defs::Agent>,
}

impl FlowWorld {
    fn build() -> FlowWorld {
        let cfg = crate::config::Config::load();
        let registry = crate::plugin::load_registry(&cfg);
        FlowWorld {
            policy: std::sync::Arc::new(crate::security::build_policy(&cfg, &registry)),
            agents: crate::ai::defs::load_agents(&crate::config::Config::agents_dir()),
        }
    }
}

impl crate::flow::verify::World for FlowWorld {
    fn agent_tools(&self, name: &str) -> Option<Vec<String>> {
        self.agents.iter().find(|a| a.name == name).map(|a| a.tools.clone())
    }
    fn guard(&self, command: &str) -> crate::flow::verify::Guard {
        use crate::flow::verify::Guard;
        match self.policy.check_command(command) {
            crate::security::Verdict::Allow => Guard::Allow,
            crate::security::Verdict::Confirm { reason } => Guard::Confirm(reason),
            crate::security::Verdict::Deny { reason } => Guard::Deny(reason),
        }
    }
    fn agent_names(&self) -> Vec<String> {
        self.agents.iter().map(|a| a.name.clone()).collect()
    }
}

/// Load and verify in one step — the gate every path that spends money goes through.
fn checked_flow(name: &str) -> Result<(crate::flow::Flow, crate::flow::verify::Report), String> {
    let flow = load_flow(name)?;
    let report = crate::flow::verify::verify(&flow, &FlowWorld::build());
    Ok((flow, report))
}

/// Print a verification report. Errors first: they are why nothing ran.
fn print_report(name: &str, report: &crate::flow::verify::Report, nodes: usize) {
    let (dim, r) = (muted(), reset());
    for e in &report.errors {
        eprintln!("  {}\u{2717}{r} {e}", accent());
    }
    for w in &report.warnings {
        eprintln!("  {dim}\u{26a0}  {w}{r}");
    }
    if report.ok() && report.warnings.is_empty() {
        println!("  {dim}\u{2713} {name} \u{b7} {nodes} node(s) \u{b7} worst case {} agent run(s){r}", report.worst_case_runs);
    }
}

// ─────────────────────────────── the surface ───────────────────────────────

/// What an `ai flow …` invocation asks for.
#[derive(Debug, PartialEq)]
pub(crate) enum FlowCmd {
    /// Bare `@flow` — the installed flows.
    List,
    Help,
    /// Verify one flow, or every installed flow when no name is given.
    Check(Option<String>),
    /// Draw a flow's graph.
    Graph(String),
    /// Past runs.
    Runs,
    Clear,
    Show(String),
    Log { id: String, node: Option<String>, follow: bool },
    Resume(String),
    Run(Box<FlowSpec>),
}

/// A flow to run.
#[derive(Debug, PartialEq, Default)]
pub(crate) struct FlowSpec {
    pub(crate) name: String,
    /// The text typed after the flow name — `{{input}}`.
    pub(crate) input: String,
    /// Bounds left unset fall back to the file's `[bounds]`, then `[flow]` config.
    pub(crate) timeout: Option<u64>,
    pub(crate) budget: Option<u64>,
    pub(crate) concurrency: Option<usize>,
    pub(crate) bg: bool,
    pub(crate) dry_run: bool,
    /// Set on the detached child so it can stamp its job record on exit.
    pub(crate) job_record: Option<String>,
}

/// Read `ai flow …` argv.
///
/// Subcommands win over flow names, and a flow file named after one is refused by
/// the verifier — so `@flow show` is never ambiguous, and the ambiguity is reported
/// where it can be explained rather than resolved by a coin toss.
pub(crate) fn parse_flow_args(args: &[String]) -> Result<FlowCmd, String> {
    let word = |i: usize| args.get(i).filter(|a| !a.starts_with('-')).cloned();
    let id_or_last = |from: usize| args[from..].iter().find(|a| !a.starts_with('-')).cloned().unwrap_or_else(|| "last".into());
    match args.first().map(String::as_str) {
        None => return Ok(FlowCmd::List),
        Some("list") if args.len() == 1 => return Ok(FlowCmd::List),
        Some("help") | Some("--help") | Some("-h") => return Ok(FlowCmd::Help),
        Some("check") => return Ok(FlowCmd::Check(word(1))),
        Some("graph") | Some("draw") => {
            return match word(1) {
                Some(name) => Ok(FlowCmd::Graph(name)),
                None => Err("graph needs a flow name — try `@flow graph implement`".into()),
            }
        }
        Some("runs") if args.len() == 1 => return Ok(FlowCmd::Runs),
        Some("clear") if args.len() == 1 => return Ok(FlowCmd::Clear),
        Some("show") => return Ok(FlowCmd::Show(id_or_last(1))),
        Some("resume") | Some("continue") => return Ok(FlowCmd::Resume(id_or_last(1))),
        Some("log") | Some("logs") => {
            let follow = args.iter().any(|a| a == "-f" || a == "--follow");
            let plain: Vec<String> = args[1..].iter().filter(|a| !a.starts_with('-')).cloned().collect();
            return Ok(FlowCmd::Log {
                id: plain.first().cloned().unwrap_or_else(|| "last".into()),
                node: plain.get(1).cloned(),
                follow,
            });
        }
        _ => {}
    }
    let mut spec = FlowSpec::default();
    let mut words: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bg" => spec.bg = true,
            "--dry-run" | "--plan" => spec.dry_run = true,
            "--job-record" => spec.job_record = Some(flag_value(&mut it, "--job-record")?),
            "--timeout" => {
                let v = flag_value(&mut it, "--timeout")?;
                let secs = corelib::datetime::duration(&v)
                    .ok_or_else(|| format!("--timeout needs a duration like 30m or 90s, got {v:?}"))?;
                spec.timeout = Some(secs.max(30));
            }
            "--budget" => {
                let v = flag_value(&mut it, "--budget")?;
                spec.budget = Some(v.parse().map_err(|_| format!("--budget needs a token count, got {v:?}"))?);
            }
            "--concurrency" => {
                let v = flag_value(&mut it, "--concurrency")?;
                let n: usize = v.parse().map_err(|_| format!("--concurrency needs a whole number, got {v:?}"))?;
                spec.concurrency = Some(n.clamp(1, 16));
            }
            w => words.push(w.to_string()),
        }
    }
    let Some((name, rest)) = words.split_first() else {
        return Err("a flow needs a name or a goal — `@flow` on its own lists them".into());
    };
    // `@flow "make the export emit JSON"` — one argument with a space in it is a goal,
    // because no flow can be called that. The model routes it, and prints its choice.
    if rest.is_empty() && crate::flow::pick::is_goal(name) {
        spec.input = name.clone();
        return Ok(FlowCmd::Run(Box::new(spec)));
    }
    spec.name = name.clone();
    // One argument is the input as typed, so `@flow ship "raise --max to 10"` keeps
    // its flag-looking words; several loose words are a sentence to rejoin.
    spec.input = match rest {
        [only] => only.clone(),
        many => many.join(" "),
    };
    Ok(FlowCmd::Run(Box::new(spec)))
}

fn flow_usage() -> String {
    [
        "usage: @flow <name> \"<input>\"       run a flow",
        "       @flow … --bg | --dry-run     detach it | verify and draw it, spend nothing",
        "       @flow … --timeout 30m --budget TOKENS --concurrency N",
        "       @flow                        list the installed flows",
        "       @flow check [<name>]         verify a flow (or all of them) — no model needed",
        "       @flow graph <name>           draw the graph",
        "       @flow runs                   recent runs",
        "       @flow show <id>              one run: the graph, with what each node cost",
        "       @flow log <id> [<node>] [-f] a node's full output",
        "       @flow resume <id>            run only what did not complete",
        "       @flow clear                  prune finished runs",
    ]
    .join("\n")
}

/// `ai flow …` — the whole surface. `args` includes the leading "flow" word.
fn ai_flow_cmd(args: &[String]) -> i32 {
    let cmd = match parse_flow_args(&args[1..]) {
        Ok(cmd) => cmd,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            eprintln!("{}", flow_usage());
            return 2;
        }
    };
    crate::config::Config::ensure_default();
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    match cmd {
        FlowCmd::List => flow_list(),
        FlowCmd::Help => {
            println!("{}", flow_usage());
            0
        }
        FlowCmd::Check(name) => flow_check(name.as_deref()),
        FlowCmd::Graph(name) => flow_graph(&name),
        FlowCmd::Runs => flow_runs(),
        FlowCmd::Clear => {
            println!("{}", crate::i18n::translate("flow.cleared", &[crate::flowruns::clear_finished().to_string()]));
            0
        }
        FlowCmd::Show(id) => flow_show(&id),
        FlowCmd::Log { id, node, follow } => flow_log(&id, node.as_deref(), follow),
        FlowCmd::Resume(id) => match crate::flowruns::resolve(&id) {
            Ok(id) => run_flow_cli(FlowSpec::default(), Some(id)),
            Err(e) => {
                eprintln!("aiTerminal: {e}");
                2
            }
        },
        FlowCmd::Run(spec) => {
            if spec.bg {
                return spawn_background(args);
            }
            let record = spec.job_record.clone();
            let code = run_flow_cli(*spec, None);
            if let Some(id) = record {
                crate::jobs::finish(&id, code);
            }
            code
        }
    }
}

/// `@flow` — the installed flows.
fn flow_list() -> i32 {
    let names = flow_names();
    if names.is_empty() {
        println!("{}", crate::i18n::translate("flow.none", &[crate::config::Config::flows_dir().display().to_string()]));
        return 0;
    }
    let (dim, r) = (muted(), reset());
    println!("{}", crate::i18n::translate("flow.header", &[names.len().to_string()]));
    for name in &names {
        match load_flow(name) {
            Ok(flow) => {
                println!("  {name:<12} {:<28} {}", clip_tail(&shape_of(&flow), 28), flow.description);
            }
            // A file that will not parse is shown rather than hidden: a flow that
            // silently vanished from the list is the harder thing to debug.
            Err(e) => println!("  {name:<16} {dim}\u{26a0} {}{r}", opening_line(&e)),
        }
    }
    println!("\n{}", crate::i18n::translate("flow.run_hint", &[]));
    0
}

/// "5 nodes · 3 in parallel · loops" — a flow's shape at a glance.
fn shape_of(flow: &crate::flow::Flow) -> String {
    let n = flow.nodes.len();
    let mut notes = Vec::new();
    // The widest set of nodes waiting on exactly the same thing — the parallelism
    // someone actually gets. Branch alternatives are excluded: the two arms of one
    // verdict wait on the same node but only ever one of them runs.
    let widest = (0..flow.nodes.len())
        .map(|i| {
            (0..flow.nodes.len())
                .filter(|&j| flow.nodes[j].needs == flow.nodes[i].needs && !crate::flow::verify::exclusive(flow, i, j))
                .count()
        })
        .max()
        .unwrap_or(0);
    if widest > 1 {
        notes.push(format!("{widest} parallel"));
    }
    if flow.nodes.iter().any(|x| x.goto.is_some()) {
        notes.push("loops".into());
    }
    if flow.nodes.iter().any(|x| x.is_map()) {
        notes.push("fans out".into());
    }
    if flow.nodes.iter().any(|x| matches!(x.kind, crate::flow::Kind::Approve { .. })) {
        notes.push("asks you".into());
    }
    if notes.is_empty() {
        format!("{n} nodes")
    } else {
        format!("{n} nodes \u{b7} {}", notes.join(" \u{b7} "))
    }
}

/// `@flow check [<name>]` — everything provable without a model.
fn flow_check(name: Option<&str>) -> i32 {
    let names: Vec<String> = match name {
        Some(n) => vec![n.to_string()],
        None => flow_names(),
    };
    if names.is_empty() {
        println!("{}", crate::i18n::translate("flow.none", &[crate::config::Config::flows_dir().display().to_string()]));
        return 0;
    }
    let mut worst = 0;
    for n in &names {
        if names.len() > 1 {
            println!("{}", n);
        }
        match checked_flow(n) {
            Ok((flow, report)) => {
                print_report(n, &report, flow.nodes.len());
                worst = worst.max(report.exit());
            }
            Err(e) => {
                eprintln!("aiTerminal: {e}");
                worst = 2;
            }
        }
    }
    worst
}

/// `@flow graph <name>` — the shape, drawn.
fn flow_graph(name: &str) -> i32 {
    let (flow, report) = match checked_flow(name) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            return 2;
        }
    };
    // A broken graph is still drawn: seeing the shape is usually how you understand
    // what the error is telling you.
    for line in crate::flow::render::draw(&flow, None, md_width()) {
        println!("{line}");
    }
    if !report.ok() {
        println!();
        print_report(name, &report, flow.nodes.len());
        return 2;
    }
    // Warnings do not fail a drawing: you asked for the picture and got it.
    0
}

/// `@flow runs` — the recent runs, newest first.
fn flow_runs() -> i32 {
    let runs = crate::flowruns::list();
    if runs.is_empty() {
        println!("{}", crate::i18n::translate("flow.no_runs", &[]));
        return 0;
    }
    let now = crate::flowruns::now();
    let (dim, r) = (muted(), reset());
    println!("{}", crate::i18n::translate("flow.runs_header", &[runs.len().to_string()]));
    for run in runs {
        let age = crate::flowruns::human_age(now.saturating_sub(run.finished.unwrap_or(run.started)));
        let input = clip_tail(&run.input, 40);
        println!("  {} {} {:<9} {} {input}  {dim}({age} ago){r}", run.status_glyph(), run.id, run.status, run.flow);
        let done = run.nodes.iter().filter(|n| n.state == crate::flowruns::NodeState::Done).count();
        let (tin, tout) = run.tokens();
        println!("      {dim}{done}/{} node(s) done \u{b7} {} tool call(s) \u{b7} {tin} in / {tout} out{r}", run.nodes.len(), run.tools());
    }
    println!("\n{}", crate::i18n::translate("flow.runs_hint", &[]));
    0
}

/// `@flow show <id>` — one run: the same picture, with what actually happened on it.
fn flow_show(id: &str) -> i32 {
    let Some(run) = resolved_run(id) else { return 2 };
    let (dim, r) = (muted(), reset());
    println!("{} {} {} \u{b7} flow '{}'", run.status_glyph(), run.id, run.status, run.flow);
    if !run.input.is_empty() {
        println!("  {dim}input{r}     {}", run.input);
    }
    if !run.cwd.is_empty() {
        println!("  {dim}folder{r}    {}", run.cwd);
    }
    let budget = run.budget.map(|b| format!(" \u{b7} {b} tokens")).unwrap_or_default();
    println!(
        "  {dim}bounds{r}    {} \u{b7} {} at a time{budget}",
        crate::flowruns::human_age(run.timeout),
        run.concurrency
    );
    let (tin, tout) = run.tokens();
    println!("  {dim}spent{r}     {} tool call(s) \u{b7} {tin} in / {tout} out", run.tools());
    println!();
    // The definition may have been edited since; then the record is the only truth.
    match load_flow(&run.flow) {
        Ok(flow) if flow.nodes.iter().all(|n| run.node(&n.id).is_some()) => {
            for line in crate::flow::render::draw(&flow, Some(&run), md_width()) {
                println!("{line}");
            }
        }
        _ => {
            for node in &run.nodes {
                println!("  {} {:<16} {}", node.state.glyph(), node.id, clip_tail(&node.output, 50));
            }
        }
    }
    if !run.unfinished().is_empty() {
        let left: Vec<&str> = run.unfinished().iter().map(|n| n.id.as_str()).collect();
        println!("\n  {dim}left to do{r} {}", left.join(", "));
        println!("  {}", crate::i18n::translate("flow.resume_hint", &[run.id.clone()]));
    }
    0
}

/// `@flow log <id> [<node>] [-f]` — what a node actually said.
fn flow_log(id: &str, node: Option<&str>, follow: bool) -> i32 {
    let Some(run) = resolved_run(id) else { return 2 };
    // With no node named, the one whose answer is the flow's answer — which is what
    // someone reaching for `@flow log last` almost always wants.
    let wanted = match node {
        Some(n) => n.to_string(),
        None => match load_flow(&run.flow).ok().and_then(|f| f.answer_node().map(|i| f.nodes[i].id.clone())) {
            Some(id) => id,
            None => run.nodes.last().map(|n| n.id.clone()).unwrap_or_default(),
        },
    };
    let Some(path) = run.node_log(&wanted) else {
        for line in no_output_message(&run, &wanted) {
            eprintln!("{line}");
        }
        return 2;
    };
    let id = run.id.clone();
    let alive = || matches!(crate::flowruns::read(&id), Some(r) if r.is_live());
    tail_log(&path, follow, &alive)
}

/// The stderr lines for a node the run has no log for — the two cases kept apart.
///
/// A node that EXISTS but has no log did not fail to be found: it did not run, and the
/// record already says why. Suggesting the name back ("no output for node 'b' — did you
/// mean 'b'?") is the tool answering a question nobody asked, so `nearest` is reserved
/// for a name that genuinely is not in the graph.
fn no_output_message(run: &crate::flowruns::Run, wanted: &str) -> Vec<String> {
    match run.node(wanted) {
        Some(node) => {
            let why = match node.state {
                crate::flowruns::NodeState::Skipped => "its condition was false",
                crate::flowruns::NodeState::Blocked => "something it needed failed",
                crate::flowruns::NodeState::Waiting => "it is waiting for an answer",
                _ => "it has not run yet",
            };
            vec![
                format!("aiTerminal: node '{wanted}' produced no output \u{2014} {why}"),
                format!("  {}", crate::i18n::translate("flow.resume_hint", &[run.id.clone()])),
            ]
        }
        None => {
            let names: Vec<&str> = run.nodes.iter().map(|n| n.id.as_str()).collect();
            vec![
                format!(
                    "aiTerminal: run {} has no node '{wanted}'{}",
                    run.id,
                    crate::flow::verify::nearest(wanted, &names)
                ),
                format!("  nodes: {}", names.join(", ")),
            ]
        }
    }
}

fn resolved_run(id: &str) -> Option<crate::flowruns::Run> {
    match crate::flowruns::resolve(id) {
        Ok(id) => crate::flowruns::read(&id),
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            None
        }
    }
}

/// The first line of a multi-line message — for one-line list rows.
fn opening_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().trim().to_string()
}

/// Whether somebody is actually at the keyboard — what decides if an approval can
/// be asked or has to park the run.
fn stdin_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

// ─────────────────────────────── the executor ───────────────────────────────

/// What a finished node produced. The scheduler carries these between nodes, so
/// everything a later node or a condition can see is in here.
#[derive(Clone, Debug, Default)]
struct NodeOut {
    ok: bool,
    output: String,
    /// A command node's exit status.
    exit: Option<i64>,
    /// An approve node's answer.
    approved: bool,
    /// Reached an approval with nobody to ask: the run parks rather than deadlocks.
    parked: bool,
    input_tokens: u64,
    output_tokens: u64,
    tools: usize,
    ms: u64,
    attempts: u32,
}

/// Self-contained work for one node — resolved on the scheduler's thread, executed
/// on a worker's. Nothing in here refers to another node, which is why two nodes
/// running at once cannot race for state.
enum NodeWork {
    /// An agent run, or one run per item when the node fans out.
    Agent { agent: String, prompts: Vec<String> },
    Run { commands: Vec<String> },
    Approve { show: String, question: String },
    /// A resume: this node already ran, and its answer is read back from disk.
    Replay(Box<NodeOut>),
}

/// Runs one flow's graph.
struct FlowDriver<'a> {
    flow: &'a crate::flow::Flow,
    cfg: &'a crate::config::Config,
    settings: crate::ai::AiSettings,
    policy: std::sync::Arc<crate::security::Policy>,
    workspace: Option<std::path::PathBuf>,
    input: String,
    run_id: String,
    /// Outputs replayed from a previous run's record, by node id.
    replay: Vec<(String, String)>,
    /// The whole-run cancel: Ctrl+C and the wall clock both trip it.
    cancel: crate::ai::CancelToken,
    budget: Option<u64>,
    spent: std::sync::atomic::AtomicU64,
    concurrency: usize,
    /// Somebody is at the terminal, so an approval can actually be answered.
    interactive: bool,
    /// The record rows, updated as each node lands.
    rows: std::sync::Mutex<Vec<crate::flowruns::NodeRun>>,
    /// The live display — one line per node, in graph order.
    board: std::sync::Arc<crate::flow::board::Board>,
}

impl FlowDriver<'_> {
    /// What a condition can ask about node `name`.
    fn facts(&self, name: &str, done: &[Option<NodeOut>], status: &[platform::orchestrator::Status]) -> Option<crate::flow::expr::Facts> {
        let i = self.flow.index(name)?;
        if status[i] == platform::orchestrator::Status::Skipped {
            return Some(crate::flow::expr::Facts { skipped: true, ..Default::default() });
        }
        let out = done[i].as_ref()?;
        Some(crate::flow::expr::Facts {
            ran: true,
            passed: out.ok,
            skipped: false,
            approved: out.approved,
            exit: out.exit,
            output: out.output.clone(),
        })
    }

    /// Fill in one `{{…}}`. Every reference was proved upstream by the verifier, so
    /// a missing one here means the branch legitimately did not run.
    fn resolve(&self, r: &crate::flow::tmpl::Ref, done: &[Option<NodeOut>], item: Option<&str>) -> String {
        use crate::flow::tmpl::{Field, Ref};
        match r {
            Ref::Input => self.input.clone(),
            Ref::FlowName => self.flow.name.clone(),
            Ref::Var(_) => item.unwrap_or_default().to_string(),
            Ref::Node { id, field } => {
                let Some(i) = self.flow.index(id) else { return String::new() };
                let Some(out) = done[i].as_ref() else { return String::new() };
                match field {
                    Field::Output => out.output.clone(),
                    Field::Exit => out.exit.map(|e| e.to_string()).unwrap_or_default(),
                }
            }
        }
    }

    /// The items a `map` node fans out over: a JSON array if the upstream produced
    /// one, else its non-empty lines. Capped, because the list comes from a node
    /// nobody bounded.
    fn items(&self, text: &str) -> Vec<String> {
        // The array first, even when it arrives wrapped in a sentence or a fence — an
        // agent asked for a list usually introduces it, and that introduction would
        // otherwise become an item with an agent run of its own.
        let parsed = crate::ai::plan::extract_array(text)
            .and_then(|json| corelib::wire::Json::parse(&json).ok())
            .and_then(|j| j.as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect::<Vec<_>>()))
            .filter(|v: &Vec<String>| !v.is_empty());
        let mut items = parsed.unwrap_or_else(|| {
            text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
        });
        items.truncate(self.cfg.flow_max_map);
        items
    }

    /// Record one node's result the moment it lands, so a run that dies mid-way is
    /// still worth resuming.
    fn record(&self, node: &crate::flow::Node, asked: &str, out: &NodeOut, state: crate::flowruns::NodeState) {
        crate::flowruns::write_node(&self.run_id, &node.id, asked, &out.output);
        let Ok(mut rows) = self.rows.lock() else { return };
        if let Some(row) = rows.iter_mut().find(|r| r.id == node.id) {
            *row = crate::flowruns::NodeRun {
                id: node.id.clone(),
                state,
                exit: out.exit,
                approved: out.approved,
                input_tokens: out.input_tokens,
                output_tokens: out.output_tokens,
                tools: out.tools,
                ms: out.ms,
                attempts: out.attempts,
                output: out.output.clone(),
            };
        }
        if let Some(mut run) = crate::flowruns::read(&self.run_id) {
            run.nodes = rows.clone();
            crate::flowruns::write(&self.run_id, &run);
        }
    }

    /// One agent run, on its own client and its own tool runner — the shape
    /// `task.run` already uses for parallel sub-agents.
    fn one_agent(&self, name: &str, prompt: &str, node: &crate::flow::Node) -> NodeOut {
        let Some(mut spec) = build_agent_spec(name, context_settings(self.cfg)) else {
            return NodeOut { ok: false, output: format!("no agent '{name}'"), ..NodeOut::default() };
        };
        if let Some(max) = node.max_steps {
            spec.max_steps = max;
        }
        let cancel = crate::ai::CancelToken::new();
        let _watch = self.node_watchdog(cancel.clone(), node);
        let client = crate::ai::Client::new(self.settings.clone(), crate::ai::CurlTransport::default()).with_cancel(cancel);
        let mut runner = build_runner(self.cfg, &self.settings, self.workspace.clone(), self.policy.clone(), true);
        runner.trace = Some(std::sync::Arc::new(crate::flow::board::NodeTrace {
            board: self.board.clone(),
            node: node.id.clone(),
        }));
        if let Some(hub) = &runner.mcp {
            for (n, describe) in hub.tools() {
                spec.tools.push(crate::ai::ToolSpec { name: n, describe });
            }
        }
        let started = std::time::Instant::now();
        let run = crate::ai::run_agent(&client, &spec, prompt, "", &mut runner, &mut crate::ai::NoopObserver);
        self.spent.fetch_add((run.input_tokens + run.output_tokens) as u64, std::sync::atomic::Ordering::Relaxed);
        NodeOut {
            ok: run.outcome == crate::ai::RunOutcome::Completed,
            output: run.answer,
            input_tokens: run.input_tokens as u64,
            output_tokens: run.output_tokens as u64,
            tools: run.steps.len(),
            ms: started.elapsed().as_millis() as u64,
            attempts: 1,
            ..NodeOut::default()
        }
    }

    /// A token that trips when this node runs out of its own time, or when the whole
    /// run is cancelled — so Ctrl+C reaches into a node that is mid-request.
    fn node_watchdog(&self, token: crate::ai::CancelToken, node: &crate::flow::Node) -> SigintWatch {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let secs = node.timeout.unwrap_or(self.cfg.flow_node_timeout);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let whole_run = self.cancel.clone();
        let flag = done.clone();
        std::thread::spawn(move || {
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                if whole_run.is_cancelled() || std::time::Instant::now() >= deadline {
                    token.cancel();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        });
        SigintWatch { done }
    }

    /// One command node, through the same guard every other command goes through.
    fn one_command(&self, command: &str, node: &crate::flow::Node) -> NodeOut {
        let secs = node.timeout.unwrap_or(self.cfg.flow_node_timeout);
        let started = std::time::Instant::now();
        match run_check(command, &self.policy, std::time::Duration::from_secs(secs)) {
            Ok(v) => NodeOut {
                ok: v.passed,
                output: v.raw,
                exit: v.code.map(i64::from),
                ms: started.elapsed().as_millis() as u64,
                attempts: 1,
                ..NodeOut::default()
            },
            Err(e) => NodeOut {
                ok: false,
                output: e,
                exit: None,
                ms: started.elapsed().as_millis() as u64,
                attempts: 1,
                ..NodeOut::default()
            },
        }
    }

    /// Ask the person. Off a terminal there is nobody to ask, so the run *parks*
    /// rather than guessing or hanging — `@flow resume` picks it up with somebody
    /// there. Gating an action behind a question nobody hears is how an unattended
    /// pipeline deadlocks.
    fn ask(&self, show: &str, question: &str) -> NodeOut {
        if !show.trim().is_empty() {
            println!("{show}");
        }
        if !self.interactive {
            return NodeOut {
                ok: false,
                parked: true,
                output: format!("{question}\n(nobody at the terminal — resume this run to answer)"),
                ..NodeOut::default()
            };
        }
        use std::io::Write;
        eprint!("{}{question} [y/N] {}", accent(), reset());
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let yes = std::io::stdin().read_line(&mut line).is_ok() && matches!(line.trim(), "y" | "Y" | "yes" | "Yes");
        NodeOut {
            ok: yes,
            approved: yes,
            output: if yes { "approved".into() } else { "declined".into() },
            attempts: 1,
            ..NodeOut::default()
        }
    }
}

use platform::orchestrator::Driver as _;

impl platform::orchestrator::Driver for FlowDriver<'_> {
    type Work = NodeWork;
    type Out = NodeOut;

    fn prepare(&self, i: usize, done: &[Option<NodeOut>], status: &[platform::orchestrator::Status]) -> platform::orchestrator::Plan<NodeWork> {
        use platform::orchestrator::Plan;
        let node = &self.flow.nodes[i];
        // A resume replays what already succeeded instead of paying for it again.
        if let Some((_, text)) = self.replay.iter().find(|(id, _)| *id == node.id) {
            let previous = crate::flowruns::read(&self.run_id).and_then(|r| r.node(&node.id).cloned());
            return Plan::Go(NodeWork::Replay(Box::new(NodeOut {
                ok: true,
                output: text.clone(),
                exit: previous.as_ref().and_then(|p| p.exit),
                approved: previous.as_ref().is_some_and(|p| p.approved),
                attempts: previous.as_ref().map_or(1, |p| p.attempts),
                ..NodeOut::default()
            })));
        }
        // The condition, evaluated on results that are already in hand.
        if let Some(when) = &node.when {
            if !when.eval(&|name| self.facts(name, done, status)) {
                self.board.settled(&node.id, crate::flow::board::State::Skipped, 0, 0, &format!("not {}", node.when_src));
                return Plan::Skip;
            }
        }
        let fill = |t: &crate::flow::tmpl::Template, item: Option<&str>| t.render(&|r| self.resolve(r, done, item));
        // A fan-out resolves its list here, on the scheduler's thread, so each item's
        // work is complete before any of it is handed to a thread.
        let items: Vec<Option<String>> = match &node.over {
            Some(over) => {
                let list = self.items(&fill(over, None));
                if list.is_empty() {
                    self.board.settled(&node.id, crate::flow::board::State::Skipped, 0, 0, "nothing to fan out over");
                    return Plan::Skip;
                }
                list.into_iter().map(Some).collect()
            }
            None => vec![None],
        };
        let each = |t: &crate::flow::tmpl::Template| items.iter().map(|it| fill(t, it.as_deref())).collect::<Vec<_>>();
        Plan::Go(match &node.kind {
            crate::flow::Kind::Agent { agent, prompt } => NodeWork::Agent { agent: agent.clone(), prompts: each(prompt) },
            crate::flow::Kind::Run { command } => NodeWork::Run { commands: each(command) },
            crate::flow::Kind::Approve { show, question } => {
                NodeWork::Approve { show: fill(show, None), question: question.clone() }
            }
        })
    }

    fn work(&self, i: usize, w: NodeWork) -> NodeOut {
        let node = &self.flow.nodes[i];
        if let NodeWork::Replay(out) = w {
            let ms = out.ms;
            let tokens = out.input_tokens + out.output_tokens;
            self.board.settled(&node.id, crate::flow::board::State::Done, ms, tokens, "replayed from the record");
            return *out;
        }
        self.board.running(&node.id, &running_note(&w));
        let started = std::time::Instant::now();
        let mut out = self.attempt(node, &w);
        out.ms = started.elapsed().as_millis() as u64;
        let shown = if out.parked {
            crate::flow::board::State::Parked
        } else if out.ok {
            crate::flow::board::State::Done
        } else {
            crate::flow::board::State::Failed
        };
        let note = if out.ok { String::new() } else { opening_line(&out.output) };
        self.board.settled(&node.id, shown, out.ms, out.input_tokens + out.output_tokens, &note);
        let state = if out.parked {
            crate::flowruns::NodeState::Waiting
        } else if out.ok {
            crate::flowruns::NodeState::Done
        } else {
            crate::flowruns::NodeState::Failed
        };
        self.record(node, &asked_text(&w), &out, state);
        out
    }

    fn ok(&self, _i: usize, out: &NodeOut) -> bool {
        out.ok
    }

    fn halted(&self) -> bool {
        if self.cancel.is_cancelled() {
            return true;
        }
        match self.budget {
            Some(b) => self.spent.load(std::sync::atomic::Ordering::Relaxed) >= b,
            None => false,
        }
    }
}

impl FlowDriver<'_> {
    /// Run a node, retrying a failure up to its `retry` count. Each attempt is a
    /// fresh run; the count survives into the record so a flaky node is visible.
    fn attempt(&self, node: &crate::flow::Node, w: &NodeWork) -> NodeOut {
        let mut last = NodeOut::default();
        for attempt in 0..=node.retry {
            if attempt > 0 {
                self.board.retrying(&node.id, attempt, node.retry);
            }
            last = self.once(node, w);
            last.attempts = attempt + 1;
            if last.ok || last.parked || self.halted() {
                break;
            }
        }
        last
    }

    fn once(&self, node: &crate::flow::Node, w: &NodeWork) -> NodeOut {
        match w {
            NodeWork::Replay(out) => (**out).clone(),
            NodeWork::Approve { show, question } => self.ask(show, question),
            NodeWork::Run { commands } => join(commands.iter().map(|c| self.one_command(c, node)).collect()),
            NodeWork::Agent { agent, prompts } => {
                // One prompt is the common case; several mean the node fans out, and
                // the items are independent by construction — each was resolved
                // before any of them started.
                if prompts.len() == 1 {
                    return self.one_agent(agent, &prompts[0], node);
                }
                let width = self.concurrency.max(1);
                let mut results: Vec<NodeOut> = Vec::with_capacity(prompts.len());
                for batch in prompts.chunks(width) {
                    let done = std::thread::scope(|scope| {
                        let handles: Vec<_> = batch
                            .iter()
                            .map(|p| scope.spawn(move || self.one_agent(agent, p, node)))
                            .collect();
                        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect::<Vec<_>>()
                    });
                    results.extend(done);
                    if self.halted() {
                        break;
                    }
                }
                join(results)
            }
        }
    }
}

/// Fold a fan-out's results into one: every part must pass, and the outputs read as
/// a numbered list so a later node can tell them apart.
fn join(parts: Vec<NodeOut>) -> NodeOut {
    if parts.len() == 1 {
        return parts.into_iter().next().unwrap_or_default();
    }
    let mut out = NodeOut { ok: true, attempts: 1, ..NodeOut::default() };
    let mut text = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        out.ok &= p.ok;
        out.input_tokens += p.input_tokens;
        out.output_tokens += p.output_tokens;
        out.tools += p.tools;
        out.ms = out.ms.max(p.ms);
        // The last non-zero exit, so a fan-out of commands reports a real failure.
        if p.exit.is_some_and(|e| e != 0) || out.exit.is_none() {
            out.exit = p.exit;
        }
        text.push(format!("## {}\n{}", i + 1, p.output.trim()));
    }
    out.output = text.join("\n\n");
    out
}

/// What a node is, in the few characters the board gives it.
fn describe_node(node: &crate::flow::Node) -> String {
    match &node.kind {
        crate::flow::Kind::Agent { agent, .. } if node.is_map() => format!("@{agent} \u{d7}n"),
        crate::flow::Kind::Agent { agent, .. } => format!("@{agent}"),
        crate::flow::Kind::Run { command } => format!("$ {}", command.source().split_whitespace().take(2).collect::<Vec<_>>().join(" ")),
        crate::flow::Kind::Approve { .. } => "asks you".into(),
    }
}

/// What to say beside a node the moment it starts.
///
/// Usually nothing: the board's own column already says what the node is, and
/// repeating it there costs the space a tool trace is about to need. A fan-out is the
/// exception — how many items it turned out to be is not knowable from the file.
fn running_note(w: &NodeWork) -> String {
    match w {
        NodeWork::Agent { prompts, .. } if prompts.len() > 1 => format!("\u{d7} {} items", prompts.len()),
        NodeWork::Run { commands } if commands.len() > 1 => format!("\u{d7} {} items", commands.len()),
        _ => String::new(),
    }
}

/// What the node was asked, for its record.
fn asked_text(w: &NodeWork) -> String {
    match w {
        NodeWork::Agent { prompts, .. } => prompts.join("\n\n---\n\n"),
        NodeWork::Run { commands } => commands.join("\n"),
        NodeWork::Approve { show, question } => format!("{show}\n\n{question}"),
        NodeWork::Replay(_) => String::new(),
    }
}

/// `aiTerminal ai flow <name> "<input>"` — verify, then run the graph.
fn run_flow_cli(spec: FlowSpec, resume: Option<String>) -> i32 {
    crate::config::Config::ensure_default();
    let cfg = crate::config::Config::load();
    crate::i18n::install(cfg.i18n_catalog());

    // A resume takes its flow and its input from the record, so continuing a run
    // never quietly becomes a different one.
    let prior = match &resume {
        Some(id) => match crate::flowruns::read(id) {
            Some(run) => Some(run),
            None => {
                eprintln!("aiTerminal: flow run {id} has no record to resume");
                return 2;
            }
        },
        None => None,
    };
    // A goal with no flow name: ask the model which of the installed graphs it wants,
    // and say so out loud before anything runs.
    let mut routed: Option<String> = None;
    let name = match prior.as_ref() {
        Some(p) => p.flow.clone(),
        None if !spec.name.is_empty() => spec.name.clone(),
        None => {
            let catalogue: Vec<(String, String)> = flow_names()
                .into_iter()
                .filter_map(|n| load_flow(&n).ok().map(|f| (n, f.description)))
                .collect();
            match crate::flow::pick::choose(&spec.input, &catalogue) {
                Ok((picked, why)) => {
                    routed = Some(why);
                    picked
                }
                Err(e) => {
                    eprintln!("aiTerminal: {e}");
                    eprintln!("  name one instead:  {}", catalogue.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(" · "));
                    return 2;
                }
            }
        }
    };
    let (flow, report) = match checked_flow(&name) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            return 2;
        }
    };
    if !report.ok() {
        eprintln!("aiTerminal: flow '{name}' cannot run:");
        print_report(&name, &report, flow.nodes.len());
        return 2;
    }
    let input = prior.as_ref().map_or(spec.input.clone(), |p| p.input.clone());
    if flow.input == crate::flow::Input::Required && input.trim().is_empty() {
        eprintln!("aiTerminal: flow '{name}' needs something to work on \u{2014} @flow {name} \"<what to do>\"");
        return 2;
    }

    // Bounds: the flags win, then the file, then `[flow]` config.
    let timeout = spec.timeout.or(flow.bounds.timeout).unwrap_or(cfg.flow_timeout);
    let budget = spec.budget.or(flow.bounds.budget);
    let concurrency = spec.concurrency.or(flow.bounds.concurrency).unwrap_or(cfg.flow_concurrency).clamp(1, 16);

    if spec.dry_run {
        let (dim, r) = (muted(), reset());
        println!("{}\u{25B8} flow '{name}'{r} {dim}\u{b7} {}{r}", accent(), shape_of(&flow));
        if let Some(why) = &routed {
            println!("  {dim}chosen for this goal \u{2014} {why}{r}");
        }
        for line in crate::flow::render::draw(&flow, None, md_width()) {
            println!("{line}");
        }
        println!(
            "\n  {dim}bounds{r}    {} \u{b7} {concurrency} at a time{}",
            crate::flowruns::human_age(timeout),
            budget.map(|b| format!(" \u{b7} {b} tokens")).unwrap_or_default()
        );
        print_report(&name, &report, flow.nodes.len());
        return 0;
    }

    // Only an agent node needs a model. A graph of `run` nodes is a perfectly good
    // flow and must not be blocked by an unconfigured key.
    let needs_model = flow.nodes.iter().any(|n| matches!(n.kind, crate::flow::Kind::Agent { .. }));
    let settings = cfg.ai_settings();
    if needs_model && settings.resolve_key().is_none() {
        eprintln!("aiTerminal: {}", crate::ai::setup_hint(&settings));
        return 2;
    }

    let registry = crate::plugin::load_registry(&cfg);
    let policy = std::sync::Arc::new(crate::security::build_policy(&cfg, &registry));
    let workspace = std::env::current_dir().ok();

    // The record exists from the first moment, so a run killed at node one is still
    // something you can look at.
    let run_id = prior.as_ref().map_or_else(crate::flowruns::new_id, |p| p.id.clone());
    let rows: Vec<crate::flowruns::NodeRun> = flow
        .nodes
        .iter()
        .map(|n| match prior.as_ref().and_then(|p| p.node(&n.id)) {
            Some(previous) if previous.state == crate::flowruns::NodeState::Done => previous.clone(),
            _ => crate::flowruns::NodeRun { id: n.id.clone(), ..crate::flowruns::NodeRun::default() },
        })
        .collect();
    let record = crate::flowruns::Run {
        id: run_id.clone(),
        flow: name.clone(),
        input: input.clone(),
        status: "running".into(),
        cwd: workspace.as_ref().map(|w| w.display().to_string()).unwrap_or_default(),
        started: prior.as_ref().map_or_else(crate::flowruns::now, |p| p.started),
        finished: None,
        pid: std::process::id(),
        timeout,
        budget,
        concurrency,
        nodes: rows.clone(),
    };
    crate::flowruns::write(&run_id, &record);

    // What a resume already has in hand: the finished nodes' answers, read back off
    // disk so they cost nothing the second time.
    let replay: Vec<(String, String)> = prior
        .as_ref()
        .map(|p| {
            p.nodes
                .iter()
                .filter(|n| n.state == crate::flowruns::NodeState::Done)
                .filter_map(|n| crate::flowruns::read_node(&run_id, &n.id).map(|text| (n.id.clone(), text)))
                .collect()
        })
        .unwrap_or_default();

    let cancel = crate::ai::CancelToken::new();
    let sigint = wire_sigint(cancel.clone());
    let _clock = wire_deadline(cancel.clone(), timeout);
    let (dim, r) = (muted(), reset());
    if let Some(why) = &routed {
        // The pick is printed BEFORE the first node, so a wrong one is a line to read
        // rather than three nodes to sit through.
        eprintln!("{}\u{25B8} {name}{r} {dim}\u{2014} {why}{r}", accent());
    }
    if !replay.is_empty() {
        eprintln!("{dim}\u{21ba} resuming {run_id} \u{2014} {} node(s) already done{r}", replay.len());
    }
    for w in &report.warnings {
        eprintln!("{dim}\u{26a0}  {w}{r}");
    }

    // One line per node, in graph order, from before the first one starts — so the
    // shape of the run is visible rather than revealed a line at a time.
    let heading = match input.trim().is_empty() {
        true => format!("{name} \u{b7} {}", shape_of(&flow)),
        false => format!("{name} \u{b7} {}", clip_tail(input.trim(), 62)),
    };
    let board = crate::flow::board::Board::new(
        heading,
        flow.nodes
            .iter()
            .map(|n| (n.id.clone(), describe_node(n), n.when_src.clone()))
            .collect(),
        // A repainting board needs a cursor. A pipe, a job log and CI have none.
        err_is_tty(),
    );
    board.start();

    let driver = FlowDriver {
        flow: &flow,
        cfg: &cfg,
        settings,
        policy,
        workspace,
        input,
        run_id: run_id.clone(),
        replay,
        cancel: cancel.clone(),
        budget,
        spent: std::sync::atomic::AtomicU64::new(0),
        concurrency,
        interactive: stdin_is_tty(),
        rows: std::sync::Mutex::new(rows),
        board: board.clone(),
    };
    let nodes = graph_nodes(&flow);
    let started = std::time::Instant::now();
    let result = platform::orchestrator::run_graph(&nodes, &driver, concurrency);
    drop(sigint);
    board.finish();

    // ── the outcome ───────────────────────────────────────────────────────
    use platform::orchestrator::Status;
    let parked = result.results.iter().flatten().any(|o| o.parked);
    let failed = result.status.iter().any(|s| matches!(s, Status::Failed | Status::Blocked));
    let status = if parked {
        "waiting"
    } else if cancel.is_cancelled() && started.elapsed().as_secs() >= timeout {
        "timeout"
    } else if cancel.is_cancelled() {
        "cancelled"
    } else if driver.halted() {
        "budget"
    } else if failed {
        "failed"
    } else {
        "done"
    };
    let final_rows = driver.rows.lock().map(|r| r.clone()).unwrap_or_default();
    let mut final_rows = final_rows;
    for (i, node) in flow.nodes.iter().enumerate() {
        if let Some(row) = final_rows.iter_mut().find(|r| r.id == node.id) {
            if row.state == crate::flowruns::NodeState::Pending {
                row.state = match result.status[i] {
                    Status::Skipped => crate::flowruns::NodeState::Skipped,
                    Status::Blocked => crate::flowruns::NodeState::Blocked,
                    _ => crate::flowruns::NodeState::Pending,
                };
            }
        }
    }
    let mut record = crate::flowruns::read(&run_id).unwrap_or(record);
    record.nodes = final_rows;
    record.status = status.into();
    record.finished = Some(crate::flowruns::now());
    crate::flowruns::write(&run_id, &record);
    crate::flowruns::prune(cfg.flow_keep_runs);

    // The answer: the node the flow says is its answer, printed to stdout so the
    // whole thing composes with a pipe like every other command here.
    if let Some(answer) = flow.answer_node().and_then(|i| result.results[i].as_ref()) {
        if !answer.output.trim().is_empty() {
            println!("{}", answer.output.trim());
        }
    }
    let (tin, tout) = record.tokens();
    let cost = Some(cfg.ai_settings().primary().cost(tin, tout));
    let glyph = match status {
        "done" => "\u{2713}",
        "waiting" => "\u{23f8}",
        "cancelled" => "\u{23f9}",
        _ => "\u{2717}",
    };
    eprintln!("{dim}{}{r}", run_footer_with(glyph, started.elapsed(), record.tools(), tin, tout, cost, cfg.ai_budget));
    if parked {
        eprintln!("{dim}{}{r}", crate::i18n::translate("flow.resume_hint", &[run_id.clone()]));
    }
    match status {
        "done" => 0,
        "waiting" => 0,
        _ => 1,
    }
}

/// The flow, as the scheduler sees it: edges by index, and the two flags it needs.
fn graph_nodes(flow: &crate::flow::Flow) -> Vec<platform::orchestrator::Node> {
    flow.nodes
        .iter()
        .map(|n| platform::orchestrator::Node {
            needs: n.needs.iter().filter_map(|d| flow.index(d)).collect(),
            goto: n.goto.as_ref().and_then(|g| flow.index(g)),
            max_loops: if n.goto.is_some() { n.max } else { 0 },
            solo: n.solo,
            optional: n.optional,
            // One rule, and it is the whole story: `needs` decides the ORDER, `when`
            // decides whether it runs. So a node that carries a condition always gets
            // to evaluate it — a fixer is not blocked by the breakage it exists to
            // handle, and a node conditioned on success is *skipped* rather than
            // *blocked*, which is what actually happened. A node with no condition
            // keeps the safe default: a failed dependency stops it.
            guarded: n.when.is_some(),
        })
        .collect()
}

// ===== @loop — an engineered agent loop (iterate until a verifiable goal) =====
//
// Loop engineering in one sentence: don't perfect a single prompt — design the loop the agent
// runs inside. Seven pieces make that real here:
//
//   1. A VERIFIABLE GOAL.  `--check "<cmd>"` is a binary stop condition: exit 0 = done, no
//      judgment involved. When nobody supplies one, the model is asked for one ONCE
//      (`ai/verify.rs`) — because the alternative, a model grading its own work, is the
//      single most-cited way agent loops fail. Only if that yields nothing does the
//      maker/checker split take over: a SEPARATE reviewer agent grades each iteration.
//   2. PROVEN BEFORE IT COSTS ANYTHING.  The check runs once BEFORE iteration 1. Guard-denied
//      or unrunnable → a setup error with nothing spent. Already passing → the goal was
//      already met. Otherwise its failure output seeds iteration 1, so the maker's first
//      attempt starts on the real error instead of a guess.
//   3. STRUCTURED FEEDBACK.  The verifier's output (tail-capped) feeds the next iteration,
//      alongside a compact line per past attempt — enough to avoid a dead end, small enough
//      that a failed transcript never poisons the next try.
//   4. STOP RULES.  Success · `--max N` · `--budget TOKENS` · `--timeout 30m` · no-progress.
//      Iterations, tokens and wall clock are three independent ways to run away, so all three
//      are bounded. No-progress remembers the last few verifier observations, so a loop that
//      oscillates between two bad states is caught, not just one that repeats itself.
//   5. ONE ESCALATION.  The first no-progress verdict does not end the run: the maker gets
//      one more iteration, told what has already been tried and asked for a materially
//      different approach. A second one ends it.
//   6. GUARDRAILS.  The check command passes the command guard (deny blocks it; confirm-tier
//      is refused in this non-interactive path); the agent's tools stay gated as in any run.
//   7. STATE THAT SURVIVES.  Every iteration is written to `ai/loops/<id>/`, so a run can be
//      read (`@loop log`), inspected (`@loop show`) and continued (`@loop resume`) with what
//      is left of each bound. `--bg` still makes the whole loop a tracked job.

/// One iteration's verification outcome.
#[derive(Debug)]
pub(crate) struct Verdict {
    passed: bool,
    /// Feedback fed into the next iteration (failure output / reviewer notes).
    feedback: String,
    /// A signature of the verifier's observation. The loop remembers the last few: seeing one
    /// again means no progress, whether it repeated or oscillated back to it.
    signature: u64,
    /// The check command's exit status, when there was a command. `127`/`126` mean the
    /// verifier itself is broken — a distinction that matters before the loop starts.
    code: Option<i32>,
    /// The command's output, undecorated. `feedback` is shaped for a loop to read
    /// back to a model; a `@flow` node's `{{x.output}}` is what the command printed
    /// and nothing else, with the status available separately as `{{x.exit}}`.
    raw: String,
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
fn loop_prompt(goal: &str, k: u32, max: u32, check: Option<&str>, feedback: &str, tried: &[String], shift: bool) -> String {
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
    // The attempt log. Two lines of "this was already tried and did not work" is what stops
    // iteration 4 from rediscovering iteration 2's dead end.
    if !tried.is_empty() {
        p.push_str("\n## Already attempted (do not repeat these)\n");
        for line in tried.iter().rev().take(LOG_LINES).rev() {
            p.push_str(&format!("- {line}\n"));
        }
    }
    if shift {
        p.push_str(
            "\n## The last approach is not working\n\
             The verifier has returned to a state it has already been in, so continuing to \
             refine the current approach will not converge. Take a MATERIALLY DIFFERENT one: \
             re-read the relevant code, question an assumption the previous attempts shared, \
             and say in one line what you are doing differently before you do it.\n",
        );
    }
    p
}

/// How many past attempts ride along in the prompt — recent ones carry the signal, and the
/// whole log would just be transcript by another name.
const LOG_LINES: usize = 6;

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
    let raw = tail(&text, 4000).to_string();
    let observed = format!("exit={:?}\n{raw}", status.code());
    Ok(Verdict { passed, feedback: observed.clone(), signature: fnv1a(&observed), code: status.code(), raw })
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
    Verdict { signature: fnv1a(&answer), raw: answer.clone(), feedback: answer, passed, code: None }
}

/// Why an engineered loop stopped. Every one of these is a *bound* doing its job, except
/// `Error` — and each maps to an exit code and a record status, so a script and a person read
/// the same truth.
#[derive(Debug, PartialEq)]
enum LoopOutcome {
    /// The verifier passed on iteration N.
    Done(u32),
    /// The verifier returned to an observation it had already produced — and the one
    /// strategy-shift escalation had already been spent.
    Stalled,
    /// The iteration cap was reached without passing.
    Exhausted,
    /// The token budget ran out.
    Budget,
    /// The wall clock ran out.
    Timeout,
    /// The verifier itself failed (e.g. the check command was guard-blocked).
    Error(String),
    /// The user interrupted (Ctrl+C).
    Cancelled,
}

impl LoopOutcome {
    /// The record status this outcome writes.
    fn status(&self) -> &'static str {
        match self {
            LoopOutcome::Done(_) => "done",
            LoopOutcome::Stalled => "stalled",
            LoopOutcome::Exhausted => "exhausted",
            LoopOutcome::Budget => "budget",
            LoopOutcome::Timeout => "timeout",
            LoopOutcome::Error(_) => "error",
            LoopOutcome::Cancelled => "cancelled",
        }
    }
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
    state: &mut LoopState,
    check_label: Option<&str>,
    mut verify: impl FnMut(&str) -> Result<Verdict, String>,
) -> LoopRun {
    let mut st = LoopRun { outcome: LoopOutcome::Exhausted, iters: 0, tin: 0, tout: 0, tools: 0 };
    // `seen` is the no-progress memory. Consecutive repeats are the obvious case; a loop that
    // flips between two bad states (A→B→A→B) is the same failure wearing a disguise, and a
    // "was the last one identical?" test never catches it.
    let mut seen: Vec<u64> = state.seen.clone();
    let mut spent: u64 = 0;
    let first = state.done + 1;
    let last = state.done + state.left.max;
    for k in first..=last {
        // Time is a bound in its own right: iterations and tokens both say nothing about an
        // agent that is simply slow. Checked before the count moves, so a run that stops here
        // reports the iterations it actually ran.
        if state.out_of_time() {
            return LoopRun { outcome: LoopOutcome::Timeout, ..st };
        }
        st.iters = k - state.done;
        eprintln!("\u{25B6} {}", crate::i18n::translate("loop.iteration", &[k.to_string(), last.to_string()]));
        let prompt = loop_prompt(goal, k, last, check_label, &state.feedback, &state.tried, state.shifting);
        let run = crate::ai::run_agent(client, maker, &prompt, "", runner, observer);
        st.tin += run.input_tokens as u64;
        st.tout += run.output_tokens as u64;
        st.tools += run.steps.len();
        spent += (run.input_tokens + run.output_tokens) as u64;
        state.shifting = false;
        // An errored/cancelled iteration is NOT work — never hand it to the
        // verifier as if it were; stop the loop with the real cause.
        match &run.outcome {
            crate::ai::RunOutcome::Cancelled if state.out_of_time() => {
                // The watchdog cancels through the same token Ctrl+C uses; the deadline says
                // which one it really was.
                return LoopRun { outcome: LoopOutcome::Timeout, ..st };
            }
            crate::ai::RunOutcome::Cancelled => return LoopRun { outcome: LoopOutcome::Cancelled, ..st },
            crate::ai::RunOutcome::Error(e) => return LoopRun { outcome: LoopOutcome::Error(e.clone()), ..st },
            _ => {}
        }

        let verdict = match verify(&run.answer) {
            Ok(v) => v,
            Err(e) => return LoopRun { outcome: LoopOutcome::Error(e), ..st },
        };
        state.note(k, &run.answer, &verdict.feedback);
        if verdict.passed {
            return LoopRun { outcome: LoopOutcome::Done(k), ..st };
        }
        if seen.contains(&verdict.signature) {
            // No progress. Spend the one escalation — a *different* approach, told what has
            // already been tried — before calling it stalled. If that lands here again, the
            // loop really is stuck and more iterations only cost money.
            if state.escalated {
                return LoopRun { outcome: LoopOutcome::Stalled, ..st };
            }
            state.escalated = true;
            state.shifting = true;
            eprintln!("\u{21BB} {}", crate::i18n::translate("loop.shift", &[]));
        }
        seen.push(verdict.signature);
        if seen.len() > SIGNATURE_MEMORY {
            seen.remove(0);
        }
        state.seen = seen.clone();
        state.feedback = verdict.feedback;
        if let Some(b) = state.left.budget {
            if spent >= b {
                return LoopRun { outcome: LoopOutcome::Budget, ..st };
            }
        }
    }
    st
}

/// How many past verifier observations count as "have I been here before?".
const SIGNATURE_MEMORY: usize = 4;

/// A verdict from a scripted observation: `PASS` passed, anything else is what the verifier
/// saw. The scenario seam — the loop's rules are about observations, not about processes.
#[cfg(test)]
pub(crate) fn scripted_verdict(observed: &str) -> Verdict {
    Verdict {
        passed: observed.trim().eq_ignore_ascii_case("PASS"),
        feedback: observed.to_string(),
        raw: observed.to_string(),
        signature: fnv1a(observed),
        code: None,
    }
}

/// What one scenario-driven loop produced.
#[cfg(test)]
pub(crate) struct TestRun {
    /// The record status this outcome writes (`done`, `stalled`, `timeout`, …).
    pub(crate) stopped: String,
    pub(crate) iters: u32,
    pub(crate) tin: u64,
    pub(crate) tout: u64,
    pub(crate) tools: usize,
}

/// Run [`drive_loop`] with no observer and no tools — everything a scenario needs, and
/// nothing it would have to construct itself.
#[cfg(test)]
pub(crate) fn drive_loop_for_test<T: crate::ai::Transport>(
    client: &crate::ai::Client<T>,
    maker: &crate::ai::AgentSpec,
    state: &mut LoopState,
    goal: &str,
    check_label: Option<&str>,
    verify: impl FnMut(&str) -> Result<Verdict, String>,
) -> TestRun {
    struct NoTools;
    impl crate::ai::ToolRunner for NoTools {
        fn run(&mut self, _: &str, _: &str) -> Result<String, String> {
            Err("no tools in this scenario".into())
        }
    }
    let run = drive_loop(client, maker, &mut NoTools, &mut crate::ai::NoopObserver, goal, state, check_label, verify);
    TestRun { stopped: run.outcome.status().into(), iters: run.iters, tin: run.tin, tout: run.tout, tools: run.tools }
}

/// What carries across iterations — and, when the record is written, across runs.
///
/// This is the loop's state file in memory: where it got to, what the verifier last said, and
/// a compact line per attempt. Carrying notes instead of a transcript is deliberate — a long
/// failed transcript poisons the next attempt, a two-line summary of it does not.
#[derive(Debug, Default)]
pub(crate) struct LoopState {
    /// Iterations already completed (non-zero only on a resume).
    pub(crate) done: u32,
    /// What is still allowed: iterations, tokens, and the wall clock.
    pub(crate) left: crate::loops::Bounds,
    /// The verifier's last observation — what the next iteration works on.
    pub(crate) feedback: String,
    /// One line per attempt, oldest first.
    pub(crate) tried: Vec<String>,
    /// Past observation signatures (the no-progress memory).
    pub(crate) seen: Vec<u64>,
    /// The single strategy-shift escalation has been spent.
    pub(crate) escalated: bool,
    /// The next iteration is the strategy shift.
    pub(crate) shifting: bool,
    /// When the whole run must stop, as an `Instant` deadline.
    pub(crate) deadline: Option<std::time::Instant>,
}

impl LoopState {
    fn out_of_time(&self) -> bool {
        self.deadline.is_some_and(|d| std::time::Instant::now() >= d)
    }

    /// Record one attempt: a single line naming what was done and what came back.
    fn note(&mut self, k: u32, answer: &str, observed: &str) {
        let did = first_line(answer, 90);
        let got = first_line(observed, 70);
        self.tried.push(format!("{k}: {did} \u{2192} {got}"));
    }
}

/// The first meaningful line of a block, clipped — enough to recognise an attempt, small
/// enough that a dozen of them still fit in a prompt.
fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("(nothing)");
    if line.chars().count() <= max {
        return line.to_string();
    }
    line.chars().take(max.saturating_sub(1)).collect::<String>() + "\u{2026}"
}

/// `ai loop "<goal>" …` — iterate the maker agent until the verifier passes or a bound fires.
///
/// With `resume`, everything comes from that record instead: the goal, the verifier, the
/// bounds, and how much of each is left.
///
/// Exit codes: 0 = goal reached · 1 = a bound stopped it · 2 = setup error · 130 = interrupted.
fn run_loop_cli(spec: LoopSpec, resume: Option<String>) -> i32 {
    let cfg = crate::config::Config::load();
    crate::i18n::install(cfg.i18n_catalog());
    let settings = cfg.ai_settings();
    if settings.resolve_key().is_none() {
        eprintln!("aiTerminal: {}", crate::ai::setup_hint(&settings));
        return 2;
    }
    let prior = match &resume {
        Some(id) => match crate::loops::read(id) {
            Some(run) => Some(run),
            None => {
                eprintln!("aiTerminal: loop {id} has no record to resume");
                return 2;
            }
        },
        None => None,
    };
    // `@<path>` attachments work in loops too (images/PDFs + inlined text files).
    let (goal, media, file_ctx) = collect_attachments(prior.as_ref().map_or(spec.goal.as_str(), |p| p.goal.as_str()));
    let goal = match file_ctx.is_empty() {
        true => goal,
        false => format!("{goal}\n{file_ctx}"),
    };
    let goal = goal.as_str();

    let registry = crate::plugin::load_registry(&cfg);
    let policy = std::sync::Arc::new(crate::security::build_policy(&cfg, &registry));
    let workspace = std::env::current_dir().ok();
    let session = crate::ai::Session::for_cwd();
    let agent_name = prior.as_ref().map_or_else(
        || spec.agent.clone().unwrap_or_else(|| "coder".into()),
        |p| p.agent.clone(),
    );
    let Some(mut maker) = build_agent_spec(&agent_name, context_settings(&cfg)) else {
        eprintln!("aiTerminal: no agent '{agent_name}' — {}", available_agents_hint());
        return 2;
    };

    // The bounds: a resume gets what its record has left, a fresh run gets the flags over the
    // `[loop]` defaults. All three are always set — a loop with no ceiling is not a loop.
    let bounds = match &prior {
        // A resume starts from what the record has left, but a bound named on the command
        // line replaces it: `@loop resume last --budget 200000` means "here is more rope".
        Some(p) => {
            let left = p.remaining();
            crate::loops::Bounds {
                max: spec.max.unwrap_or(left.max).clamp(0, 25),
                budget: spec.budget.or(left.budget),
                timeout: spec.timeout.unwrap_or(left.timeout),
            }
        }
        None => crate::loops::Bounds {
            max: spec.max.unwrap_or(cfg.loop_max).clamp(1, 25),
            budget: spec.budget,
            timeout: spec.timeout.unwrap_or(cfg.loop_timeout),
        },
    };
    if bounds.max == 0 {
        eprintln!("aiTerminal: loop {} has no iterations left — start a new one", resume.unwrap_or_default());
        return 2;
    }

    // The verifier, decided ONCE: an explicit `--check` wins, then the AI's proposal (which
    // the guard still adjudicates), and the reviewer agent backs both up.
    let check_deadline = std::time::Duration::from_secs(cfg.loop_check_timeout);
    let verifier = match &prior {
        Some(p) => p.verifier.clone(),
        None => choose_verifier(&spec, &cfg, goal, &policy),
    };
    eprintln!(
        "{}\u{1F501} {}{}",
        accent(),
        crate::i18n::translate("loop.start", &[agent_name.clone(), bounds.max.to_string()]),
        reset()
    );
    eprintln!("  {}{}{}", muted(), crate::i18n::translate("loop.verifier", &[verifier.describe()]), reset());

    // Pre-flight: prove the verifier before spending anything on the maker. A check that the
    // guard refuses, or that cannot run at all, is a setup error — not something to discover
    // after paying for a full agent turn. And a check that already passes means there is
    // nothing to do.
    let mut seed = String::new();
    if let Some(cmd) = verifier.command() {
        match run_check(cmd, &policy, check_deadline) {
            Err(e) => {
                eprintln!("aiTerminal: {e}");
                return 2;
            }
            Ok(v) if v.passed => {
                eprintln!("\u{2713} {}", crate::i18n::translate("loop.already", &[]));
                return 0;
            }
            // 127 = not found, 126 = not executable. That is not "the goal is unmet", it is a
            // verifier that can never pass — and a loop whose stop condition is impossible
            // will spend its whole budget proving it.
            Ok(v) if matches!(v.code, Some(126) | Some(127)) => {
                eprintln!("aiTerminal: the check command `{cmd}` could not be run \u{2014} exit {}", v.code.unwrap_or(0));
                return 2;
            }
            // It fails, as expected — so iteration 1 starts from the real error instead of
            // guessing at it.
            Ok(v) => seed = v.feedback,
        }
    }
    if spec.dry_run {
        println!("{}{goal}{}", accent(), reset());
        println!("  verifier  {}", verifier.describe());
        println!("  maker     @{agent_name}");
        let budget = bounds.budget.map(|b| format!(" \u{b7} {b} tokens")).unwrap_or_default();
        println!(
            "  bounds    {} iteration(s) \u{b7} {}{budget}",
            bounds.max,
            crate::loops::human_age(bounds.timeout)
        );
        return 0;
    }

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
    // The wall clock, enforced the same way Ctrl+C is: an in-flight model turn stops at the
    // deadline instead of the loop only noticing once the turn finally returns.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(bounds.timeout);
    let _watchdog = wire_deadline(cancel.clone(), bounds.timeout);
    let client = crate::ai::Client::new(settings.clone(), crate::ai::CurlTransport::default()).with_images(media).with_cancel(cancel);
    let mut runner = build_runner(&cfg, &settings, workspace, policy.clone(), true);
    if let Some(hub) = &runner.mcp {
        for (name, describe) in hub.tools() {
            maker.tools.push(crate::ai::ToolSpec { name, describe });
        }
    }

    // The record exists from the first moment, so a crash, a Ctrl+C or a closed lid still
    // leaves something to read and resume.
    let id = resume.clone().unwrap_or_else(crate::loops::new_id);
    let mut record = prior.clone().unwrap_or_else(|| crate::loops::Run {
        id: id.clone(),
        goal: goal.to_string(),
        agent: agent_name.clone(),
        status: "running".into(),
        verifier: verifier.clone(),
        bounds,
        cwd: cwd_string(),
        started: crate::loops::now(),
        finished: None,
        pid: std::process::id(),
        progress: crate::loops::Progress::default(),
    });
    record.status = "running".into();
    record.pid = std::process::id();
    record.finished = None;
    crate::loops::write(&id, &record);

    let mut state = LoopState {
        done: record.progress.iterations,
        left: bounds,
        // A resume continues from what the verifier last said; a fresh run from the
        // pre-flight failure.
        feedback: if resume.is_some() { record.progress.feedback.clone() } else { seed },
        tried: record.progress.tried.clone(),
        seen: Vec::new(),
        escalated: record.progress.escalated,
        shifting: false,
        deadline: Some(deadline),
    };

    let started = std::time::Instant::now();
    let sub = runner.sub.clone();
    let cap_ctx = runner.ctx.clone();
    let keep = cfg.loop_keep_runs;
    let log_id = id.clone();
    let mut n = state.done;
    let verifier_cmd = verifier.command().map(str::to_string);
    let verify = |answer: &str| {
        let verdict = match &verifier_cmd {
            Some(cmd) => run_check(cmd, &cap_ctx.policy, check_deadline)?,
            None => run_reviewer(&sub, cap_ctx.clone(), goal, answer),
        };
        // Write the iteration down as it happens — a run that is killed mid-flight still
        // leaves every completed iteration on disk.
        n += 1;
        crate::loops::write_iteration(&log_id, keep, n, answer, &verdict.feedback);
        Ok(verdict)
    };
    let mut obs = CliObserver::new(std::io::stdout());
    let run = drive_loop(&client, &maker, &mut runner, &mut obs, goal, &mut state, verifier.command(), verify);
    let _ = { use std::io::Write; std::io::stdout().write_all(b"\n") };

    let (dim, r) = (muted(), reset());
    let (code, glyph, digest) = match &run.outcome {
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
        LoopOutcome::Timeout => {
            eprintln!("\u{26D4} {}", crate::i18n::translate("loop.timeout", &[]));
            (1, "\u{26a0}", "ran out of time".into())
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

    record.status = run.outcome.status().into();
    record.finished = Some(crate::loops::now());
    record.pid = 0;
    record.progress = crate::loops::Progress {
        iterations: state.done + run.iters,
        input_tokens: record.progress.input_tokens + run.tin,
        output_tokens: record.progress.output_tokens + run.tout,
        tools: record.progress.tools + run.tools,
        feedback: state.feedback.clone(),
        tried: state.tried.clone(),
        escalated: state.escalated,
    };
    crate::loops::write(&id, &record);
    crate::loops::prune(cfg.loop_keep_runs);

    // The same footer as agent/flow, with iterations in place of a lone elapsed count.
    let cost = Some(client.model().cost(run.tin, run.tout));
    let footer = run_footer_with(glyph, started.elapsed(), run.tools, run.tin, run.tout, cost, cfg.ai_budget);
    eprintln!("{dim}{footer} \u{b7} {} iteration{}{r}", run.iters, if run.iters == 1 { "" } else { "s" });
    if code != 0 {
        eprintln!("{dim}  {}{r}", crate::i18n::translate("loop.resume_hint", &[id.clone()]));
    }
    record_session_run(session.as_ref(), "@loop", goal, &digest);
    code
}

/// Which verifier this run uses. An explicit `--check` is the user's word and is taken as
/// given; otherwise — unless they said `--no-check` or turned it off in config — the AI reads
/// the goal once and proposes a command, which the guard still has to allow.
fn choose_verifier(
    spec: &LoopSpec,
    cfg: &crate::config::Config,
    goal: &str,
    policy: &crate::security::Policy,
) -> crate::loops::Verifier {
    if let Some(cmd) = &spec.check {
        return crate::loops::Verifier::Check { command: cmd.clone(), source: crate::loops::Source::Explicit };
    }
    if spec.no_check || !cfg.loop_propose_check {
        return crate::loops::Verifier::Reviewer;
    }
    match crate::ai::verify::propose(goal) {
        // A verifier is supposed to OBSERVE. Anything the guard stops — a deploy, a push —
        // is a command that would change the world to measure it, so it is refused and the
        // reviewer takes over.
        Some(cmd) if guard_refusal(policy, &cmd).is_none() => {
            crate::loops::Verifier::Check { command: cmd, source: crate::loops::Source::Proposed }
        }
        Some(cmd) => {
            eprintln!("{}  the proposed verifier `{cmd}` is not allowed here{}", muted(), reset());
            crate::loops::Verifier::Reviewer
        }
        None => crate::loops::Verifier::Reviewer,
    }
}

/// Trip `token` once `secs` have passed — the wall-clock bound, wired through the same
/// cancellation the user's Ctrl+C uses so an in-flight turn stops promptly. The watcher exits
/// when the returned handle drops.
fn wire_deadline(token: crate::ai::CancelToken, secs: u64) -> SigintWatch {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let done = done.clone();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                if std::time::Instant::now() >= deadline {
                    token.cancel();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
    }
    SigintWatch { done }
}

// ===== background jobs (run + track + monitor from the terminal) =============

/// Detach the CURRENT invocation as a tracked job: the child re-runs this exact argv with
/// its output in the job's log, and stamps the record when it exits. Shared by
/// `@ai --bg`, `@flow --bg` and `@loop --bg`; `@job` has its own record-driven path.
fn spawn_background(args: &[String]) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aiTerminal: can't resolve the binary path: {e}");
            return 1;
        }
    };
    let id = crate::jobs::new_id();
    // The child re-runs `ai` without `--bg`, plus the record marker it stamps on exit.
    let mut child_args: Vec<String> = vec!["ai".into()];
    child_args.extend(args.iter().filter(|a| a.as_str() != "--bg").cloned());
    child_args.push("--job-record".into());
    child_args.push(id.clone());
    let record = crate::jobs::Job {
        id: id.clone(),
        status: "running".into(),
        cmd: args.iter().filter(|a| a.as_str() != "--bg").cloned().collect::<Vec<_>>().join(" "),
        says: String::new(),
        // What actually runs, recorded honestly — `@job show` prints the real command.
        task: crate::jobs::Task::Shell(crate::jobs::Cmd::Argv(
            std::iter::once(exe.display().to_string()).chain(child_args.iter().cloned()).collect(),
        )),
        cwd: cwd_string(),
        started: crate::jobs::now(),
        finished: None,
        exit: None,
        pid: 0,
        schedule: None,
        next_at: None,
        runs: 0,
        last_exit: None,
    };
    let Some((log_path, log)) = crate::jobs::open_run_log(&id, keep_runs()) else {
        eprintln!("aiTerminal: can't create the job log");
        return 1;
    };
    let err = match log.try_clone() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("aiTerminal: can't create the job log: {e}");
            return 1;
        }
    };
    // Detach into its OWN SESSION so closing this terminal never SIGHUPs the job.
    match platform::os::spawn_detached(&exe, &child_args, log, err) {
        Ok(child_pid) => {
            crate::jobs::write(&id, &crate::jobs::Job { pid: child_pid, ..record });
            println!("\u{25B6} background job {id}");
            println!("  monitor: aiTerminal ai job     \u{b7}  tail -f {}", log_path.display());
            0
        }
        Err(e) => {
            eprintln!("aiTerminal: failed to launch the background job: {e}");
            1
        }
    }
}

/// This process's working directory as a string (the folder a job belongs to).
fn cwd_string() -> String {
    std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()
}

fn keep_runs() -> usize {
    crate::config::Config::load().jobs_keep_runs
}

/// Current unix time (seconds).
fn unix_now() -> u64 {
    crate::jobs::now()
}

/// Parse a natural delay / clock phrase out of a request and return the schedule plus the
/// request with that phrase removed — the **fallback** when the AI planner is unavailable,
/// and the reader for the explicit `--in` / `--at` / `--every` flags. Recognizes
/// "in|after <n> sec|min|hour|day(s)" (or a fused `30s`/`2min`), "at HH[:MM][am/pm]", and
/// "every <n> <unit>" / "every hour|day". No match → `(None, request)` (run now).
fn parse_schedule(prompt: &str, now: u64) -> (Option<crate::jobs::Schedule>, String) {
    let words: Vec<&str> = prompt.split_whitespace().collect();
    for i in 0..words.len() {
        let kw = words[i].to_ascii_lowercase();
        if kw == "every" {
            if let Some((secs, used)) = parse_period(&words[i + 1..]) {
                return (Some(crate::jobs::Schedule::Every(secs)), join_excluding(&words, i, i + 1 + used));
            }
        } else if kw == "in" || kw == "after" {
            if let Some((secs, used)) = parse_delay(&words[i + 1..]) {
                return (Some(crate::jobs::Schedule::Once(now + secs)), join_excluding(&words, i, i + 1 + used));
            }
        } else if kw == "at" {
            if let Some(word) = words.get(i + 1) {
                if let Some(fire) = parse_clock_at(word, now) {
                    return (Some(crate::jobs::Schedule::Once(fire)), join_excluding(&words, i, i + 2));
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

/// Run `prompt` through `agent` with the full live chrome; when `log` is set the
/// streamed answer is ALSO written there (the foreground-tracked job's record).
fn run_prompt_as_agent(agent: &str, prompt: &str, mut log: Option<std::fs::File>) -> i32 {
    let (prompt, media, file_ctx) = collect_attachments(prompt);
    let cfg = crate::config::Config::load();
    crate::i18n::install(cfg.i18n_catalog());
    let settings = cfg.ai_settings();
    // Every early exit is written into the run log as well as stderr. A detached job has
    // nobody watching stderr — it lands in `spawn.log`, which no command shows — so a
    // reason that only goes there is a reason nobody ever reads.
    if settings.resolve_key().is_none() {
        return job_setup_error(&mut log, &crate::ai::setup_hint(&settings));
    }
    if build_agent_spec(agent, context_settings(&cfg)).is_none() {
        return job_setup_error(&mut log, &format!("no agent '{agent}' \u{2014} {}", available_agents_hint()));
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


/// Report a job's setup failure to both places that matter, and exit 2.
fn job_setup_error(log: &mut Option<std::fs::File>, reason: &str) -> i32 {
    use std::io::Write;
    eprintln!("aiTerminal: {reason}");
    if let Some(f) = log.as_mut() {
        let _ = writeln!(f, "aiTerminal: {reason}");
    }
    2
}

// ===== @job ==================================================================

/// What an `ai job …` invocation asks for. A pure parse, so the grammar people actually
/// type — a quoted request, loose prose, flags anywhere, `--` for a command — is
/// unit-testable without touching disk or a model.
#[derive(Debug, PartialEq)]
pub(crate) enum JobCmd {
    List,
    Clear,
    Help,
    Cancel(String),
    Log { id: String, follow: bool },
    Show(String),
    /// Create a job from a request.
    Run(Box<RunSpec>),
    /// The detached child: execute one occurrence of an existing record, after an
    /// optional sleep until its fire-time.
    Occurrence { id: String, at: Option<u64> },
}

/// A request to turn into a job.
#[derive(Debug, PartialEq, Default)]
pub(crate) struct RunSpec {
    /// Exactly what the user asked for — kept verbatim for the planner and for display.
    pub(crate) request: String,
    /// Set when `--`/`--shell` made the command explicit; otherwise the planner decides.
    pub(crate) cmd: Option<crate::jobs::Cmd>,
    pub(crate) agent: Option<String>,
    /// Set by the explicit `--every`/`--at`/`--in` flags — these bypass the planner.
    pub(crate) schedule: Option<crate::jobs::Schedule>,
    bg: bool,
    dry_run: bool,
}

/// Read `ai job …` argv.
///
/// The request itself is taken **verbatim when it is a single argument** — so a quoted
/// `@job "write docs for the --bg flag"` keeps its flag-looking words, its spacing and its
/// newlines — and joined with single spaces when it arrives as loose words. After `--`,
/// several words are a command to execute directly (quoting preserved) and one quoted word
/// is a shell line.
pub(crate) fn parse_job_args(args: &[String]) -> JobCmd {
    match args.first().map(String::as_str) {
        None => return JobCmd::List,
        Some("clear") if args.len() == 1 => return JobCmd::Clear,
        Some("help") | Some("--help") | Some("-h") => return JobCmd::Help,
        // `last` by default, like `@job log`, `@flow show` and `@loop show`. These used to
        // default to "", which `record::resolve` matched against every id.
        Some("cancel") | Some("stop") => {
            return JobCmd::Cancel(args.get(1).cloned().unwrap_or_else(|| "last".into()))
        }
        Some("show") => return JobCmd::Show(args.get(1).cloned().unwrap_or_else(|| "last".into())),
        Some("log") | Some("logs") => {
            let follow = args.iter().any(|a| a == "-f" || a == "--follow");
            let id = args.iter().skip(1).find(|a| !a.starts_with('-')).cloned().unwrap_or_else(|| "last".into());
            return JobCmd::Log { id, follow };
        }
        _ => {}
    }
    let mut spec = RunSpec::default();
    let mut words: Vec<String> = Vec::new();
    let (mut record, mut at) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            // Everything after `--` is the command, exactly as the shell handed it over.
            "--" => {
                let rest: Vec<String> = it.by_ref().cloned().collect();
                spec.cmd = Some(match rest.as_slice() {
                    [one] => crate::jobs::Cmd::Line(one.clone()),
                    many => crate::jobs::Cmd::Argv(many.to_vec()),
                });
                break;
            }
            "--shell" => spec.cmd = it.next().cloned().map(crate::jobs::Cmd::Line),
            "--agent" => spec.agent = it.next().cloned(),
            "--every" => spec.schedule = it.next().and_then(|s| every_flag(s)),
            "--cron" => spec.schedule = it.next().and_then(|s| crate::jobs::Cron::parse(s)).map(crate::jobs::Schedule::Cron),
            "--at" => spec.schedule = it.next().and_then(|s| parse_clock_at(s, unix_now())).map(crate::jobs::Schedule::Once),
            "--in" => spec.schedule = it.next().and_then(|s| parse_delay(&[s.as_str()]).map(|(secs, _)| crate::jobs::Schedule::Once(unix_now() + secs))),
            "--bg" => spec.bg = true,
            "--dry-run" | "--plan" => spec.dry_run = true,
            "--run" => record = it.next().cloned(),
            "--run-at" | "--at-unix" => at = it.next().and_then(|s| s.parse().ok()),
            // Kept for records written by an older version, whose children carry these.
            "--job-record" => record = it.next().cloned(),
            w => words.push(w.to_string()),
        }
    }
    if let Some(id) = record {
        return JobCmd::Occurrence { id, at };
    }
    // One argument is the request as typed; several are a sentence to rejoin.
    spec.request = match words.as_slice() {
        [one] => one.clone(),
        many => many.join(" "),
    };
    JobCmd::Run(Box::new(spec))
}

/// `--every 15m` / `--every hour` / `--every 2 hours` → an interval schedule.
fn every_flag(spec: &str) -> Option<crate::jobs::Schedule> {
    let words: Vec<&str> = spec.split_whitespace().collect();
    parse_period(&words).map(|(secs, _)| crate::jobs::Schedule::Every(secs))
}

/// A period after `every`: a counted delay (`15 minutes`, `30m`) **or** a bare unit,
/// because "every hour" means every one hour.
fn parse_period(rest: &[&str]) -> Option<(u64, usize)> {
    parse_delay(rest).or_else(|| rest.first().and_then(|w| unit_secs(w, 1)).map(|s| (s, 1)))
}

/// `@job` — the tracked-task surface. Bare lists; `clear` prunes; `cancel|log|show` operate
/// on one job; anything else is a request to turn into a job. `args` includes the leading
/// "job" word.
fn ai_job_cmd(args: &[String]) -> i32 {
    match parse_job_args(&args[1..]) {
        JobCmd::List => ai_jobs(&[]),
        JobCmd::Clear => ai_jobs(&["clear".to_string()]),
        JobCmd::Help => {
            println!("{}", job_usage());
            0
        }
        JobCmd::Cancel(id) => match crate::jobs::cancel(&id) {
            Ok(msg) => {
                println!("{msg}");
                0
            }
            Err(e) => {
                eprintln!("aiTerminal: {e}");
                2
            }
        },
        JobCmd::Log { id, follow } => job_log(&id, follow),
        JobCmd::Show(id) => job_show(&id),
        JobCmd::Occurrence { id, at } => run_occurrence_child(&id, at),
        JobCmd::Run(spec) => {
            if spec.request.trim().is_empty() && spec.cmd.is_none() {
                eprintln!("{}", job_usage());
                return 2;
            }
            create_job(*spec)
        }
    }
}

// ===== @loop — the surface ===================================================

/// What an `ai loop …` invocation asks for. A pure parse returning a `Result`, because the
/// whole point is that a bound you asked for and a bound you got are the same thing: a
/// misspelled value is an error here, never a silent default.
#[derive(Debug, PartialEq)]
pub(crate) enum LoopCmd {
    List,
    Clear,
    Help,
    Show(String),
    Log { id: String, follow: bool },
    /// Continue a recorded run, optionally with fresh bounds — resuming a run that its
    /// budget stopped is pointless if you cannot raise the budget.
    Resume { id: String, spec: Box<LoopSpec> },
    Run(Box<LoopSpec>),
}

/// A loop to run.
#[derive(Debug, PartialEq, Default)]
pub(crate) struct LoopSpec {
    /// The goal, exactly as typed.
    pub(crate) goal: String,
    /// `--check` — the deterministic verifier.
    pub(crate) check: Option<String>,
    /// `--no-check` — refuse to infer one; grade with the reviewer agent.
    pub(crate) no_check: bool,
    pub(crate) agent: Option<String>,
    /// Bounds left unset fall back to `[loop]` config.
    pub(crate) max: Option<u32>,
    pub(crate) budget: Option<u64>,
    pub(crate) timeout: Option<u64>,
    pub(crate) bg: bool,
    pub(crate) dry_run: bool,
    /// Set on the detached child so it can stamp its job record on exit.
    pub(crate) job_record: Option<String>,
}

/// The value after a flag. Missing — or another flag — is an error: `--budget --bg` means the
/// user believes they set a budget, and running without one would be a lie.
fn flag_value<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<String, String> {
    match it.next() {
        Some(v) if !v.starts_with("--") => Ok(v.clone()),
        _ => Err(format!("{flag} needs a value")),
    }
}

/// Read `ai loop …` argv.
///
/// The goal is taken **verbatim when it is a single argument** — so `@loop "raise --max to 10"`
/// keeps its flag-looking words — and joined with single spaces when it arrives as loose words.
pub(crate) fn parse_loop_args(args: &[String]) -> Result<LoopCmd, String> {
    let one = |i: usize| args.get(i).cloned().unwrap_or_else(|| "last".into());
    match args.first().map(String::as_str) {
        None => return Ok(LoopCmd::List),
        Some("list") if args.len() == 1 => return Ok(LoopCmd::List),
        Some("clear") if args.len() == 1 => return Ok(LoopCmd::Clear),
        Some("help") | Some("--help") | Some("-h") => return Ok(LoopCmd::Help),
        Some("show") => return Ok(LoopCmd::Show(one(1))),
        Some("resume") | Some("continue") => {
            let id = args.iter().skip(1).find(|a| !a.starts_with('-')).cloned().unwrap_or_else(|| "last".into());
            // Everything else is bounds for the continuation.
            let rest: Vec<String> = args[1..].iter().filter(|a| **a != id).cloned().collect();
            let spec = match parse_loop_args(&[vec!["_".to_string()], rest].concat())? {
                LoopCmd::Run(spec) => spec,
                _ => Box::new(LoopSpec::default()),
            };
            return Ok(LoopCmd::Resume { id, spec });
        }
        Some("log") | Some("logs") => {
            let follow = args.iter().any(|a| a == "-f" || a == "--follow");
            let id = args.iter().skip(1).find(|a| !a.starts_with('-')).cloned().unwrap_or_else(|| "last".into());
            return Ok(LoopCmd::Log { id, follow });
        }
        _ => {}
    }
    let mut spec = LoopSpec::default();
    let mut words: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--check" => spec.check = Some(flag_value(&mut it, "--check")?),
            "--no-check" => spec.no_check = true,
            "--agent" => spec.agent = Some(flag_value(&mut it, "--agent")?),
            "--max" => {
                let v = flag_value(&mut it, "--max")?;
                let n: u32 = v.parse().map_err(|_| format!("--max needs a whole number, got {v:?}"))?;
                spec.max = Some(n.clamp(1, 25));
            }
            "--budget" => {
                let v = flag_value(&mut it, "--budget")?;
                spec.budget = Some(v.parse().map_err(|_| format!("--budget needs a token count, got {v:?}"))?);
            }
            "--timeout" => {
                let v = flag_value(&mut it, "--timeout")?;
                let secs = corelib::datetime::duration(&v)
                    .ok_or_else(|| format!("--timeout needs a duration like 30m or 90s, got {v:?}"))?;
                spec.timeout = Some(secs.max(30));
            }
            "--bg" => spec.bg = true,
            "--dry-run" | "--plan" => spec.dry_run = true,
            "--job-record" => spec.job_record = Some(flag_value(&mut it, "--job-record")?),
            w => words.push(w.to_string()),
        }
    }
    if spec.check.is_some() && spec.no_check {
        return Err("--check and --no-check ask for opposite things".into());
    }
    // One argument is the goal as typed; several are a sentence to rejoin.
    spec.goal = match words.as_slice() {
        [only] => only.clone(),
        many => many.join(" "),
    };
    if spec.goal.trim().is_empty() {
        return Err("a loop needs a goal".into());
    }
    Ok(LoopCmd::Run(Box::new(spec)))
}

fn loop_usage() -> String {
    [
        "usage: @loop \"<goal>\"                 iterate until the goal verifies",
        "       @loop … --check \"<cmd>\"        the verifier: exit 0 = done",
        "       @loop … --no-check             grade with a reviewer agent instead",
        "       @loop … --agent <name>         the maker (default coder)",
        "       @loop … --max N --budget TOKENS --timeout 30m",
        "       @loop … --bg | --dry-run       detach it | show the plan only",
        "       @loop                          list recent runs",
        "       @loop show|log|resume <id>     details | output | carry on",
        "       @loop clear                    prune finished runs",
    ]
    .join("\n")
}

/// `ai loop …` — the whole surface. Bare lists; `clear` prunes; `show`/`log`/`resume` operate
/// on one run; anything else is a goal to iterate on. `args` includes the leading "loop" word.
fn ai_loop_cmd(args: &[String]) -> i32 {
    let cmd = match parse_loop_args(&args[1..]) {
        Ok(cmd) => cmd,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            eprintln!("{}", loop_usage());
            return 2;
        }
    };
    match cmd {
        LoopCmd::List => loop_list(),
        LoopCmd::Clear => {
            crate::config::Config::ensure_default();
            crate::i18n::install(crate::config::Config::load().i18n_catalog());
            println!("{}", crate::i18n::translate("loop.cleared", &[crate::loops::clear_finished().to_string()]));
            0
        }
        LoopCmd::Help => {
            println!("{}", loop_usage());
            0
        }
        LoopCmd::Show(id) => loop_show(&id),
        LoopCmd::Log { id, follow } => loop_log(&id, follow),
        LoopCmd::Resume { id, spec } => match crate::loops::resolve(&id) {
            Ok(id) => run_loop_cli(*spec, Some(id)),
            Err(e) => {
                eprintln!("aiTerminal: {e}");
                2
            }
        },
        LoopCmd::Run(spec) => {
            if spec.bg {
                return spawn_background(args);
            }
            let record = spec.job_record.clone();
            let code = run_loop_cli(*spec, None);
            if let Some(id) = record {
                crate::jobs::finish(&id, code);
            }
            code
        }
    }
}

/// `@loop` — the recent runs, newest first.
fn loop_list() -> i32 {
    crate::config::Config::ensure_default();
    let cfg = crate::config::Config::load();
    crate::i18n::install(cfg.i18n_catalog());
    let runs = crate::loops::list();
    if runs.is_empty() {
        println!("{}", crate::i18n::translate("loop.none", &[]));
        return 0;
    }
    let now = crate::loops::now();
    let (dim, r) = (muted(), reset());
    println!("{}", crate::i18n::translate("loop.header", &[runs.len().to_string()]));
    for run in runs {
        let goal = clip_tail(&run.goal, 46);
        let age = crate::loops::human_age(now.saturating_sub(run.finished.unwrap_or(run.started)));
        println!("  {} {} {:<9} {goal}  {dim}({age} ago){r}", run.status_glyph(), run.id, run.status);
        let p = &run.progress;
        let iters = format!("{}/{} iteration(s)", p.iterations, run.bounds.max);
        println!("      {dim}{} \u{b7} {iters}{r}", run.verifier.describe());
    }
    println!("\n{}", crate::i18n::translate("loop.run_hint", &[]));
    0
}

/// One run's full record.
fn loop_show(id: &str) -> i32 {
    let id = match crate::loops::resolve(id) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            return 2;
        }
    };
    let Some(run) = crate::loops::read(&id) else { return 2 };
    let (dim, r) = (muted(), reset());
    println!("{} {} {}", run.status_glyph(), run.id, run.status);
    println!("  {dim}goal{r}      {}", run.goal);
    println!("  {dim}verifier{r}  {}", run.verifier.describe());
    println!("  {dim}maker{r}     @{}", run.agent);
    if !run.cwd.is_empty() {
        println!("  {dim}folder{r}    {}", run.cwd);
    }
    let budget = run.bounds.budget.map(|b| format!(" \u{b7} {b} tokens")).unwrap_or_default();
    println!(
        "  {dim}bounds{r}    {} iteration(s) \u{b7} {}{budget}",
        run.bounds.max,
        crate::loops::human_age(run.bounds.timeout)
    );
    let p = &run.progress;
    println!(
        "  {dim}spent{r}     {} iteration(s) \u{b7} {} tool call(s) \u{b7} {} in / {} out",
        p.iterations, p.tools, p.input_tokens, p.output_tokens
    );
    // Each line already starts with its iteration number, so it needs no second one.
    for line in &p.tried {
        println!("  {dim}tried{r}     {line}");
    }
    if p.escalated {
        println!("  {dim}escalated{r} a different approach was already asked for");
    }
    if let Some(path) = run.latest_log() {
        println!("  {dim}log{r}       {}", path.display());
    }
    0
}

/// One run's newest iteration, optionally followed while it is still live.
fn loop_log(id: &str, follow: bool) -> i32 {
    let id = match crate::loops::resolve(id) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            return 2;
        }
    };
    let Some(run) = crate::loops::read(&id) else { return 2 };
    let Some(path) = run.latest_log() else {
        println!("loop {id} has not finished an iteration yet");
        return 0;
    };
    let alive = || matches!(crate::loops::read(&id), Some(r) if r.is_live());
    tail_log(&path, follow, &alive)
}

/// Print a log, then (with `follow`) keep printing what is appended while `live` holds.
fn tail_log(path: &std::path::Path, follow: bool, live: &dyn Fn() -> bool) -> i32 {
    use std::io::{Read, Seek, Write};
    let Ok(mut f) = std::fs::File::open(path) else {
        eprintln!("aiTerminal: can't read {}", path.display());
        return 1;
    };
    let mut text = String::new();
    let _ = f.read_to_string(&mut text);
    print!("{text}");
    let _ = std::io::stdout().flush();
    if !follow {
        return 0;
    }
    let mut at = f.stream_position().unwrap_or(0);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > at {
                let _ = f.seek(std::io::SeekFrom::Start(at));
                let mut more = String::new();
                let _ = f.read_to_string(&mut more);
                print!("{more}");
                let _ = std::io::stdout().flush();
                at = meta.len();
            }
        }
        if !live() {
            return 0;
        }
    }
}

/// Clip a single line to `max` display columns, ellipsising the middle-end.
fn clip_tail(s: &str, max: usize) -> String {
    let one_line: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if one_line.chars().count() <= max {
        return one_line;
    }
    one_line.chars().take(max.saturating_sub(1)).collect::<String>() + "\u{2026}"
}

fn job_usage() -> String {
    [
        "usage: @job \"<request>\"            what to do, and when — the AI reads it",
        "       @job -- <command>            run a command instead of an agent task",
        "       @job … --bg                  detach it",
        "       @job … --every 15m | --cron \"0 9 * * 1-5\" | --at 17:30 | --in 2m",
        "       @job … --dry-run             show the plan without scheduling it",
        "       @job                         list jobs",
        "       @job log|show|cancel <id>    a job's output, details, or stop it",
        "       @job clear                   prune finished jobs",
    ]
    .join("\n")
}

/// Turn a request into a record, then run it now or leave it armed for its first fire.
fn create_job(spec: RunSpec) -> i32 {
    let now = unix_now();
    let (schedule, task, says) = resolve_spec(&spec, now, &crate::ai::plan::read_request);

    let next_at = schedule.as_ref().and_then(|s| s.next_after(now));
    if spec.dry_run {
        println!("{}{says}{}", accent(), reset());
        if let Some(at) = next_at {
            println!("  first run in {}", crate::jobs::human_age(at.saturating_sub(now)));
        }
        return 0;
    }

    let id = crate::jobs::new_id();
    let scheduled = next_at.is_some();
    let record = crate::jobs::Job {
        id: id.clone(),
        status: if scheduled { "scheduled".into() } else { "running".into() },
        cmd: if spec.request.trim().is_empty() { task_line(&task) } else { spec.request.clone() },
        says,
        task,
        cwd: cwd_string(),
        started: now,
        finished: None,
        exit: None,
        pid: 0,
        schedule,
        next_at,
        runs: 0,
        last_exit: None,
    };
    crate::jobs::write(&id, &record);

    // Waiting for its first fire: arm a sleeper and hand the prompt back.
    if let Some(at) = next_at {
        if !crate::jobs::arm(&id, at) {
            eprintln!("aiTerminal: failed to schedule the job");
            return 1;
        }
        eprintln!("{}\u{29D6} {} \u{b7} job {id}{}", accent(), record.says, reset());
        eprintln!("  fires in {} \u{b7} list: @job \u{b7} cancel: @job cancel {id}", crate::jobs::human_age(at.saturating_sub(now)));
        return 0;
    }
    // Run it now: detached, or right here with the live chrome.
    if spec.bg {
        return match crate::jobs::spawn_occurrence(&id, None) {
            Some(_) => {
                println!("\u{25B6} background job {id}");
                println!("  monitor: @job \u{b7} @job log {id} -f");
                0
            }
            None => {
                eprintln!("aiTerminal: failed to launch the background job");
                1
            }
        };
    }
    execute_occurrence(&id, true)
}

/// Turn a request into *when to run* and *what to run*, in that order of authority:
/// explicit flags, then the planner, then the deterministic word parser.
///
/// The planner is passed in rather than called directly so this precedence can be tested
/// (and driven by a scripted model) without a network — and so `@job` keeps working when
/// `planner` returns `None`, which is exactly what "no model configured" looks like.
pub(crate) fn resolve_spec(
    spec: &RunSpec,
    now: u64,
    planner: &dyn Fn(&str, u64) -> Option<crate::ai::plan::Plan>,
) -> (Option<crate::jobs::Schedule>, crate::jobs::Task, String) {
    // Explicit flags are unambiguous, so they win outright and no model is consulted.
    match (spec.schedule.clone(), spec.cmd.clone()) {
        (sched, Some(cmd)) => {
            let says = describe(&sched, &cmd.display());
            (sched, crate::jobs::Task::Shell(cmd), says)
        }
        (Some(sched), None) => {
            let says = describe(&Some(sched.clone()), &spec.request);
            (Some(sched), agent_task(spec, &spec.request), says)
        }
        // Nothing explicit: let the planner read the sentence, and fall back to the
        // word parser when there is no model (or it answers with nonsense).
        (None, None) => match planner(&spec.request, now) {
            Some(plan) => {
                let task = match plan.cmd {
                    Some(cmd) => crate::jobs::Task::Shell(cmd),
                    None => agent_task(spec, &plan.task),
                };
                (plan.schedule, task, plan.says)
            }
            None => {
                let (sched, cleaned) = parse_schedule(&spec.request, now);
                let says = describe(&sched, &cleaned);
                (sched, agent_task(spec, &cleaned), says)
            }
        },
    }
}

/// The agent task for a request, honoring an explicit `--agent`.
fn agent_task(spec: &RunSpec, text: &str) -> crate::jobs::Task {
    crate::jobs::Task::Agent { text: text.to_string(), agent: spec.agent.clone().unwrap_or_else(|| "coder".into()) }
}

/// A one-line sentence for a plan the planner didn't describe itself.
fn describe(schedule: &Option<crate::jobs::Schedule>, what: &str) -> String {
    match schedule {
        Some(s) => format!("{} \u{2014} {what}", s.describe()),
        None => format!("now \u{2014} {what}"),
    }
}

/// The task as a single display line (used when the request itself was empty).
fn task_line(task: &crate::jobs::Task) -> String {
    match task {
        crate::jobs::Task::Agent { text, .. } => text.clone(),
        crate::jobs::Task::Shell(cmd) => cmd.display(),
    }
}

/// The detached child: optionally sleep until the fire-time (noticing a cancel while it
/// waits), then run exactly one occurrence.
fn run_occurrence_child(id: &str, at: Option<u64>) -> i32 {
    if let Some(at) = at {
        loop {
            let now = unix_now();
            if now >= at {
                break;
            }
            // Cancelled out from under us? Stop without running.
            match crate::jobs::read(id) {
                Some(j) if j.status == "scheduled" => {}
                _ => return 130,
            }
            std::thread::sleep(std::time::Duration::from_secs((at - now).min(2)));
        }
    }
    execute_occurrence(id, false)
}

/// Run one occurrence of a recorded job: open its log, stamp `running`, execute, stamp the
/// outcome (which also advances a repeating schedule to its next fire).
fn execute_occurrence(id: &str, foreground: bool) -> i32 {
    let Some(job) = crate::jobs::read(id) else {
        eprintln!("aiTerminal: no such job '{id}'");
        return 2;
    };
    let opened = crate::jobs::open_run_log(id, keep_runs());
    // A run always writes down that it happened, before and after.
    //
    // An EMPTY log reads as "nothing went wrong", which is the exact opposite of the truth
    // when a run died before it produced a line — and that is the common case, because
    // "no model configured" is decided before any agent starts. The header and footer are
    // written here, around every task kind, so no failure path can leave a silent log.
    let mut note = opened.as_ref().and_then(|(_, f)| f.try_clone().ok());
    run_log_header(&mut note, &job);
    let log = opened.map(|(_, f)| f);
    crate::jobs::mark_running(id, std::process::id());
    let code = match &job.task {
        crate::jobs::Task::Agent { text, agent } => run_prompt_as_agent(agent, text, log),
        crate::jobs::Task::Shell(cmd) => run_shell_job(cmd, &job.cwd, log, foreground),
    };
    run_log_footer(&mut note, code);
    crate::jobs::finish(id, code);
    code
}

/// Open a run log with what is about to happen.
fn run_log_header(log: &mut Option<std::fs::File>, job: &crate::jobs::Job) {
    use std::io::Write;
    let Some(f) = log.as_mut() else { return };
    let what = match &job.task {
        crate::jobs::Task::Agent { agent, text } => format!("@{agent} {text}"),
        crate::jobs::Task::Shell(cmd) => cmd.display(),
    };
    let when =
        corelib::datetime::format(crate::jobs::now() as i64, "%Y-%m-%d %H:%M", platform::os::utc_offset_secs());
    let _ = writeln!(f, "# {what}\n# in {} at {when}\n", job.cwd);
}

/// Close it with the outcome, so `@job log` always ends with the answer to "did it work".
fn run_log_footer(log: &mut Option<std::fs::File>, code: i32) {
    use std::io::Write;
    let Some(f) = log.as_mut() else { return };
    let verdict = match code {
        0 => "\u{2713} done".to_string(),
        2 => "\u{2717} setup error (exit 2) \u{2014} see the reason above".to_string(),
        130 => "\u{23f9} cancelled".to_string(),
        n => format!("\u{2717} failed (exit {n})"),
    };
    let _ = writeln!(f, "\n{verdict}");
}

/// Why a job's command must not run, or `None` when it may.
///
/// A job has nobody to answer a prompt, so "ask first" is a refusal here — which is also the
/// check that matters most for a command the *model* proposed.
pub(crate) fn guard_refusal(policy: &crate::security::Policy, line: &str) -> Option<String> {
    match policy.check_command(line) {
        crate::security::Verdict::Allow => None,
        crate::security::Verdict::Deny { reason } => Some(format!("blocked by the command guard: {reason}")),
        crate::security::Verdict::Confirm { reason } => {
            Some(format!("needs confirmation ({reason}) \u{2014} a job can't ask, so it was not run"))
        }
    }
}

/// Run a job's command: guard-checked, in the job's folder, output streamed to its log
/// (and to this terminal when the job is in the foreground).
fn run_shell_job(cmd: &crate::jobs::Cmd, cwd: &str, log: Option<std::fs::File>, foreground: bool) -> i32 {
    use std::io::Write;
    let line = cmd.display();
    let cfg = crate::config::Config::load();
    let registry = crate::plugin::load_registry(&cfg);
    let policy = crate::security::build_policy(&cfg, &registry);
    let refusal = guard_refusal(&policy, &line);
    let mut sink = Sink { log, echo: foreground, written: 0, cap: cfg.jobs_max_log_bytes };
    if let Some(reason) = refusal {
        sink.write_line(&format!("aiTerminal: {reason}"));
        // The sink already echoed it when this job is in the foreground; a detached one has
        // only its log, so say it on stderr too.
        if !foreground {
            eprintln!("aiTerminal: {reason}");
        }
        return 2;
    }
    sink.write_line(&format!("$ {line}"));

    let mut command = match cmd {
        crate::jobs::Cmd::Line(l) => {
            let mut c = std::process::Command::new("/bin/sh");
            c.arg("-c").arg(l);
            c
        }
        crate::jobs::Cmd::Argv(argv) => {
            let mut c = std::process::Command::new(&argv[0]);
            c.args(&argv[1..]);
            c
        }
    };
    if !cwd.is_empty() && std::path::Path::new(cwd).is_dir() {
        command.current_dir(cwd);
    }
    let mut child = match command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("aiTerminal: {line}: {e}");
            sink.write_line(&msg);
            eprintln!("{msg}");
            return 127;
        }
    };
    // Drain both pipes on threads so a chatty command can't dead-lock on a full pipe.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    for stream in [child.stdout.take().map(Pipe::Out), child.stderr.take().map(Pipe::Err)].into_iter().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 8192];
            let mut reader: Box<dyn Read + Send> = match stream {
                Pipe::Out(o) => Box::new(o),
                Pipe::Err(e) => Box::new(e),
            };
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(String::from_utf8_lossy(&buf[..n]).into_owned()).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    for chunk in rx {
        sink.write(&chunk);
    }
    let status = child.wait();
    let code = match status {
        Ok(st) => st.code().unwrap_or(130),
        Err(e) => {
            sink.write_line(&format!("aiTerminal: {e}"));
            1
        }
    };
    sink.write_line(&format!("\n[exit {code}]"));
    let _ = std::io::stdout().flush();
    code
}

/// Which pipe a drained chunk came from (both go to the same place).
enum Pipe {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

/// Where a shell job's output goes: the run log (size-capped) and, in the foreground, the
/// terminal. A job that prints forever costs a bounded log instead of the disk.
struct Sink {
    log: Option<std::fs::File>,
    echo: bool,
    written: u64,
    cap: u64,
}

impl Sink {
    fn write(&mut self, text: &str) {
        use std::io::Write;
        if self.echo {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        let Some(log) = self.log.as_mut() else { return };
        if self.written >= self.cap {
            return;
        }
        let room = (self.cap - self.written) as usize;
        let slice = if text.len() > room { &text[..text.floor_char_boundary(room)] } else { text };
        if log.write_all(slice.as_bytes()).is_ok() {
            self.written += slice.len() as u64;
            if self.written >= self.cap {
                let _ = log.write_all(b"\n[log truncated]\n");
            }
        }
    }

    fn write_line(&mut self, text: &str) {
        self.write(text);
        self.write("\n");
    }
}

/// `@job log <id> [-f]` — print the newest run's log, optionally following it.
fn job_log(id: &str, follow: bool) -> i32 {
    let id = match crate::jobs::resolve(id) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            return 2;
        }
    };
    let Some(job) = crate::jobs::read(&id) else { return 2 };
    let Some(path) = job.latest_log() else {
        println!("job {id} has not run yet");
        return 0;
    };
    // A log that exists but is empty used to print nothing at all — which reads as "it went
    // fine and said nothing", the opposite of the truth. Say so, and point at the one place
    // a message could still be hiding.
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) == 0 {
        let (dim, r) = (muted(), reset());
        println!("job {id} ran but wrote nothing to its log");
        if let Some(spawn) = crate::jobs::dir(&id).map(|d| d.join("spawn.log")) {
            if std::fs::metadata(&spawn).map(|m| m.len()).unwrap_or(0) > 0 {
                println!("{dim}anything it printed before starting is in {}{r}", spawn.display());
            }
        }
        return 0;
    }
    use std::io::{Read, Seek, Write};
    let Ok(mut f) = std::fs::File::open(&path) else {
        eprintln!("aiTerminal: can't read {}", path.display());
        return 1;
    };
    let mut text = String::new();
    let _ = f.read_to_string(&mut text);
    print!("{text}");
    let _ = std::io::stdout().flush();
    if !follow {
        return 0;
    }
    // Follow: poll for growth while the job is still live, so `-f` ends by itself.
    let mut at = f.stream_position().unwrap_or(0);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > at {
                let _ = f.seek(std::io::SeekFrom::Start(at));
                let mut more = String::new();
                let _ = f.read_to_string(&mut more);
                print!("{more}");
                let _ = std::io::stdout().flush();
                at = meta.len();
            }
        }
        match crate::jobs::read(&id) {
            Some(j) if j.status == "running" => {}
            _ => return 0,
        }
    }
}

/// `@job show <id>` — everything the record knows, in the order a person asks it.
fn job_show(id: &str) -> i32 {
    let id = match crate::jobs::resolve(id) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("aiTerminal: {e}");
            return 2;
        }
    };
    let Some(job) = crate::jobs::read(&id) else { return 2 };
    let now = unix_now();
    let (dim, r) = (muted(), reset());
    println!("{} {} {}", job.status_glyph(), job.id, job.status);
    println!("  {dim}request{r}  {}", job.cmd);
    if !job.says.is_empty() {
        println!("  {dim}plan{r}     {}", job.says);
    }
    match &job.task {
        crate::jobs::Task::Agent { agent, text } => {
            println!("  {dim}task{r}     agent {agent}: {text}");
        }
        crate::jobs::Task::Shell(cmd) => println!("  {dim}command{r}  {}", cmd.display()),
    }
    if !job.cwd.is_empty() {
        println!("  {dim}folder{r}   {}", job.cwd);
    }
    if let Some(s) = &job.schedule {
        println!("  {dim}schedule{r} {}", s.describe());
    }
    if let Some(at) = job.next_at.filter(|_| job.status == "scheduled") {
        println!("  {dim}next{r}     in {}", crate::jobs::human_age(at.saturating_sub(now)));
    }
    if job.runs > 0 {
        let last = job.last_exit.map(|c| if c == 0 { "ok".to_string() } else { format!("exit {c}") }).unwrap_or_default();
        println!("  {dim}runs{r}     {} \u{b7} last {last}", job.runs);
    }
    // Why it failed, not just that it did. `exit 2` on its own sends people to read a log
    // they have to be told exists; the reason is one line and it belongs here.
    if let Some(reason) = job.latest_log().and_then(|p| failure_reason(&p)) {
        println!("  {dim}reason{r}   {reason}");
    }
    if let Some(p) = job.latest_log() {
        println!("  {dim}log{r}      {}", p.display());
    }
    0
}

/// The first real complaint in a run log — the `aiTerminal: …` line a failing run writes.
///
/// Deliberately narrow: it looks for the line the run itself emitted about why it stopped,
/// not for anything that merely resembles an error in a model's prose.
fn failure_reason(log: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(log).ok()?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("aiTerminal: "))
        .map(|l| clip_tail(l, 72))
}

/// `aiTerminal ai job [clear]` — list jobs (newest first), or prune the finished ones.
fn ai_jobs(args: &[String]) -> i32 {
    crate::config::Config::ensure_default();
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    if args.first().map(String::as_str) == Some("clear") {
        let n = crate::jobs::clear_finished();
        println!("{}", crate::i18n::translate("job.cleared", &[n.to_string()]));
        return 0;
    }
    // Listing is also when a CLI-only user's due work gets picked up.
    crate::jobs::reconcile();
    let list = crate::jobs::list();
    if list.is_empty() {
        println!("{}", crate::i18n::translate("job.none", &[]));
        return 0;
    }
    println!("{}", crate::i18n::translate("job.header", &[list.len().to_string()]));
    let now = unix_now();
    let (dim, r) = (muted(), reset());
    for j in &list {
        // One glanceable line per job: glyph · id · status · what it is · timing.
        let what = if j.cmd.chars().count() > 44 {
            format!("{}\u{2026}", j.cmd.chars().take(43).collect::<String>())
        } else {
            j.cmd.clone()
        };
        println!("  {} {:<12} {:<9} {}  {dim}({}){r}", j.status_glyph(), j.id, j.status, what, j.timing(now));
        // A repeating job's second line is its schedule and how the last run went.
        let mut notes: Vec<String> = Vec::new();
        if let Some(s) = &j.schedule {
            if s.repeats() {
                notes.push(s.describe());
            }
        }
        if j.runs > 0 {
            notes.push(format!("{} run(s)", j.runs));
            if let Some(c) = j.last_exit {
                notes.push(if c == 0 { "last ok".into() } else { format!("last exit {c}") });
            }
        }
        if !notes.is_empty() {
            println!("      {dim}{}{r}", notes.join(" \u{b7} "));
        }
    }
    0
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
            // Both sources, because both are running. The bundled plugins load from the
            // registry root and the installed ones from `~/.aiTerminal/plugins/` — listing
            // only the second printed "(none)" on a fresh machine while thirty-one were
            // active, which reads as "you have no plugins".
            let cfg = crate::config::Config::load();
            let registry = crate::plugin::load_registry(&cfg);
            let installed = store.installed();
            let names: Vec<String> = installed.iter().map(|p| p.name.clone()).collect();
            let bundled: Vec<(String, String, String, bool)> =
                registry.loaded().into_iter().filter(|(n, _, _, _)| !names.contains(n)).collect();
            println!("plugins ({} bundled · {} installed):", bundled.len(), installed.len());
            for (name, version, description, _) in &bundled {
                println!("  \u{25CF} {name:<18} {version:<8} {description}");
            }
            for p in &installed {
                let mark = if p.enabled { "\u{25CF}" } else { "\u{25CB}" };
                println!("  {mark} {:<18} {:<8} {}  (installed)", p.name, p.version, p.description);
            }
            let (dim, r) = (muted(), reset());
            println!("\n{dim}bundled plugins live in the app; yours go in {}{r}", crate::config::Config::plugins_dir().display());
            println!("{dim}one in full:  @plugin info <name>   \u{b7}  turn one off:  @plugin disable <name>{r}");
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
            // Installed first (yours shadows a bundled one of the same name), then the
            // bundled set — `info git` used to say "not installed" about a plugin that was
            // loaded and working.
            Some(name) => match store.installed().into_iter().find(|p| &p.name == name) {
                Some(p) => {
                    println!("{}  v{}\n{}\ninstalled \u{b7} enabled: {}", p.name, p.version, p.description, p.enabled);
                    0
                }
                None => {
                    let cfg = crate::config::Config::load();
                    let registry = crate::plugin::load_registry(&cfg);
                    match registry.loaded().into_iter().find(|(n, _, _, _)| n == name) {
                        Some((n, v, d, _)) => {
                            println!("{n}  v{v}\n{d}\nbundled with the app \u{b7} enabled: {}", store.is_enabled(&n));
                            0
                        }
                        None => {
                            let all: Vec<String> = registry.names();
                            let refs: Vec<&str> = all.iter().map(String::as_str).collect();
                            eprintln!("no plugin '{name}'{}", crate::flow::verify::nearest(name, &refs));
                            1
                        }
                    }
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
    use super::{clamp_tail, command_marker, erase_seq, error_comment, fnv1a, is_open_diagram_fence, loop_prompt, reviewer_passed, session_lines, tail, CONFIRM_MARK, EDIT_MARK, RUN_MARK};
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
    fn every_flow_subcommand_is_told_from_a_flow_name() {
        use super::{FlowCmd, parse_flow_args};
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let parse = |xs: &[&str]| parse_flow_args(&a(xs)).expect("parses");
        assert_eq!(parse(&[]), FlowCmd::List);
        assert_eq!(parse(&["list"]), FlowCmd::List);
        assert_eq!(parse(&["check"]), FlowCmd::Check(None), "no name checks them all");
        assert_eq!(parse(&["check", "implement"]), FlowCmd::Check(Some("implement".into())));
        assert_eq!(parse(&["graph", "implement"]), FlowCmd::Graph("implement".into()));
        assert_eq!(parse(&["runs"]), FlowCmd::Runs);
        assert_eq!(parse(&["clear"]), FlowCmd::Clear);
        assert_eq!(parse(&["show"]), FlowCmd::Show("last".into()), "an id defaults to the newest");
        assert_eq!(parse(&["show", "1700-1"]), FlowCmd::Show("1700-1".into()));
        assert_eq!(parse(&["resume", "1700-1"]), FlowCmd::Resume("1700-1".into()));
        assert_eq!(
            parse(&["log", "1700-1", "verify", "-f"]),
            FlowCmd::Log { id: "1700-1".into(), node: Some("verify".into()), follow: true }
        );
        assert_eq!(parse(&["log"]), FlowCmd::Log { id: "last".into(), node: None, follow: false });
        // `graph` with nothing to draw is an error, not a guess.
        assert!(parse_flow_args(&a(&["graph"])).is_err());
    }

    #[test]
    fn a_quoted_input_arrives_verbatim_and_loose_words_rejoin() {
        use super::{FlowCmd, parse_flow_args};
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let run = |xs: &[&str]| match parse_flow_args(&a(xs)).expect("parses") {
            FlowCmd::Run(spec) => *spec,
            other => panic!("expected a run, got {other:?}"),
        };
        // One argument is the input exactly as typed — so a flag-looking word inside
        // the quotes stays text instead of being eaten.
        let spec = run(&["ship", "raise --max to 10"]);
        assert_eq!((spec.name.as_str(), spec.input.as_str()), ("ship", "raise --max to 10"));
        // Loose words become a sentence.
        let spec = run(&["ship", "add", "a", "flag"]);
        assert_eq!(spec.input, "add a flag");
        // Flags are read wherever they appear, and never land in the input.
        let spec = run(&["ship", "--bg", "add", "a", "flag", "--concurrency", "2"]);
        assert!(spec.bg && spec.concurrency == Some(2));
        assert_eq!(spec.input, "add a flag", "--bg used to end up inside the prompt text");
    }

    #[test]
    fn one_argument_with_a_space_in_it_is_a_goal_not_a_name() {
        use super::{FlowCmd, parse_flow_args};
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let run = |xs: &[&str]| match parse_flow_args(&a(xs)).expect("parses") {
            FlowCmd::Run(spec) => *spec,
            other => panic!("expected a run, got {other:?}"),
        };
        // No flow can be called this, so it is a goal for the model to route.
        let spec = run(&["Build a SaaS landing page end to end"]);
        assert_eq!(spec.name, "", "no flow was named");
        assert_eq!(spec.input, "Build a SaaS landing page end to end");
        // Flags still work around it.
        let spec = run(&["Research LLM memory techniques", "--dry-run"]);
        assert!(spec.dry_run && spec.name.is_empty());
        // A single word is still a flow name, and loose words are still an error case
        // resolved by name — the typo footgun does not return through this door.
        assert_eq!(run(&["build"]).name, "build");
        assert_eq!(run(&["revieew", "the", "parser"]).name, "revieew");
    }

    #[test]
    fn a_bound_you_asked_for_and_a_bound_you_got_are_the_same_thing() {
        use super::parse_flow_args;
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // A value that cannot be read is an error naming the flag — never a silent
        // default, which would run the flow with a bound the user did not choose.
        for (args, want) in [
            (vec!["f", "--budget", "abc"], "--budget"),
            (vec!["f", "--budget"], "--budget needs a value"),
            (vec!["f", "--timeout", "soon"], "--timeout"),
            (vec!["f", "--concurrency", "lots"], "--concurrency"),
            (vec!["f", "--timeout", "--bg"], "--timeout needs a value"),
        ] {
            let err = parse_flow_args(&a(&args)).map(|_| ()).expect_err(&format!("{args:?} must not parse"));
            assert!(err.contains(want), "{args:?} said {err:?}");
        }
        assert!(parse_flow_args(&a(&[])).is_ok());
        // And a name is required: `@flow --bg` alone asks for nothing.
        assert!(parse_flow_args(&a(&["--bg"])).is_err());
    }

    #[test]
    fn the_shipped_example_flow_is_a_valid_graph() {
        // The examples are what people copy, so they are held to the live schema.
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
        let text = std::fs::read_to_string(format!("{root}/ai/flow.toml")).unwrap();
        let flow = crate::flow::parse("ship", &text).expect("examples/ai/flow.toml parses");
        assert!(flow.nodes.len() >= 3);
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
        let p = loop_prompt("make tests pass", 3, 8, Some("cargo test"), "assertion failed: left == right", &[], false);
        assert!(p.contains("iteration 3 of at most 8"));
        assert!(p.contains("exits 0: `cargo test`"));
        assert!(p.contains("assertion failed"), "verifier feedback is fed forward");
        // First iteration: no feedback section, nothing tried yet, no escalation.
        let first = loop_prompt("goal", 1, 5, None, "", &[], false);
        assert!(!first.contains("Verifier feedback"));
        assert!(!first.contains("Already attempted"));
        assert!(!first.contains("MATERIALLY DIFFERENT"));
    }

    #[test]
    fn loop_prompt_carries_the_attempt_log_and_the_strategy_shift() {
        let tried: Vec<String> = (1..=9).map(|i| format!("{i}: tried thing {i} \u{2192} still failing")).collect();
        let p = loop_prompt("goal", 10, 12, None, "same failure", &tried, true);
        // The log rides along, but only the recent tail of it — the rest would be transcript.
        assert!(p.contains("Already attempted"));
        assert!(p.contains("tried thing 9"), "the newest attempt is there");
        assert!(!p.contains("tried thing 1 "), "the oldest attempts are dropped");
        // The escalation says, in the prompt, that refining the same approach will not work.
        assert!(p.contains("MATERIALLY DIFFERENT"));
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
        crate::ai::AgentSpec { system: "You fix things.".into(), tools: Vec::new(), max_steps: 3, ..Default::default() }
    }

    /// A scripted client with one canned answer per expected iteration.
    fn scripted(answers: &[&str]) -> crate::ai::Client<crate::ai::ScriptedTransport> {
        let fixtures = answers.iter().map(|a| crate::ai::text_sse(a, 10, 4)).collect();
        crate::ai::Client::new(keyed_settings(), crate::ai::ScriptedTransport::new(fixtures))
    }

    fn verdict(passed: bool, feedback: &str) -> super::Verdict {
        super::Verdict { passed, feedback: feedback.into(), raw: feedback.into(), signature: fnv1a(feedback), code: None }
    }

    /// Fresh loop state with the given bounds and no history.
    fn state(max: u32, budget: Option<u64>) -> super::LoopState {
        super::LoopState {
            left: crate::loops::Bounds { max, budget, timeout: 3600 },
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(3600)),
            ..Default::default()
        }
    }

    /// Drive a loop over a scripted verifier, returning the outcome and the final state.
    fn drive(
        answers: &[&str],
        st: &mut super::LoopState,
        verify: impl FnMut(&str) -> Result<super::Verdict, String>,
    ) -> super::LoopOutcome {
        let client = scripted(answers);
        super::drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", st, None, verify).outcome
    }

    #[test]
    fn loop_passes_when_the_verifier_passes_and_feeds_feedback_forward() {
        let client = scripted(&["attempt one", "attempt two"]);
        let mut iterations = 0;
        let mut st = state(5, None);
        let outcome = super::drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "fix it", &mut st, Some("cargo test"), |answer| {
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
        assert_eq!(st.tried.len(), 2, "both attempts are written into the state");
        assert!(st.tried[0].starts_with("1: attempt one"), "{:?}", st.tried);
    }

    #[test]
    fn loop_escalates_once_on_no_progress_then_stalls() {
        // The same failure forever. The FIRST repeat buys one strategy shift; the next
        // one ends the run — a stuck loop must not be able to spend the whole cap.
        let mut st = state(10, None);
        let mut n = 0;
        let outcome = drive(&["a", "b", "c", "d"], &mut st, |_| {
            n += 1;
            Ok(verdict(false, "exit=1 same failure"))
        });
        assert_eq!(outcome, super::LoopOutcome::Stalled);
        assert_eq!(n, 3, "iteration 2 repeats → escalate; iteration 3 repeats → stop");
        assert!(st.escalated, "the one escalation was spent");
    }

    #[test]
    fn loop_catches_an_oscillation_not_just_a_repeat() {
        // A → B → A. Nothing is ever identical to the PREVIOUS observation, so a
        // "same as last time?" test would run to the cap; this is still no progress.
        let mut st = state(10, None);
        let mut n = 0;
        let outcome = drive(&["a", "b", "c", "d", "e", "f"], &mut st, |_| {
            n += 1;
            Ok(verdict(false, if n % 2 == 1 { "failure A" } else { "failure B" }))
        });
        assert_eq!(outcome, super::LoopOutcome::Stalled);
        assert!(n < 6, "stopped well before the cap, after {n} iterations");
    }

    #[test]
    fn loop_exhausts_at_the_iteration_cap() {
        let mut st = state(3, None);
        let mut n = 0;
        let outcome = drive(&["a", "b", "c"], &mut st, |_| {
            n += 1;
            Ok(verdict(false, &format!("different failure {n}"))) // always progressing
        });
        assert_eq!(outcome, super::LoopOutcome::Exhausted);
        assert_eq!(n, 3, "ran exactly --max iterations");
    }

    #[test]
    fn loop_stops_at_the_token_budget() {
        // Each scripted turn reports 10 in + 4 out tokens; budget 1 → stop after
        // the first (still-failing) iteration.
        let mut st = state(10, Some(1));
        let outcome = drive(&["a", "b"], &mut st, |_| Ok(verdict(false, "still failing")));
        assert_eq!(outcome, super::LoopOutcome::Budget);
    }

    #[test]
    fn loop_stops_when_the_clock_runs_out() {
        // A deadline already in the past: the run stops before starting an iteration, so a
        // slow agent can't outlive its wall clock however few iterations it has used.
        let mut st = super::LoopState {
            left: crate::loops::Bounds { max: 10, budget: None, timeout: 1 },
            deadline: Some(std::time::Instant::now()),
            ..Default::default()
        };
        let outcome = drive(&["a"], &mut st, |_| Ok(verdict(false, "x")));
        assert_eq!(outcome, super::LoopOutcome::Timeout);
    }

    #[test]
    fn loop_surfaces_a_verifier_error() {
        // A check command the guard refuses aborts the loop as a setup error.
        let mut st = state(5, None);
        let outcome = drive(&["a"], &mut st, |_| Err("check command blocked by guard: deploy-prod".into()));
        assert_eq!(outcome, super::LoopOutcome::Error("check command blocked by guard: deploy-prod".into()));
    }

    #[test]
    fn a_resumed_loop_continues_where_it_stopped() {
        // Two iterations already done, three of five left: numbering carries on and the run
        // reports only the NEW iterations, so a resume never re-bills the old ones.
        let mut st = super::LoopState {
            done: 2,
            left: crate::loops::Bounds { max: 3, budget: None, timeout: 3600 },
            feedback: "exit=1 the old failure".into(),
            tried: vec!["1: first".into(), "2: second".into()],
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(3600)),
            ..Default::default()
        };
        let mut seen = Vec::new();
        let client = scripted(&["third", "fourth", "fifth"]);
        let run = super::drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", &mut st, None, |a| {
            seen.push(a.to_string());
            Ok(verdict(false, &format!("failure {}", seen.len())))
        });
        assert_eq!(run.outcome, super::LoopOutcome::Exhausted);
        assert_eq!(run.iters, 3, "three NEW iterations, not five");
        assert_eq!(st.tried.len(), 5, "the attempt log grew from two to five");
        assert!(st.tried[2].starts_with("3: third"), "numbering continues: {:?}", st.tried);
    }

    #[test]
    fn loop_flags_are_read_strictly_or_refused() {
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let spec = |args: &[&str]| match super::parse_loop_args(&a(args)) {
            Ok(super::LoopCmd::Run(spec)) => *spec,
            other => panic!("{other:?}"),
        };
        // A goal, taken verbatim when it is one argument — flag-looking words inside stay text.
        let s = spec(&["raise --max to 10"]);
        assert_eq!(s.goal, "raise --max to 10");
        assert_eq!(s.max, None);
        // Loose words rejoin into a sentence.
        assert_eq!(spec(&["make", "the", "tests", "pass"]).goal, "make the tests pass");
        // Bounds are read.
        let s = spec(&["goal", "--max", "8", "--budget", "50000", "--timeout", "30m"]);
        assert_eq!((s.max, s.budget, s.timeout), (Some(8), Some(50_000), Some(1800)));
        // A value that cannot be read is an ERROR — never a silent default, because a bound
        // you asked for and did not get is worse than no bound at all.
        for bad in [
            vec!["goal", "--budget", "abc"],
            vec!["goal", "--max", "lots"],
            vec!["goal", "--timeout", "soon"],
            vec!["goal", "--budget"],          // no value at all
            vec!["goal", "--check"],           // …would have silently self-graded
            vec!["goal", "--check", "--bg"],   // the next flag is not a value
            vec!["--max", "3"],                // no goal
            vec!["goal", "--check", "x", "--no-check"], // contradictory
        ] {
            assert!(super::parse_loop_args(&a(&bad)).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn loop_subcommands_parse() {
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let p = |xs: &[&str]| super::parse_loop_args(&a(xs)).unwrap();
        assert_eq!(p(&[]), super::LoopCmd::List);
        assert_eq!(p(&["clear"]), super::LoopCmd::Clear);
        assert_eq!(p(&["show", "4310"]), super::LoopCmd::Show("4310".into()));
        let super::LoopCmd::Resume { id, spec } = p(&["resume", "last", "--budget", "200000"]) else {
            panic!("resume should parse")
        };
        assert_eq!(id, "last");
        assert_eq!(spec.budget, Some(200_000), "a resume can be given more rope");
        assert_eq!(p(&["log", "-f"]), super::LoopCmd::Log { id: "last".into(), follow: true });
        // A bare id defaults to the newest run, so `@loop show` alone means "the last one".
        assert_eq!(p(&["show"]), super::LoopCmd::Show("last".into()));
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
            ..Default::default()
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
        let spec = super::build_agent_spec("coder", (0, crate::ai::DEFAULT_COMPACT_AT)).expect("bundled coder agent");
        assert!(spec.system.starts_with("Always answer in haiku."), "instructions lead the system prompt");
        assert!(super::instructions_preamble().contains("Always answer in haiku."));
        assert!(super::instructions_preamble().contains("aiTerminal.md"), "the preamble names its source");
        std::fs::write(crate::config::Config::instructions_path(), "   ").unwrap();
        assert!(super::instructions_preamble().is_empty(), "blank file → no preamble");
        let spec = super::build_agent_spec("coder", (0, crate::ai::DEFAULT_COMPACT_AT)).unwrap();
        assert!(!spec.system.starts_with("##"), "blank instructions add nothing");
    }

    #[test]
    fn two_runs_never_share_a_scratch_directory() {
        // `record::new_id()` is `<unix-secs>-<pid>`, so four @flow nodes starting in
        // the same second inside one process get the same id — and offloaded files are
        // named by turn index, so two nodes would each write `003-fs-read.txt` into
        // the same directory and one would read back the other's output.
        let dirs: std::collections::HashSet<_> = (0..64).map(|_| super::run_scratch()).collect();
        assert_eq!(dirs.len(), 64, "64 runs, 64 directories");
    }

    #[test]
    fn grounding_is_trimmed_from_the_least_valuable_end() {
        // On a small-window model the preamble could otherwise crowd out the question
        // it exists to ground. What goes first is the part that grows on its own and
        // nobody asked for; what survives is what the user actually said.
        let big = |n: usize| "x ".repeat(n);
        let blocks = |n: usize| {
            [
                ("instructions", "## Instructions\nAlways answer in haiku.".to_string()),
                ("attachments", "## Attached\nthe file they picked".to_string()),
                ("memory", big(n)),
                ("session", big(n)),
                ("terminal", big(n)),
            ]
        };

        // A large window keeps everything.
        let roomy = super::fit_context(&crate::ai::ContextBudget::new(200_000, 4_096, 0.75), "why?", &blocks(200));
        for want in ["haiku", "the file they picked"] {
            assert!(roomy.contains(want), "kept with room to spare: {want}");
        }

        // A small one drops terminal first, then session, then memory — and never the
        // instructions or the user's own attachment.
        let tight = super::fit_context(&crate::ai::ContextBudget::new(8_192, 7_000, 0.75), "why?", &blocks(4_000));
        assert!(tight.contains("haiku"), "standing instructions survive: {tight:?}");
        assert!(tight.contains("the file they picked"), "an explicit attachment survives: {tight:?}");
        assert!(tight.len() < 4_000, "the bulky blocks went: {}", tight.len());

        // Blocks go WHOLE — half a digest is a misleading digest.
        let one = super::fit_context(
            &crate::ai::ContextBudget::new(8_192, 7_000, 0.75),
            "why?",
            &[("session", "## Session\ncomplete or absent".to_string())],
        );
        assert!(one.is_empty() || one.contains("complete or absent"), "never half a block: {one:?}");

        // Empty blocks are not counted, and nothing is fabricated.
        let none = super::fit_context(
            &crate::ai::ContextBudget::new(200_000, 4_096, 0.75),
            "why?",
            &[("session", String::new()), ("terminal", "   ".into())],
        );
        assert!(none.is_empty(), "no grounding means no preamble: {none:?}");
    }

    #[test]
    fn job_grammar_parses_the_intuitive_form() {
        use super::JobCmd;
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let run = |args: &[&str]| match super::parse_job_args(&a(args)) {
            JobCmd::Run(spec) => *spec,
            other => panic!("expected a run, got {other:?}"),
        };
        // The shape people type: free text with optional flags anywhere on the line.
        let spec = run(&["create", "a", "file", "called", "hamid.txt", "in", "one", "minute", "--bg", "--agent", "tester"]);
        assert_eq!(spec.request, "create a file called hamid.txt in one minute");
        assert_eq!(spec.agent.as_deref(), Some("tester"));
        assert!(spec.bg);
        // Defaults: no agent named (the planner or `coder`), foreground, no schedule flag.
        let spec = run(&["build", "the", "docs"]);
        assert_eq!(spec.request, "build the docs");
        assert_eq!(spec.agent, None);
        assert!(!spec.bg && spec.schedule.is_none() && !spec.dry_run);
        // `--dry-run` asks for the plan without creating anything.
        assert!(run(&["check the logs at midnight", "--dry-run"]).dry_run);
    }

    #[test]
    fn parse_schedule_reads_natural_time() {
        use crate::jobs::Schedule;
        // The fallback parser (used when no model is configured, and by --in/--at/--every):
        // "in N unit" fires N later, with the phrase stripped from the request.
        let (at, cleaned) = super::parse_schedule("create a file named hamid in 2 minutes", 1_000);
        assert_eq!(at, Some(Schedule::Once(1_120)));
        assert_eq!(cleaned, "create a file named hamid");
        // Fused unit + "after".
        assert_eq!(super::parse_schedule("build after 30s", 0).0, Some(Schedule::Once(30)));
        assert_eq!(super::parse_schedule("build in 1 hour", 0).0, Some(Schedule::Once(3600)));
        // "every …" repeats, and the words leave the task behind.
        let (every, cleaned) = super::parse_schedule("summarize the kafka logs every hour", 0);
        assert_eq!(every, Some(Schedule::Every(3600)));
        assert_eq!(cleaned, "summarize the kafka logs");
        assert_eq!(super::parse_schedule("sync every 15 minutes", 0).0, Some(Schedule::Every(900)));
        // A middle phrase is removed too.
        let (at, cleaned) = super::parse_schedule("ping the server in 5 minutes please", 0);
        assert_eq!(at, Some(Schedule::Once(300)));
        assert_eq!(cleaned, "ping the server please");
        // No schedule → run now, request untouched.
        assert_eq!(super::parse_schedule("just do it now", 0), (None, "just do it now".to_string()));
        // "in" as ordinary prose (not a delay) does not misfire.
        assert_eq!(super::parse_schedule("look in the src folder", 0).0, None);
        // "at HH:MM" resolves to a future unix time.
        assert!(super::parse_schedule("email me at 17:30", 0).0.is_some());
    }

    #[test]
    fn diagram_draws_as_text_art_off_our_terminal() {
        // Not our GUI terminal (TERM_PROGRAM unset) → the picture in box art, never the
        // syntax, and never a native OSC the other terminal couldn't read.
        std::env::remove_var("TERM_PROGRAM");
        let out = super::diagram_output("flowchart TD\n A[Start] --> B[End]");
        assert!(out.contains("Start") && out.contains("End"), "the labels are drawn: {out:?}");
        assert!(out.contains('▼'), "an arrowhead is drawn: {out:?}");
        assert!(!out.contains("-->"), "no diagram syntax reaches the user: {out:?}");
        assert!(!out.contains("\x1b]1338"), "no native OSC off our terminal");
    }

    #[test]
    fn an_unreadable_diagram_still_falls_back_to_a_box() {
        std::env::remove_var("TERM_PROGRAM");
        let out = super::diagram_output("this is not a diagram at all");
        assert!(out.contains("diagram") && out.contains('╭'), "fallback box: {out:?}");
        assert!(out.contains("this is not a diagram at all"));
    }

    #[test]
    fn a_misspelled_flow_name_is_refused_and_pointed_at_the_real_one() {
        // This used to fall through to the `implement` pipeline, so a typo ran a
        // code-editing graph over the repository. Now it is an error with a hint.
        let (_h, _home) = crate::test_home::lock_home("cli-flow-typo");
        crate::config::Config::ensure_default();
        assert!(super::load_flow("review").is_ok(), "a bundled flow resolves by name");
        let err = super::load_flow("revieew").expect_err("a typo is not a flow");
        assert!(err.contains("no flow 'revieew'"), "{err}");
        assert!(err.contains("did you mean 'review'?"), "{err}");
        // Nothing that could escape the flows directory is a name at all.
        assert!(super::load_flow("../../etc/passwd").is_err());
        assert!(super::load_flow("").is_err());
    }

    #[test]
    fn the_tool_families_an_agent_declares_actually_work() {
        // `app_data` was `None` at the one place in the whole product that builds a
        // `CapCtx`, so `todo.*` / `data.*` / `queue.*` / `store.*` — nineteen registered
        // methods — answered "only available to installed apps" everywhere. Four of them
        // are declared by `coder`, whose own prompt tells it to mark `todo.done` as it
        // works, and five are granted to any agent that declares no tools.
        let (_h, _home) = crate::test_home::lock_home("cli-app-data");
        crate::config::Config::ensure_default();
        let cfg = crate::config::Config::load();
        let policy = std::sync::Arc::new(crate::security::Policy::new());
        let runner = super::build_runner(&cfg, &cfg.ai_settings(), None, policy, false);
        let ctx = &runner.ctx;
        assert!(ctx.app_data.is_some(), "a terminal run has somewhere to keep its own state");

        let run = |m: &str, args: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> =
                args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            crate::caps::run(m, &pairs, ctx)
        };
        // A checklist an agent keeps while it works.
        run("todo.set", &[("items", "[\"map the code\", \"make the edit\"]")]).expect("todo.set");
        run("todo.add", &[("text", "run the tests")]).expect("todo.add");
        run("todo.done", &[("text", "map the code")]).expect("todo.done");
        let todos = super::json_text(&run("todo.list", &[]).expect("todo.list"));
        assert!(todos.contains("run the tests"), "the list survives the calls: {todos}");

        // A table it builds up, and gets back.
        run("data.insert", &[("table", "notes"), ("row", "{\"who\":\"ada\",\"n\":1}")]).expect("data.insert");
        let rows = super::json_text(&run("data.query", &[("table", "notes")]).expect("data.query"));
        assert!(rows.contains("ada"), "the row reads back: {rows}");

        // And the other two families the same context unlocks.
        run("queue.push", &[("queue", "work"), ("item", "one")]).expect("queue.push");
        assert!(super::json_text(&run("queue.size", &[("queue", "work")]).expect("queue.size")).contains('1'));
        run("store.set", &[("key", "k"), ("value", "v")]).expect("store.set");
        assert!(super::json_text(&run("store.get", &[("key", "k")]).expect("store.get")).contains('v'));

        // It is the *project's* state, not a global pile: the folder decides.
        let session = crate::ai::Session::at(
            &std::env::current_dir().unwrap(),
            &crate::config::Config::sessions_dir(),
        );
        assert_eq!(ctx.app_data.as_ref(), Some(&session.data_dir()));
        assert!(session.data_dir().ends_with("data"));

        // And this is the shape of the bug, so the guard explains itself: with nowhere
        // to write, every one of those calls is a wasted turn and an error string the
        // model has to interpret.
        let nowhere = crate::caps::CapCtx { app_data: None, ..ctx.clone() };
        let err = crate::caps::run("todo.list", &[], &nowhere).expect_err("refused");
        assert!(err.contains("only available to installed apps"), "{err}");
    }

    #[test]
    fn a_job_run_is_never_silent_about_what_happened() {
        // The bug: a job that died before producing a line left a 0-byte log, so
        // `@job log` printed nothing and `@job show` said only "exit 2". The reason was
        // real and recoverable — no model configured — and there was no way to see it.
        let dir = std::env::temp_dir().join(format!("tt-joblog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1.md");

        let job = crate::jobs::Job {
            id: "1-1".into(),
            status: "running".into(),
            cmd: "get me the weather".into(),
            says: String::new(),
            task: crate::jobs::Task::Agent { agent: "coder".into(), text: "get me the weather".into() },
            cwd: "/tmp".into(),
            started: 0,
            finished: None,
            exit: None,
            pid: 0,
            schedule: None,
            next_at: None,
            runs: 0,
            last_exit: None,
        };
        let mut log = Some(std::fs::File::create(&path).unwrap());
        super::run_log_header(&mut log, &job);
        super::job_setup_error(&mut log, "AI isn't set up yet. Add a model to config.toml");
        super::run_log_footer(&mut log, 2);
        drop(log);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.trim().is_empty(), "a run that happened leaves a log");
        assert!(text.contains("@coder get me the weather"), "what ran: {text}");
        assert!(text.contains("AI isn't set up yet"), "why it stopped: {text}");
        assert!(text.contains("setup error (exit 2)"), "and the verdict: {text}");

        // …and `@job show` can name the reason without anybody opening the file.
        let reason = super::failure_reason(&path).expect("a reason");
        assert!(reason.starts_with("AI isn't set up yet"), "{reason}");

        // A log with no complaint in it yields no reason — it does not invent one.
        let clean = dir.join("2.md");
        std::fs::write(&clean, format!("# a job\n\nall good\n\n✓ done\n")).unwrap();
        assert_eq!(super::failure_reason(&clean), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bare_job_subcommand_means_the_newest_one() {
        // `show` and `cancel` defaulted to "", which `record::resolve` matched against every
        // id: it silently picked one with a single job and errored with "matches 2" as soon
        // as there were two.
        use super::JobCmd;
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::parse_job_args(&a(&["show"])), JobCmd::Show("last".into()));
        assert_eq!(super::parse_job_args(&a(&["cancel"])), JobCmd::Cancel("last".into()));
        assert_eq!(super::parse_job_args(&a(&["show", "17-3"])), JobCmd::Show("17-3".into()));
        assert_eq!(super::parse_job_args(&a(&["log"])), JobCmd::Log { id: "last".into(), follow: false });
    }

    #[test]
    fn a_node_with_no_log_is_told_why_not_suggested_back_to_itself() {
        // `@flow log last b` on a node that WAS skipped used to answer
        //   run … has no output for node 'b' — did you mean 'b'?
        // The node is 'b'. It ran the only way a skipped node can: not at all, because
        // its edge condition was false — which is the one thing the message should say.
        use crate::flowruns::{NodeRun, NodeState, Run};
        let node = |id: &str, state: NodeState| NodeRun { id: id.into(), state, ..Default::default() };
        let run = Run {
            id: "1785371201-90257".into(),
            flow: "review".into(),
            input: String::new(),
            status: "done".into(),
            cwd: String::new(),
            started: 0,
            finished: None,
            pid: 0,
            timeout: 0,
            budget: None,
            concurrency: 1,
            nodes: vec![
                node("a", NodeState::Done),
                node("b", NodeState::Skipped),
                node("c", NodeState::Blocked),
                node("d", NodeState::Waiting),
                node("e", NodeState::Pending),
            ],
        };

        // A node that exists is explained, never suggested back to itself.
        for (id, why) in [
            ("b", "its condition was false"),
            ("c", "something it needed failed"),
            ("d", "it is waiting for an answer"),
            ("e", "it has not run yet"),
        ] {
            let msg = super::no_output_message(&run, id).join("\n");
            assert!(msg.contains(why), "node '{id}' should say {why:?}, said: {msg}");
            assert!(!msg.contains("did you mean"), "node '{id}' suggested a name: {msg}");
            assert!(msg.contains(&format!("node '{id}' produced no output")), "{msg}");
        }

        // A name that genuinely is not in the graph still gets the suggestion — that is
        // the case `nearest` was for.
        let msg = super::no_output_message(&run, "bb").join("\n");
        assert!(msg.contains("has no node 'bb'"), "{msg}");
        assert!(msg.contains("did you mean 'b'"), "a real typo should be corrected: {msg}");
        assert!(msg.contains("nodes: a, b, c, d, e"), "{msg}");
    }

    #[test]
    fn every_bundled_agent_is_valid() {
        // An agent is a file somebody edits. A misspelled tool used to reach the model
        // with a plausible generic description and fail three minutes into a run; a
        // missing skill silently produced a weaker prompt with no sign of it.
        let (_h, _home) = crate::test_home::lock_home("cli-agents-valid");
        crate::config::Config::ensure_default();
        let problems = crate::ai::defs::validate(
            &crate::config::Config::agents_dir(),
            &crate::config::Config::skills_dir(),
            &crate::config::Config::prompts_dir(),
            &crate::caps::is_method,
        );
        assert!(problems.is_empty(), "the agents we ship are not valid:\n  {}", problems.join("\n  "));

        let agents = crate::ai::defs::load_agents(&crate::config::Config::agents_dir());
        assert!(agents.len() >= 5, "agents ship with the app");
        // Every agent a bundled flow names has to exist, or the flow dies partway
        // through — the one class of breakage a user cannot do anything about.
        for name in super::flow_names() {
            let flow = super::load_flow(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
            for node in &flow.nodes {
                if let crate::flow::Kind::Agent { agent, .. } = &node.kind {
                    assert!(
                        agents.iter().any(|a| &a.name == agent),
                        "flow '{name}' node '{}' wants agent '{agent}', which is not installed",
                        node.id
                    );
                }
            }
        }
    }

    #[test]
    fn an_agent_a_flow_chains_on_states_what_it_returns() {
        // `{{explore.output}}` is only as good as the agent's discipline, and the two
        // loops in the bundled flows branch on a literal verdict line. Both are
        // contracts, so both are checked rather than hoped for.
        let (_h, _home) = crate::test_home::lock_home("cli-agents-contract");
        crate::config::Config::ensure_default();
        for a in crate::ai::defs::load_agents(&crate::config::Config::agents_dir()) {
            assert!(
                a.system.contains("## What you return"),
                "agent '{}' does not say what it returns",
                a.name
            );
        }
        for name in ["tester", "reviewer"] {
            let a = crate::ai::defs::agent(&crate::config::Config::agents_dir(), name)
                .unwrap_or_else(|| panic!("{name} ships"));
            assert!(a.system.contains("VERDICT: PASS"), "{name} must promise the line the loops read");
            assert!(a.system.contains("VERDICT: FAIL"), "{name} must promise the line the loops read");
        }
    }

    #[test]
    fn every_bundled_flow_verifies_clean() {
        // The flows we ship are the worked examples of the format. If one of them
        // does not pass the tool's own checker, the format is not documented — it is
        // aspirational.
        let (_h, _home) = crate::test_home::lock_home("cli-flow-bundled");
        crate::config::Config::ensure_default();
        let names = super::flow_names();
        assert!(!names.is_empty(), "flows ship with the app");
        for name in names {
            let (flow, report) = super::checked_flow(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(report.ok(), "{name} has errors: {:?}", report.errors);
            assert!(!flow.description.is_empty(), "{name} needs a description — it is what `@flow` lists");
            // And each one draws, so `@flow graph <name>` can never come up empty.
            assert!(!crate::flow::render::draw(&flow, None, 100).is_empty(), "{name} draws");
        }
    }

    #[test]
    fn a_quoted_request_arrives_verbatim_and_loose_words_rejoin() {
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let run = |args: &[&str]| match super::parse_job_args(&a(args)) {
            super::JobCmd::Run(spec) => *spec,
            other => panic!("expected a run, got {other:?}"),
        };
        // One argument is the request exactly as typed — spacing, newlines and all.
        let spec = run(&["summarize  the   logs\nthen stop"]);
        assert_eq!(spec.request, "summarize  the   logs\nthen stop");
        // Loose words become a sentence.
        assert_eq!(run(&["summarize", "the", "logs"]).request, "summarize the logs");
        // A flag INSIDE the quoted request is text, not a flag.
        let spec = run(&["write docs for the --bg flag"]);
        assert_eq!(spec.request, "write docs for the --bg flag");
        assert!(!spec.bg, "the --bg inside the quotes never reached the parser");
        // …while a real flag beside it is one.
        assert!(run(&["summarize the logs", "--bg"]).bg);
    }

    #[test]
    fn a_command_after_the_separator_keeps_its_shape() {
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let cmd = |args: &[&str]| match super::parse_job_args(&a(args)) {
            super::JobCmd::Run(spec) => spec.cmd.expect("a command"),
            other => panic!("expected a run, got {other:?}"),
        };
        // Several words are argv — re-joining them would run `sh -c echo hi`.
        assert_eq!(
            cmd(&["--", "sh", "-c", "echo hi"]),
            crate::jobs::Cmd::Argv(vec!["sh".into(), "-c".into(), "echo hi".into()])
        );
        // One quoted word is a shell line, because pipes need a shell.
        assert_eq!(cmd(&["--", "ls | wc -l"]), crate::jobs::Cmd::Line("ls | wc -l".into()));
        assert_eq!(cmd(&["--shell", "ls | wc -l"]), crate::jobs::Cmd::Line("ls | wc -l".into()));
        // Flags before `--` still apply; after it, everything is the command.
        let spec = match super::parse_job_args(&a(&["--bg", "--", "./x.sh", "--bg"])) {
            super::JobCmd::Run(spec) => *spec,
            other => panic!("{other:?}"),
        };
        assert!(spec.bg);
        assert_eq!(spec.cmd, Some(crate::jobs::Cmd::Argv(vec!["./x.sh".into(), "--bg".into()])));
    }

    #[test]
    fn the_job_subcommands_are_recognized() {
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::parse_job_args(&[]), super::JobCmd::List);
        assert_eq!(super::parse_job_args(&a(&["clear"])), super::JobCmd::Clear);
        assert_eq!(super::parse_job_args(&a(&["cancel", "12-3"])), super::JobCmd::Cancel("12-3".into()));
        assert_eq!(super::parse_job_args(&a(&["show", "12-3"])), super::JobCmd::Show("12-3".into()));
        assert_eq!(super::parse_job_args(&a(&["log", "12-3", "-f"])), super::JobCmd::Log { id: "12-3".into(), follow: true });
        // `@job log` with no id follows the newest.
        assert_eq!(super::parse_job_args(&a(&["log"])), super::JobCmd::Log { id: "last".into(), follow: false });
        // The child form the scheduler spawns.
        assert_eq!(
            super::parse_job_args(&a(&["--run", "9-9", "--run-at", "1700000000"])),
            super::JobCmd::Occurrence { id: "9-9".into(), at: Some(1_700_000_000) }
        );
    }

    #[test]
    fn explicit_schedule_flags_are_read_without_a_model() {
        let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let sched = |args: &[&str]| match super::parse_job_args(&a(args)) {
            super::JobCmd::Run(spec) => spec.schedule,
            other => panic!("{other:?}"),
        };
        assert_eq!(sched(&["x", "--every", "15m"]), Some(crate::jobs::Schedule::Every(900)));
        assert_eq!(sched(&["x", "--every", "2 hours"]), Some(crate::jobs::Schedule::Every(7200)));
        assert!(matches!(sched(&["x", "--cron", "0 9 * * 1-5"]), Some(crate::jobs::Schedule::Cron(_))));
        assert!(matches!(sched(&["x", "--in", "30s"]), Some(crate::jobs::Schedule::Once(_))));
        assert_eq!(sched(&["x"]), None, "no flags → the planner decides");
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
        let mut st = state(5, None);
        let outcome = super::drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", &mut st, None, |_| {
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
        let mut st = state(5, None);
        let outcome = super::drive_loop(&client, &maker(), &mut NoTools, &mut crate::ai::NoopObserver, "goal", &mut st, None, |_| {
            panic!("the verifier must not run on a cancelled iteration")
        }).outcome;
        assert_eq!(outcome, super::LoopOutcome::Cancelled);
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
