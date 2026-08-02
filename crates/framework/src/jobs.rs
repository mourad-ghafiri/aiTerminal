//! Tracked jobs: the on-disk record, the schedule math, and the supervisor that keeps
//! recurring work firing across restarts.
//!
//! A job is a folder under `~/.aiTerminal/ai/jobs/<id>/`:
//!
//! ```text
//! job.toml        what to run, when, and how the last run went
//! runs/<n>.md     one log per occurrence (newest kept, each size-capped)
//! ```
//!
//! Plain TOML a human can read, edit or delete — deleting the folder is a valid way to
//! cancel a job. Everything here is deterministic: the AI reads a request **once**, at
//! creation, and writes its answer into the record; every occurrence after that is pure
//! arithmetic on `[schedule]`, so run #47 of an hourly job costs nothing and behaves
//! exactly like run #1.

use crate::config::Config;
use corelib::wire::Toml;
use std::path::PathBuf;

// ─────────────────────────────── the schedule ───────────────────────────────

/// When a job runs. `Once` fires and finishes; the other two repeat forever until
/// cancelled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Schedule {
    /// A single fire at this unix time.
    Once(u64),
    /// Every `n` seconds from the previous fire — "every 15 minutes".
    Every(u64),
    /// A five-field cron expression — the canonical form for anything clock-anchored
    /// ("at midnight" → `0 0 * * *`, "weekdays at 6pm" → `0 18 * * 1-5`).
    Cron(Cron),
}

impl Schedule {
    /// The first fire strictly after `now`, or `None` when a one-shot has passed.
    pub fn next_after(&self, now: u64) -> Option<u64> {
        match self {
            Schedule::Once(at) => (*at > now).then_some(*at),
            Schedule::Every(secs) => Some(now + (*secs).max(1)),
            Schedule::Cron(c) => c.next_after(now, platform::os::utc_offset_secs()),
        }
    }

    /// True when this schedule keeps firing (so the job survives `clear` and cancel means
    /// something).
    pub fn repeats(&self) -> bool {
        !matches!(self, Schedule::Once(_))
    }

    /// A short human phrase for the list — the planner's own sentence is preferred when
    /// there is one, this is the fallback.
    pub fn describe(&self) -> String {
        match self {
            // A one-shot is only meaningful as the wall clock it fires at.
            Schedule::Once(at) => format!(
                "at {}",
                corelib::datetime::format(*at as i64, "%H:%M", platform::os::utc_offset_secs())
            ),
            Schedule::Every(s) => format!("every {}", human_age(*s)),
            Schedule::Cron(c) => format!("cron {}", c.source),
        }
    }
}

/// A five-field cron expression: minute, hour, day-of-month, month, day-of-week.
///
/// Supports the vocabulary a schedule actually needs — `*`, a number, a `a-b` range, a
/// `a,b,c` list and a `*/n` or `a-b/n` step — evaluated against local civil time. Anything
/// it can't read fails to parse rather than silently matching everything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cron {
    pub source: String,
    minute: Field,
    hour: Field,
    dom: Field,
    month: Field,
    dow: Field,
}

/// One cron field as the set of values it allows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Field {
    /// `true` for `*` — matches anything, and (for day fields) means "not restricted".
    any: bool,
    values: Vec<u32>,
}

impl Field {
    fn matches(&self, v: u32) -> bool {
        self.any || self.values.contains(&v)
    }

    /// Parse one field, clamped to `[lo, hi]`. `dow` accepts `7` as Sunday.
    fn parse(spec: &str, lo: u32, hi: u32) -> Option<Field> {
        let spec = spec.trim();
        if spec.is_empty() {
            return None;
        }
        if spec == "*" {
            return Some(Field { any: true, values: Vec::new() });
        }
        let mut values = Vec::new();
        for part in spec.split(',') {
            let (range, step) = match part.split_once('/') {
                Some((r, s)) => (r, s.parse::<u32>().ok().filter(|n| *n > 0)?),
                None => (part, 1),
            };
            let (from, to) = if range == "*" {
                (lo, hi)
            } else if let Some((a, b)) = range.split_once('-') {
                (a.trim().parse().ok()?, b.trim().parse().ok()?)
            } else {
                let n: u32 = range.trim().parse().ok()?;
                (n, n)
            };
            if from > to || from < lo || to > hi {
                return None;
            }
            let mut v = from;
            while v <= to {
                values.push(v);
                v += step;
            }
        }
        values.sort_unstable();
        values.dedup();
        (!values.is_empty()).then_some(Field { any: false, values })
    }
}

impl Cron {
    /// `"0 0 * * *"` → a cron. `None` when it isn't five readable fields.
    pub fn parse(source: &str) -> Option<Cron> {
        let f: Vec<&str> = source.split_whitespace().collect();
        if f.len() != 5 {
            return None;
        }
        let dow = Field::parse(&f[4].replace('7', "0"), 0, 6)?;
        Some(Cron {
            source: source.split_whitespace().collect::<Vec<_>>().join(" "),
            minute: Field::parse(f[0], 0, 59)?,
            hour: Field::parse(f[1], 0, 23)?,
            dom: Field::parse(f[2], 1, 31)?,
            month: Field::parse(f[3], 1, 12)?,
            dow,
        })
    }

    /// The first minute strictly after `now` that this expression matches, in the local
    /// zone. Bounded: it walks at most four years of minutes before giving up, so an
    /// impossible expression (`0 0 30 2 *` — February 30th) returns `None` instead of
    /// spinning.
    pub fn next_after(&self, now: u64, offset: i64) -> Option<u64> {
        // Start at the next whole minute; cron has minute resolution.
        let mut t = ((now / 60) + 1) * 60;
        let limit = now + 4 * 366 * 86_400;
        while t <= limit {
            let dt = corelib::datetime::from_unix(t as i64, offset);
            if self.matches(&dt) {
                return Some(t);
            }
            // Skip a whole day when the day itself can't match — four years of
            // minute-by-minute stepping would be 2M iterations, this is ~1500.
            if !self.day_matches(&dt) {
                let midnight = corelib::datetime::to_unix(dt.year, dt.month, dt.day, 0, 0, 0, offset);
                t = (midnight + 86_400) as u64;
                continue;
            }
            t += 60;
        }
        None
    }

    fn matches(&self, dt: &corelib::datetime::DateTime) -> bool {
        self.minute.matches(dt.minute) && self.hour.matches(dt.hour) && self.day_matches(dt)
    }

    /// Cron's day rule: when both day fields are restricted, either one matching is enough.
    fn day_matches(&self, dt: &corelib::datetime::DateTime) -> bool {
        if !self.month.matches(dt.month) {
            return false;
        }
        match (self.dom.any, self.dow.any) {
            (true, true) => true,
            (false, true) => self.dom.matches(dt.day),
            (true, false) => self.dow.matches(dt.weekday),
            (false, false) => self.dom.matches(dt.day) || self.dow.matches(dt.weekday),
        }
    }
}

// ─────────────────────────────── the record ───────────────────────────────

/// What a job runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Task {
    /// An agent task: the request text, run by `agent`.
    Agent { text: String, agent: String },
    /// A command: either argv (executed directly) or a shell line.
    Shell(Cmd),
}

/// A command, in the form the user actually wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cmd {
    /// Separate words — executed directly, no shell, so quoting is never re-interpreted.
    Argv(Vec<String>),
    /// One quoted line — run through `/bin/sh -c`, because pipes and globs need a shell.
    Line(String),
}

impl Cmd {
    /// The command as one displayable/guard-checkable line.
    pub fn display(&self) -> String {
        match self {
            Cmd::Line(s) => s.clone(),
            Cmd::Argv(v) => v.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" "),
        }
    }
}

/// Shell-quote a word for display, so a re-typed line means what the argv did.
fn quote(w: &str) -> String {
    if !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "._-/=:@,+".contains(c)) {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', r"'\''"))
    }
}

/// One job, as read from disk.
#[derive(Clone, Debug)]
pub(crate) struct Job {
    pub id: String,
    pub status: String,
    /// What the user typed (display + `@job show`).
    pub cmd: String,
    /// The planner's sentence, when the AI read the request.
    pub says: String,
    pub task: Task,
    /// Whether this job's log is a Markdown document — what an agent writes — or the raw
    /// output of a command.
    ///
    /// A reader cannot tell by looking, and must not try: a shell job that prints a `#`
    /// line is not writing a heading. Nor can it be read off [`Task`], because a detached
    /// `@ai --bg` / `@flow --bg` / `@loop --bg` is recorded honestly as the shell command
    /// it really is — this binary, re-run. So the creator, which knows, writes it down.
    pub markdown: bool,
    pub cwd: String,
    pub started: u64,
    pub finished: Option<u64>,
    pub exit: Option<i32>,
    pub pid: u32,
    pub schedule: Option<Schedule>,
    pub next_at: Option<u64>,
    pub runs: u64,
    pub last_exit: Option<i32>,
}

impl Job {
    /// A glanceable timing note: a pending job shows when it fires next; others show when
    /// they started and how long they ran.
    pub fn timing(&self, now: u64) -> String {
        if self.status == "scheduled" {
            return match self.next_at {
                Some(at) if at > now => format!("fires in {}", human_age(at - now)),
                Some(_) => "due".to_string(),
                None => "pending".to_string(),
            };
        }
        let ago = human_age(now.saturating_sub(self.started));
        let dur = human_age(self.finished.unwrap_or(now).saturating_sub(self.started));
        format!("{ago} ago \u{b7} {dur}")
    }

    pub fn status_glyph(&self) -> &'static str {
        match self.status.as_str() {
            "running" => "\u{25B6}",
            "done" => "\u{2713}",
            "scheduled" => "\u{29D6}", // ⧖ waiting to fire
            "cancelled" => "\u{23f9}",
            "missed" => "\u{26A0}", // ⚠ its scheduler died before it fired
            _ => "\u{2717}",        // failed / died
        }
    }

    /// Is this job still live — running now, or waiting to fire again?
    pub fn is_live(&self) -> bool {
        matches!(self.status.as_str(), "running" | "scheduled")
    }

    /// The newest occurrence log, falling back to the pre-`runs/` layout.
    pub fn latest_log(&self) -> Option<PathBuf> {
        let dir = dir(&self.id)?;
        let newest = crate::record::logs(&dir, "runs").into_iter().next_back();
        newest.or_else(|| {
            let legacy = dir.join("log.md");
            legacy.is_file().then_some(legacy)
        })
    }
}

pub(crate) use crate::record::{human_age, new_id, now};

/// The job's folder — see [`crate::record::folder`] for why the id is charset-checked.
pub(crate) fn dir(id: &str) -> Option<PathBuf> {
    crate::record::folder(&Config::jobs_dir(), id)
}

/// Create the log file for the next occurrence and prune old ones.
pub(crate) fn open_run_log(id: &str, keep: usize) -> Option<(PathBuf, std::fs::File)> {
    crate::record::open_log(&dir(id)?, "runs", keep)
}

/// Write a record. Every field the job needs to run again lives here, so a scheduled run
/// hours later never has to reconstruct anything.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write(id: &str, job: &Job) {
    let Some(dir) = dir(id) else { return };
    let mut pairs = vec![
        ("cmd".into(), Toml::Str(job.cmd.clone())),
        ("status".into(), Toml::Str(job.status.clone())),
        ("started".into(), Toml::Int(job.started as i64)),
        ("pid".into(), Toml::Int(job.pid as i64)),
        ("cwd".into(), Toml::Str(job.cwd.clone())),
    ];
    if !job.says.is_empty() {
        pairs.push(("says".into(), Toml::Str(job.says.clone())));
    }
    if job.markdown {
        pairs.push(("markdown".into(), Toml::Bool(true)));
    }
    match &job.task {
        Task::Agent { text, agent } => {
            pairs.push(("kind".into(), Toml::Str("agent".into())));
            pairs.push(("text".into(), Toml::Str(text.clone())));
            pairs.push(("agent".into(), Toml::Str(agent.clone())));
        }
        Task::Shell(cmd) => {
            pairs.push(("kind".into(), Toml::Str("shell".into())));
            match cmd {
                Cmd::Line(line) => pairs.push(("shell".into(), Toml::Str(line.clone()))),
                Cmd::Argv(argv) => pairs.push(("argv".into(), Toml::Array(argv.iter().map(|w| Toml::Str(w.clone())).collect()))),
            }
        }
    }
    if let Some(f) = job.finished {
        pairs.push(("finished".into(), Toml::Int(f as i64)));
    }
    if let Some(e) = job.exit {
        pairs.push(("exit".into(), Toml::Int(e as i64)));
    }
    if let Some(s) = &job.schedule {
        let mut sched = match s {
            Schedule::Once(at) => vec![("kind".into(), Toml::Str("once".into())), ("at".into(), Toml::Int(*at as i64))],
            Schedule::Every(secs) => vec![("kind".into(), Toml::Str("every".into())), ("every".into(), Toml::Int(*secs as i64))],
            Schedule::Cron(c) => vec![("kind".into(), Toml::Str("cron".into())), ("cron".into(), Toml::Str(c.source.clone()))],
        };
        if let Some(at) = job.next_at {
            sched.push(("next_at".into(), Toml::Int(at as i64)));
        }
        sched.push(("runs".into(), Toml::Int(job.runs as i64)));
        if let Some(e) = job.last_exit {
            sched.push(("last_exit".into(), Toml::Int(e as i64)));
        }
        pairs.push(("schedule".into(), Toml::Table(sched)));
    }
    crate::record::save(&dir.join("job.toml"), &Toml::Table(pairs).to_string());
}

/// Read one record. Records written before `[schedule]` existed still load: a missing
/// `kind` is an agent task, a missing schedule is a job that has already had its one run.
pub(crate) fn read(id: &str) -> Option<Job> {
    let dir = dir(id)?;
    let text = std::fs::read_to_string(dir.join("job.toml")).ok()?;
    let doc = Toml::parse(&text).ok()?;
    let s = |k: &str| doc.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let i = |k: &str| doc.get(k).and_then(|v| v.as_int());
    let cmd = s("cmd");
    let task = if s("kind") == "shell" {
        let argv: Vec<String> = match doc.get("argv") {
            Some(Toml::Array(items)) => items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        };
        Task::Shell(if argv.is_empty() { Cmd::Line(s("shell")) } else { Cmd::Argv(argv) })
    } else {
        let text = if s("text").is_empty() { cmd.clone() } else { s("text") };
        let agent = if s("agent").is_empty() { "coder".to_string() } else { s("agent") };
        Task::Agent { text, agent }
    };
    let sched = doc.get("schedule");
    let schedule = sched.and_then(|t| {
        let kind = t.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "once" => t.get("at").and_then(|v| v.as_int()).map(|n| Schedule::Once(n.max(0) as u64)),
            "every" => t.get("every").and_then(|v| v.as_int()).map(|n| Schedule::Every(n.max(1) as u64)),
            "cron" => t.get("cron").and_then(|v| v.as_str()).and_then(Cron::parse).map(Schedule::Cron),
            _ => None,
        }
    });
    let field = |k: &str| sched.and_then(|t| t.get(k)).and_then(|v| v.as_int());
    Some(Job {
        id: id.to_string(),
        status: doc.get("status").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
        cmd,
        says: s("says"),
        markdown: doc.get("markdown").and_then(|v| v.as_bool()).unwrap_or(false),
        task,
        cwd: s("cwd"),
        started: i("started").unwrap_or(0).max(0) as u64,
        finished: i("finished").map(|n| n.max(0) as u64),
        exit: i("exit").map(|n| n as i32),
        pid: i("pid").unwrap_or(0).max(0) as u32,
        // A record from before `[schedule]` may still carry a flat `run_at` one-shot.
        schedule: schedule.or_else(|| i("run_at").map(|n| Schedule::Once(n.max(0) as u64))),
        next_at: field("next_at").map(|n| n.max(0) as u64).or_else(|| i("run_at").map(|n| n.max(0) as u64)),
        runs: field("runs").unwrap_or(0).max(0) as u64,
        last_exit: field("last_exit").map(|n| n as i32),
    })
}

/// Every recorded job, newest first — RECONCILED: a `running` record whose pid is gone
/// (crash, SIGKILL, reboot) heals to `died`, and a `scheduled` one whose sleeper is gone
/// stays `scheduled` so the supervisor can re-arm it (only a one-shot whose moment has
/// passed unrecoverably becomes `missed`).
pub(crate) fn list() -> Vec<Job> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(Config::jobs_dir()) else { return out };
    for e in entries.flatten() {
        let Some(id) = e.file_name().to_str().map(str::to_string) else { continue };
        let Some(mut job) = read(&id) else { continue };
        if job.status == "running" && !platform::os::pid_alive(job.pid) {
            job.status = "died".into();
            job.finished = Some(now());
            write(&id, &job);
        }
        out.push(job);
    }
    out.sort_by(|a, b| b.started.cmp(&a.started).then(b.id.cmp(&a.id)));
    out
}

/// Resolve a user-typed job reference: an exact id, any unambiguous piece of one, or
/// `last`.
pub(crate) fn resolve(reference: &str) -> Result<String, String> {
    let ids: Vec<String> = list().into_iter().map(|j| j.id).collect();
    crate::record::resolve(&ids, reference, "job")
}

/// Stamp a job's outcome, advancing a repeating schedule to its next fire.
pub(crate) fn finish(id: &str, code: i32) {
    finish_at(id, code, now())
}

/// [`finish`] against a given clock — the seam that makes "an hourly job re-anchors an hour
/// out" a checkable statement instead of a race with the wall clock.
pub(crate) fn finish_at(id: &str, code: i32, now: u64) {
    let Some(mut job) = read(id) else { return };
    job.exit = Some(code);
    job.last_exit = Some(code);
    job.runs += 1;
    job.finished = Some(now);
    match job.schedule.clone().filter(Schedule::repeats) {
        // A repeating job goes straight back to waiting, with its next fire computed.
        Some(sched) => {
            job.status = "scheduled".into();
            job.next_at = sched.next_after(now);
            job.pid = 0;
        }
        None => {
            job.status = match code {
                0 => "done",
                130 => "cancelled",
                _ => "failed",
            }
            .into();
        }
    }
    write(id, &job);
}

/// Flip a record to `running` for this occurrence.
pub(crate) fn mark_running(id: &str, pid: u32) {
    let Some(mut job) = read(id) else { return };
    job.status = "running".into();
    job.started = now();
    job.finished = None;
    job.exit = None;
    job.pid = pid;
    write(id, &job);
}

/// Cancel a job: stop its process if one is live, and end the schedule for good.
pub(crate) fn cancel(id: &str) -> Result<String, String> {
    let id = resolve(id)?;
    let mut job = read(&id).ok_or_else(|| format!("no such job '{id}'"))?;
    if !job.is_live() {
        return Ok(format!("job {id} is already {}", job.status));
    }
    if job.pid > 0 {
        platform::os::terminate(job.pid);
    }
    job.status = "cancelled".into();
    job.exit = Some(130);
    job.finished = Some(now());
    // Cancelling a recurring job means *stop*, so the schedule goes with it.
    job.schedule = None;
    job.next_at = None;
    write(&id, &job);
    Ok(format!("\u{23f9} cancelled job {id}"))
}

// ─────────────────────────────── the supervisor ───────────────────────────────

/// Spawn the detached process that runs one occurrence of `id` — immediately, or after
/// sleeping until `at`. Returns its pid.
///
/// Its own session (`setsid`), so closing the terminal that created the job never kills it.
pub(crate) fn spawn_occurrence(id: &str, at: Option<u64>) -> Option<u32> {
    let exe = std::env::current_exe().ok()?;
    let mut args = vec!["ai".to_string(), "job".to_string(), "--run".into(), id.to_string()];
    if let Some(at) = at {
        args.push("--run-at".into());
        args.push(at.to_string());
    }
    // The child writes its own per-run log; this stdio only catches anything that escapes.
    let sink = std::fs::File::create(dir(id)?.join("spawn.log")).ok()?;
    let err = sink.try_clone().ok()?;
    let pid = platform::os::spawn_detached(&exe, &args, sink, err).ok()?;
    Some(pid)
}

/// Arm a job's next fire: spawn the sleeper and record its pid, so the job survives this
/// terminal, this shell, and the app itself.
pub(crate) fn arm(id: &str, at: u64) -> bool {
    let Some(pid) = spawn_occurrence(id, Some(at)) else { return false };
    let Some(mut job) = read(id) else { return false };
    job.status = "scheduled".into();
    job.next_at = Some(at);
    job.pid = pid;
    write(id, &job);
    true
}

/// Bring every record back in line with reality — the piece that makes a schedule survive a
/// reboot, a quit, or a laptop lid.
///
/// For each job waiting to fire whose sleeper is gone: a fire-time still ahead gets a fresh
/// sleeper; a fire-time already past runs **once** and moves on (an hourly job that missed
/// six hours runs once, not six times — nobody wants six catch-up runs). A one-shot whose
/// moment passed while nothing was watching is `missed`, honestly. Bounded by
/// `[jobs] max_concurrent` so a fleet of due work can't fork-bomb the machine.
pub(crate) fn reconcile() {
    let cfg = crate::config::Config::load();
    let jobs = list(); // heals `running` records whose pid is gone
    let live = jobs.iter().filter(|j| j.status == "running" && platform::os::pid_alive(j.pid)).count();
    let watched = |j: &Job| platform::os::pid_alive(j.pid);
    for (id, action) in decide(&jobs, now(), live, cfg.jobs_max_concurrent, &watched) {
        match action {
            Action::Arm(at) => {
                arm(&id, at);
            }
            Action::RunNow => {
                spawn_occurrence(&id, None);
            }
            Action::Missed => {
                if let Some(mut job) = read(&id) {
                    job.status = "missed".into();
                    job.finished = Some(now());
                    write(&id, &job);
                }
            }
        }
    }
}

/// What reconciling decided to do about one job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Its fire-time is still ahead — spawn a fresh sleeper for it.
    Arm(u64),
    /// Overdue and it repeats — run one occurrence now, then it re-anchors.
    RunNow,
    /// A one-shot whose moment passed with nothing watching.
    Missed,
}

/// The decision half of [`reconcile`], with no disk and no processes in it: given the
/// records, the clock, how much is already running and the cap, say what should happen.
///
/// `watched` answers "is this job's sleeper still alive?" — the one fact that has to come
/// from the OS.
pub(crate) fn decide(
    jobs: &[Job],
    now: u64,
    live: usize,
    max_concurrent: usize,
    watched: &dyn Fn(&Job) -> bool,
) -> Vec<(String, Action)> {
    let mut live = live;
    let mut out = Vec::new();
    for job in jobs {
        if job.status != "scheduled" || watched(job) {
            continue; // not pending, or its sleeper is already waiting
        }
        let Some(at) = job.next_at else { continue };
        if at > now {
            out.push((job.id.clone(), Action::Arm(at)));
            continue;
        }
        // Overdue. A repeat catches up once and re-anchors from now; a one-shot that
        // nothing was around to fire is missed.
        match job.schedule.as_ref().filter(|s| s.repeats()) {
            Some(_) => {
                if live >= max_concurrent {
                    continue; // over the cap — try again on the next pass
                }
                live += 1;
                out.push((job.id.clone(), Action::RunNow));
            }
            None => out.push((job.id.clone(), Action::Missed)),
        }
    }
    out
}

/// Remove every job that is neither running nor waiting to fire. Returns how many went.
pub(crate) fn clear_finished() -> usize {
    let mut n = 0;
    for j in list() {
        if !j.is_live() {
            if let Some(d) = dir(&j.id) {
                if std::fs::remove_dir_all(d).is_ok() {
                    n += 1;
                }
            }
        }
    }
    n
}

#[cfg(test)]
mod tests;
