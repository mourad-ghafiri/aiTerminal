use crate::cli::agentloop::args::ai_loop_cmd;
use crate::cli::agents::{ai_agent_cmd, run_agent_cli, wire_sigint};
use crate::cli::attach::collect_attachments;
use crate::cli::flow::show::ai_flow_cmd;
use crate::cli::format::run_footer_with;
use crate::cli::jobs::args::ai_job_cmd;
use crate::cli::jobs::spawn::spawn_background;
use crate::cli::live::TerminalSink;
use crate::cli::observe::Spinner;
use crate::cli::runner::fit_context;
use crate::cli::style::{muted, reset};

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
        Some("mcp") => return crate::cli::mcp::ai_mcp_cmd(),
        // Machinery, not surface: the detached child that writes the `[motivation]` line
        // pool (see `motivation::refill`). Deliberately absent from the usage text — it
        // is how the feature works, not something a person runs.
        Some("refill-motivation") => return crate::motivation::refill::run_now(),
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
        eprintln!("       aiTerminal ai mcp                          # the declared MCP servers, connected");
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
    // the terminal grounding, and any attached files. Every secret in it is hidden before egress.
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
    // Secrets leave as placeholders — and come back as themselves at whatever seam
    // touches this machine. This is the egress point for the grounding preamble.
    let registry = crate::plugin::load_registry(&cfg);
    let guard = crate::guard::build(&cfg, &registry);
    let ctx = guard.hide(&ctx);
    // The PROMPT too, not just the grounding around it. What somebody typed is text off
    // this machine like any other — a pasted key in a question is a key a model receives.
    let prompt = &guard.hide(prompt);
    let guard = std::sync::Arc::new(guard);
    let workspace_root = cwd_path.clone();

    // `--agent <name>` runs the agent's full tool loop (tools = native objects via a
    // pure `caps::run` runner), streaming live — no GUI/host needed.
    if let Some(name) = agent {
        let mode = format!("@{name}");
        let code = run_agent_cli(&cfg, settings, &name, prompt, &ctx, workspace_root, guard.clone(), media);
        record_session_run(session.as_ref(), &guard, &mode, prompt, &outcome_label(code));
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
            run_footer_with("\u{2713}", started.elapsed(), 0, crate::ai::Usage { input: tin as u32, output: tout as u32, ..Default::default() }, Some(client.model().cost(tin, tout)), cfg.ai_budget)
        };

        match out.reply {
            crate::ai::CommandReply::Failed(e) => {
                println!("{}", error_comment(&format!("AI error: {e}")));
            }
            crate::ai::CommandReply::Command(cmd) => {
                // Judged as the model wrote it — placeholders and all — so a refusal can be
                // shown, logged and remembered without a secret in it. The values go back
                // only into the line the shell will actually run.
                let verdict = guard.judge(crate::guard::Act::Run(&cmd));
                let ready = match guard.ready_command(&cmd) {
                    Ok(line) => line,
                    Err(why) => {
                        eprintln!("{dim}{}{r}", footer(tin, tout));
                        println!("{}", error_comment(&why));
                        return 0;
                    }
                };
                eprintln!("{dim}{ready}{r}");
                eprintln!("{dim}{}{r}", footer(tin, tout));
                println!("{}", command_marker(Some(&ready), Some(verdict), &cfg.ai_command_mode, &ready));
                // The RECORD keeps the placeholder form. A folder's session digest is read
                // back into a later prompt, and a secret written there would leak on a
                // different day, through a different run, with nothing to connect it to this one.
                record_session_run(session.as_ref(), &guard, "@ai", prompt, &cmd);
            }
            crate::ai::CommandReply::Answer => {
                sink.finish();
                eprintln!();
                eprintln!("{dim}{}{r}", footer(tin, tout));
                println!("{ANSWER_MARK}");
                record_session_run(session.as_ref(), &guard, "@ai", prompt, "answered");
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
    let mut spinner = Some(Spinner::start(crate::cli::observe::Motivated::label(crate::cli::observe::WAIT, &cfg)));
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
    eprintln!("{dim}{}{r}", run_footer_with("\u{2713}", started.elapsed(), 0, crate::ai::Usage { input: tin as u32, output: tout as u32, ..Default::default() }, Some(client.model().cost(tin, tout)), cfg.ai_budget));
    record_session_run(session.as_ref(), &guard, "@ai (q&a)", prompt, "answered");
    0
}

/// A compact outcome label from an exit code, for the folder-session digest.
pub(crate) fn outcome_label(code: i32) -> String {
    match code {
        0 => "ok".into(),
        2 => "setup error".into(),
        130 => "interrupted".into(),
        _ => "failed".into(),
    }
}

/// Append one run to this folder's session digest — best-effort, never blocks/fails a run.
/// Append one run to this folder's digest — a store that outlives the run and is read
/// back into a LATER one's context.
///
/// Scrubbed, not hidden. A placeholder belongs to the vault of the run that minted it; a
/// later run reading one back could never turn it into anything, and would refuse the
/// command built from it. And the secret itself must not be written down at all. So what
/// crosses a run boundary keeps neither — `«redacted»` is the honest record of "there was
/// something here you are not being shown".
pub(crate) fn record_session_run(session: Option<&crate::ai::Session>, guard: &crate::guard::Guard, mode: &str, prompt: &str, outcome: &str) {
    if let Some(s) = session {
        s.record_run(mode, &guard.scrub(prompt), &guard.scrub(outcome));
    }
}

/// The global AI instructions (`~/.aiTerminal/aiTerminal.md`) — the system-prompt
/// base for every run. Empty when the file is absent/blank.
pub(crate) fn instructions() -> String {
    std::fs::read_to_string(crate::config::Config::instructions_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

/// The context preamble carrying the global instructions (for the Q&A / command
/// paths, which have no system prompt of their own).
pub(crate) fn instructions_preamble() -> String {
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
pub(crate) fn session_lines() -> Vec<String> {
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
pub(crate) fn memory_preamble(cfg: &crate::config::Config, query: &str, folder_mem: Option<&std::path::Path>) -> String {
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
pub(crate) fn session_preamble(session: Option<&crate::ai::Session>) -> String {
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
pub(crate) const RUN_MARK: &str = "#TT-RUN# ";
pub(crate) const EDIT_MARK: &str = "#TT-EDIT# ";
pub(crate) const CONFIRM_MARK: &str = "#TT-CONFIRM# ";
/// A prose answer was already streamed to stderr — the shell preloads nothing.
pub(crate) const ANSWER_MARK: &str = "#TT-ANSWER#";

/// The single line `@ai --command` prints for a suggested command + guard verdict.
/// Pure (no I/O) so the dispatch policy is unit-testable: auto vs manual, the
/// always-review confirm tier, a guard block, and a model refusal / empty answer.
pub(crate) fn command_marker(cmd: Option<&str>, verdict: Option<crate::guard::Decision>, mode: &str, refusal: &str) -> String {
    use crate::guard::Decision;
    match (cmd, verdict) {
        (Some(c), Some(Decision::Allow)) => {
            if mode.eq_ignore_ascii_case("auto") {
                format!("{RUN_MARK}{c}")
            } else {
                format!("{EDIT_MARK}{c}")
            }
        }
        (Some(c), Some(Decision::Confirm { .. })) => format!("{CONFIRM_MARK}{c}"),
        (Some(_), Some(Decision::Deny { reason })) => format!("# blocked by guard: {reason}"),
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
pub(crate) fn tool_args_to_pairs(args: &str) -> Vec<(String, String)> {
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
pub(crate) fn json_text(v: &corelib::wire::Json) -> String {
    match v {
        corelib::wire::Json::Str(s) => s.clone(),
        other => other.to_string(),
    }
}
