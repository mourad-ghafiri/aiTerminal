// ===== background jobs (run + track + monitor from the terminal) =============

/// Detach the CURRENT invocation as a tracked job: the child re-runs this exact argv with
/// its output in the job's log, and stamps the record when it exits. Shared by
/// `@ai --bg`, `@flow --bg` and `@loop --bg`; `@job` has its own record-driven path.
use crate::cli::agents::{available_agents_hint, build_agent_spec, run_agent_streaming};
use crate::cli::attach::collect_attachments;
use crate::cli::run::{instructions_preamble, memory_preamble, outcome_label, record_session_run, session_preamble};
use crate::cli::runner::context_settings;

pub(crate) fn spawn_background(args: &[String]) -> i32 {
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
        // Everything detached here is an AI run — `@ai --bg`, `@flow --bg`, `@loop --bg` —
        // and every one of them writes its answer as Markdown into this log. The task
        // below cannot say so: it records the shell command that really runs, which is
        // this binary re-invoked.
        markdown: true,
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
pub(crate) fn cwd_string() -> String {
    std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()
}

pub(crate) fn keep_runs() -> usize {
    crate::config::Config::load().jobs_keep_runs
}

/// Current unix time (seconds).
pub(crate) fn unix_now() -> u64 {
    crate::jobs::now()
}

/// Run `prompt` through `agent` with the full live chrome; when `log` is set the
/// streamed answer is ALSO written there (the foreground-tracked job's record).
pub(crate) fn run_prompt_as_agent(agent: &str, prompt: &str, mut log: Option<std::fs::File>) -> i32 {
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
pub(crate) fn job_setup_error(log: &mut Option<std::fs::File>, reason: &str) -> i32 {
    use std::io::Write;
    eprintln!("aiTerminal: {reason}");
    if let Some(f) = log.as_mut() {
        let _ = writeln!(f, "aiTerminal: {reason}");
    }
    2
}
