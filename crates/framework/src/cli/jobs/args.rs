// ===== @job ==================================================================

/// What an `ai job …` invocation asks for. A pure parse, so the grammar people actually
/// type — a quoted request, loose prose, flags anywhere, `--` for a command — is
/// unit-testable without touching disk or a model.
use crate::cli::jobs::create::{create_job, job_usage, run_occurrence_child};
use crate::cli::jobs::schedule::{parse_clock_at, parse_delay, unit_secs};
use crate::cli::jobs::show::{ai_jobs, job_log, job_show};
use crate::cli::jobs::spawn::unix_now;

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
    pub(crate) bg: bool,
    pub(crate) dry_run: bool,
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
            "--in" => spec.schedule = it.next().and_then(|s| parse_delay(&[s.as_str()]).map(|(secs, _)| crate::jobs::Schedule::Once(unix_now().saturating_add(secs)))),
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
pub(crate) fn parse_period(rest: &[&str]) -> Option<(u64, usize)> {
    parse_delay(rest).or_else(|| rest.first().and_then(|w| unit_secs(w, 1)).map(|s| (s, 1)))
}

/// `@job` — the tracked-task surface. Bare lists; `clear` prunes; `cancel|log|show` operate
/// on one job; anything else is a request to turn into a job. `args` includes the leading
/// "job" word.
pub(crate) fn ai_job_cmd(args: &[String]) -> i32 {
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
