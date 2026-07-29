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
        let newest = run_logs(&dir).into_iter().next_back();
        newest.or_else(|| {
            let legacy = dir.join("log.md");
            legacy.is_file().then_some(legacy)
        })
    }
}

/// `95` → `1m`, `4000` → `1h` — coarse, glanceable durations.
pub(crate) fn human_age(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3600 {
        format!("{}h", secs / 3600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

pub(crate) fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A fresh, sortable job id: `<unix-secs>-<pid>`.
pub(crate) fn new_id() -> String {
    format!("{}-{}", now(), std::process::id())
}

/// The job's folder (the id is charset-checked so it can never escape the jobs dir).
pub(crate) fn dir(id: &str) -> Option<PathBuf> {
    let ok = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then(|| Config::jobs_dir().join(id))
}

/// Every occurrence log in a job folder, oldest first.
fn run_logs(dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join("runs")) else { return Vec::new() };
    let mut logs: Vec<(u64, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .filter_map(|p| {
            let seq: u64 = p.file_stem()?.to_str()?.parse().ok()?;
            Some((seq, p))
        })
        .collect();
    logs.sort_by_key(|(seq, _)| *seq);
    logs.into_iter().map(|(_, p)| p).collect()
}

/// Create the log file for the next occurrence and prune old ones.
pub(crate) fn open_run_log(id: &str, keep: usize) -> Option<(PathBuf, std::fs::File)> {
    let dir = dir(id)?;
    let runs = dir.join("runs");
    std::fs::create_dir_all(&runs).ok()?;
    let existing = run_logs(&dir);
    let next = existing
        .last()
        .and_then(|p| p.file_stem()?.to_str()?.parse::<u64>().ok())
        .map(|n| n + 1)
        .unwrap_or(1);
    // Keep the newest `keep - 1`, because this call is about to add one.
    let keep = keep.max(1);
    for old in existing.iter().take(existing.len().saturating_sub(keep - 1)) {
        let _ = std::fs::remove_file(old);
    }
    let path = runs.join(format!("{next}.md"));
    let file = std::fs::File::create(&path).ok()?;
    Some((path, file))
}

/// Write a record. Every field the job needs to run again lives here, so a scheduled run
/// hours later never has to reconstruct anything.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write(id: &str, job: &Job) {
    let Some(dir) = dir(id) else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
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
    let _ = std::fs::write(dir.join("job.toml"), Toml::Table(pairs).to_string());
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
    let all = list();
    if reference == "last" {
        return all.first().map(|j| j.id.clone()).ok_or_else(|| "no jobs yet".to_string());
    }
    if all.iter().any(|j| j.id == reference) {
        return Ok(reference.to_string());
    }
    // An id is `<unix-secs>-<pid>`, so the part a person actually reads off the list and
    // retypes is usually the tail. Any unique piece of it works — start, end, or middle.
    let hits: Vec<&Job> = all.iter().filter(|j| j.id.contains(reference)).collect();
    match hits.len() {
        1 => Ok(hits[0].id.clone()),
        0 => Err(format!("no such job '{reference}'")),
        n => Err(format!("'{reference}' matches {n} jobs — use more of the id")),
    }
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
mod tests {
    use super::*;

    /// Fixed local offset for the schedule tests: UTC, so the arithmetic is checkable by
    /// hand and never depends on where the test runs.
    const UTC: i64 = 0;

    fn cron(s: &str) -> Cron {
        Cron::parse(s).unwrap_or_else(|| panic!("{s:?} should parse"))
    }

    fn at(y: i64, mo: u32, d: u32, h: u32, mi: u32) -> u64 {
        corelib::datetime::to_unix(y, mo, d, h, mi, 0, UTC) as u64
    }

    #[test]
    fn cron_fields_cover_the_vocabulary() {
        assert!(Cron::parse("0 0 * * *").is_some());
        assert!(Cron::parse("*/15 * * * *").is_some());
        assert!(Cron::parse("0 18 * * 1-5").is_some());
        assert!(Cron::parse("30 9,17 1,15 * *").is_some());
        // Sunday is 0 or 7 — same schedule, each keeping the source it was written with.
        let from = at(2024, 3, 5, 0, 0);
        assert_eq!(cron("0 0 * * 7").next_after(from, UTC), cron("0 0 * * 0").next_after(from, UTC));
        // Nonsense is refused rather than matching everything.
        for bad in ["", "* * * *", "* * * * * *", "60 * * * *", "* 25 * * *", "abc * * * *", "*/0 * * * *", "5-1 * * * *"] {
            assert!(Cron::parse(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn midnight_fires_at_the_next_midnight() {
        let c = cron("0 0 * * *");
        // 2024-03-05 13:20 → 2024-03-06 00:00
        let next = c.next_after(at(2024, 3, 5, 13, 20), UTC).unwrap();
        assert_eq!(next, at(2024, 3, 6, 0, 0));
        // Exactly at midnight, the *next* one is tomorrow — never the same instant twice.
        assert_eq!(c.next_after(at(2024, 3, 6, 0, 0), UTC).unwrap(), at(2024, 3, 7, 0, 0));
    }

    #[test]
    fn weekday_and_monthday_rules() {
        // Weekdays at 18:00: Friday → Monday.
        let c = cron("0 18 * * 1-5");
        let friday_evening = at(2024, 3, 8, 19, 0); // 2024-03-08 is a Friday
        assert_eq!(c.next_after(friday_evening, UTC).unwrap(), at(2024, 3, 11, 18, 0));
        // The 1st of each month at 03:00.
        let c = cron("0 3 1 * *");
        assert_eq!(c.next_after(at(2024, 3, 5, 0, 0), UTC).unwrap(), at(2024, 4, 1, 3, 0));
        // Both day fields restricted → either may match (cron's own rule).
        let c = cron("0 0 1 * 0");
        let from = at(2024, 3, 5, 0, 0); // Tue 5 Mar
        assert_eq!(c.next_after(from, UTC).unwrap(), at(2024, 3, 10, 0, 0)); // the coming Sunday
    }

    #[test]
    fn steps_and_lists() {
        let c = cron("*/15 * * * *");
        let t = at(2024, 3, 5, 10, 7);
        assert_eq!(c.next_after(t, UTC).unwrap(), at(2024, 3, 5, 10, 15));
        let c = cron("0 9,17 * * *");
        assert_eq!(c.next_after(at(2024, 3, 5, 10, 0), UTC).unwrap(), at(2024, 3, 5, 17, 0));
    }

    #[test]
    fn an_impossible_expression_gives_up_instead_of_spinning() {
        assert_eq!(cron("0 0 30 2 *").next_after(at(2024, 1, 1, 0, 0), UTC), None);
    }

    #[test]
    fn schedule_kinds_advance_as_they_should() {
        let now = 1_000_000;
        assert_eq!(Schedule::Once(now + 60).next_after(now), Some(now + 60));
        assert_eq!(Schedule::Once(now - 60).next_after(now), None, "a passed one-shot has no next fire");
        assert_eq!(Schedule::Every(900).next_after(now), Some(now + 900));
        assert!(Schedule::Every(900).repeats());
        assert!(!Schedule::Once(now).repeats());
    }

    #[test]
    fn a_command_is_displayed_the_way_it_would_be_retyped() {
        assert_eq!(Cmd::Argv(vec!["sh".into(), "-c".into(), "echo hi".into()]).display(), "sh -c 'echo hi'");
        assert_eq!(Cmd::Argv(vec!["./x.sh".into()]).display(), "./x.sh");
        assert_eq!(Cmd::Line("ls | wc -l".into()).display(), "ls | wc -l");
        // A quote inside a word survives the round trip.
        assert_eq!(Cmd::Argv(vec!["echo".into(), "it's".into()]).display(), r"echo 'it'\''s'");
    }

    /// A job with the given task and schedule, as `@job` would first write it.
    fn fixture(id: &str, task: Task, schedule: Option<Schedule>) -> Job {
        Job {
            id: id.into(),
            status: if schedule.is_some() { "scheduled".into() } else { "running".into() },
            cmd: "check the logs".into(),
            says: "every day at 00:00 — check the logs".into(),
            task,
            cwd: "/tmp".into(),
            started: 1_700_000_000,
            finished: None,
            exit: None,
            pid: std::process::id(),
            next_at: schedule.as_ref().and_then(|s| s.next_after(1_700_000_000)),
            schedule,
            runs: 0,
            last_exit: None,
        }
    }

    #[test]
    fn a_record_round_trips_through_disk() {
        let (_h, _home) = crate::test_home::lock_home("jobs-round-trip");
        let job = fixture("100-1", Task::Agent { text: "check the logs".into(), agent: "coder".into() }, Some(Schedule::Cron(cron("0 0 * * *"))));
        write("100-1", &job);
        let back = read("100-1").expect("the record reads back");
        assert_eq!(back.task, job.task);
        assert_eq!(back.schedule, job.schedule);
        assert_eq!(back.next_at, job.next_at);
        assert_eq!(back.says, job.says);
        assert_eq!(back.cwd, "/tmp");

        // A shell job keeps its argv words separate — the whole point of `Cmd::Argv`.
        let argv = Cmd::Argv(vec!["sh".into(), "-c".into(), "echo hi".into()]);
        write("100-2", &fixture("100-2", Task::Shell(argv.clone()), Some(Schedule::Every(900))));
        assert_eq!(read("100-2").unwrap().task, Task::Shell(argv));
    }

    #[test]
    fn a_record_from_the_previous_layout_still_loads() {
        let (_h, _home) = crate::test_home::lock_home("jobs-legacy");
        let dir = dir("900-1").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        // Exactly what the shipped version wrote: flat keys, no `kind`, no `[schedule]`.
        std::fs::write(
            dir.join("job.toml"),
            "cmd = \"summarize the logs --agent reviewer\"\nstatus = \"done\"\nstarted = 1700000000\npid = 42\nexit = 0\n",
        )
        .unwrap();
        let job = read("900-1").expect("an old record still reads");
        assert_eq!(job.status, "done");
        assert_eq!(job.exit, Some(0));
        // No `kind` means it was an agent task, and its text is the command line.
        assert!(matches!(job.task, Task::Agent { ref text, .. } if text.contains("summarize")));
        assert!(job.schedule.is_none());
    }

    #[test]
    fn finishing_advances_a_repeating_job_and_ends_a_one_shot() {
        let (_h, _home) = crate::test_home::lock_home("jobs-finish");
        write("200-1", &fixture("200-1", Task::Shell(Cmd::Line("true".into())), Some(Schedule::Every(60))));
        mark_running("200-1", 4242);
        finish("200-1", 0);
        let job = read("200-1").unwrap();
        assert_eq!(job.status, "scheduled", "a repeating job goes back to waiting");
        assert_eq!(job.runs, 1);
        assert_eq!(job.last_exit, Some(0));
        assert!(job.next_at.unwrap() > now(), "and has its next fire computed");

        write("200-2", &fixture("200-2", Task::Shell(Cmd::Line("false".into())), None));
        finish("200-2", 3);
        let once = read("200-2").unwrap();
        assert_eq!(once.status, "failed");
        assert_eq!(once.exit, Some(3));
    }

    #[test]
    fn cancelling_ends_the_schedule_for_good() {
        let (_h, _home) = crate::test_home::lock_home("jobs-cancel");
        write("300-1", &fixture("300-1", Task::Shell(Cmd::Line("sleep 9".into())), Some(Schedule::Every(60))));
        // The pid is this test process, which is alive — so cancel must not signal it.
        let mut job = read("300-1").unwrap();
        job.pid = 0;
        write("300-1", &job);
        assert!(cancel("300-1").unwrap().contains("cancelled"));
        let after = read("300-1").unwrap();
        assert_eq!(after.status, "cancelled");
        assert!(after.schedule.is_none(), "no further occurrences");
        assert!(!after.is_live());
        // Cancelling again says so rather than failing.
        assert!(cancel("300-1").unwrap().contains("already"));
    }

    #[test]
    fn a_reference_resolves_by_any_unique_piece_or_last() {
        let (_h, _home) = crate::test_home::lock_home("jobs-resolve");
        write("500-1", &fixture("500-1", Task::Shell(Cmd::Line("true".into())), None));
        write("600-2", &fixture("600-2", Task::Shell(Cmd::Line("true".into())), None));
        assert_eq!(resolve("600-2").unwrap(), "600-2");
        assert_eq!(resolve("60").unwrap(), "600-2", "an unambiguous prefix is enough");
        // The tail is what a person reads off the list and retypes — it must work too.
        assert_eq!(resolve("2").unwrap(), "600-2", "an unambiguous suffix is enough");
        assert_eq!(resolve("last").unwrap(), "600-2", "newest first");
        assert!(resolve("nope").is_err());
        // Now two ids contain "600" → ambiguous, and it says so instead of guessing.
        write("6000-3", &fixture("6000-3", Task::Shell(Cmd::Line("true".into())), None));
        assert!(resolve("600").unwrap_err().contains("matches 2"));
    }

    #[test]
    fn run_logs_rotate_and_the_newest_is_the_one_shown() {
        let (_h, _home) = crate::test_home::lock_home("jobs-logs");
        write("700-1", &fixture("700-1", Task::Shell(Cmd::Line("true".into())), Some(Schedule::Every(60))));
        for i in 1..=5 {
            let (path, _f) = open_run_log("700-1", 3).expect("a log opens");
            assert!(path.ends_with(format!("{i}.md")), "sequence keeps counting: {path:?}");
        }
        let kept = run_logs(&dir("700-1").unwrap());
        assert_eq!(kept.len(), 3, "only the newest three survive: {kept:?}");
        assert!(kept.last().unwrap().ends_with("5.md"));
        assert_eq!(read("700-1").unwrap().latest_log().unwrap(), *kept.last().unwrap());
    }

    #[test]
    fn clear_keeps_live_jobs_and_prunes_the_rest() {
        let (_h, _home) = crate::test_home::lock_home("jobs-clear");
        write("800-1", &fixture("800-1", Task::Shell(Cmd::Line("true".into())), Some(Schedule::Every(60))));
        let mut done = fixture("800-2", Task::Shell(Cmd::Line("true".into())), None);
        done.status = "done".into();
        write("800-2", &done);
        assert_eq!(clear_finished(), 1);
        let left = list();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "800-1");
    }

    #[test]
    fn durations_read_at_a_glance() {
        assert_eq!(human_age(45), "45s");
        assert_eq!(human_age(90), "1m");
        assert_eq!(human_age(7200), "2h");
        assert_eq!(human_age(200_000), "2d");
    }
}
