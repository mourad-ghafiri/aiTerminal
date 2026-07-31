//! Turning a request into a job record, and running one occurrence of it.

use crate::cli::jobs::args::RunSpec;
use crate::cli::jobs::schedule::parse_schedule;
use crate::cli::jobs::shell::run_shell_job;
use crate::cli::jobs::spawn::{cwd_string, keep_runs, run_prompt_as_agent, unix_now};
use crate::cli::style::{accent, reset};

pub(crate) fn job_usage() -> String {
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
pub(crate) fn create_job(spec: RunSpec) -> i32 {
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
            // A command with no schedule FLAG still gets the word parser run over
            // whatever was typed before the `--`. Without this, `@job tomorrow at 9 --
            // ./deploy.sh` deployed immediately: the words were captured, echoed back as
            // the request, and then silently dropped — the worst shape a bug can take,
            // because the user was shown their schedule being understood.
            let sched = sched.or_else(|| parse_schedule(&spec.request, now).0);
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
pub(crate) fn describe(schedule: &Option<crate::jobs::Schedule>, what: &str) -> String {
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
pub(crate) fn run_occurrence_child(id: &str, at: Option<u64>) -> i32 {
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
pub(crate) fn run_log_header(log: &mut Option<std::fs::File>, job: &crate::jobs::Job) {
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
pub(crate) fn run_log_footer(log: &mut Option<std::fs::File>, code: i32) {
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
