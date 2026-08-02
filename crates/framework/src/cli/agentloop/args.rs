// ===== @loop — the surface ===================================================

/// What an `ai loop …` invocation asks for. A pure parse returning a `Result`, because the
/// whole point is that a bound you asked for and a bound you got are the same thing: a
/// misspelled value is an error here, never a silent default.
use crate::cli::agentloop::run::run_loop_cli;
use crate::cli::agentloop::show::{loop_list, loop_log, loop_show};
use crate::cli::jobs::spawn::spawn_background;

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
pub(crate) fn flag_value<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<String, String> {
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

pub(crate) fn loop_usage() -> String {
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
pub(crate) fn ai_loop_cmd(args: &[String]) -> i32 {
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
