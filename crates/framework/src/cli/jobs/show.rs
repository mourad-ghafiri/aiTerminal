use crate::cli::agentloop::show::clip_tail;
use crate::cli::jobs::spawn::unix_now;
use crate::cli::style::{muted, reset};

/// `@job log <id> [-f]` — print the newest run's log, optionally following it.
pub(crate) fn job_log(id: &str, follow: bool) -> i32 {
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
    let running = || matches!(crate::jobs::read(&id), Some(j) if j.status == "running");
    crate::cli::logs::show_log(&path, follow, job.markdown, &running)
}

/// `@job show <id>` — everything the record knows, in the order a person asks it.
pub(crate) fn job_show(id: &str) -> i32 {
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
pub(crate) fn failure_reason(log: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(log).ok()?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("aiTerminal: "))
        .map(|l| clip_tail(l, 72))
}

/// `aiTerminal ai job [clear]` — list jobs (newest first), or prune the finished ones.
pub(crate) fn ai_jobs(args: &[String]) -> i32 {
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
