//! Something to read while you wait.
//!
//! A run that is waiting on a model shows a spinner and nothing else, sometimes for a
//! long time. This puts one dim line beside it — a tip about this tool, a fact worth
//! knowing, a short quote — so the wait buys you something.
//!
//! Three rules decide whether that is charming or infuriating, and all three are
//! structural rather than a matter of taste:
//!
//! 1. **It occupies no rows.** It rides inside the spinner's own single self-erasing
//!    line, or inside one constant row of a flow board. Nothing scrolls, nothing
//!    accumulates, nothing survives the run.
//! 2. **It only appears while nothing else is happening.** [`Muse`] is silent until a
//!    wait has lasted `after`, and the moment the answer starts streaming the spinner
//!    stops and takes the line with it. A run that answers in three seconds never shows
//!    one.
//! 3. **It cannot reach anything but a screen.** The spinner is TTY-only and the board
//!    row is drawn only when the board is live, so a pipe, a `--bg` job, a job log, a
//!    flow record and CI never see it.
//!
//! The lines themselves are written by the model, once, into a cache and reused
//! ([`refill`]). With no model configured there is no pool and the feature is simply
//! absent — no shipped fallback, because a canned line pretending to be a fresh one is
//! worse than a plain spinner.

pub(crate) mod refill;

use corelib::wire::Toml;
use std::time::Duration;

/// What a line is for. A person picks these in `[motivation] kinds`, so they are the
/// vocabulary of the setting as much as of the code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// About aiTerminal itself — the waiting time teaches you the tool you are using.
    Tip,
    /// Something true and worth knowing.
    Fact,
    /// A short attributed line.
    Quote,
    /// Plain encouragement, carrying no information at all.
    Cheer,
}

impl Kind {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Kind::Tip => "tips",
            Kind::Fact => "facts",
            Kind::Quote => "quotes",
            Kind::Cheer => "encouragement",
        }
    }

    pub(crate) fn read(word: &str) -> Option<Kind> {
        match word.trim().to_ascii_lowercase().as_str() {
            "tip" | "tips" => Some(Kind::Tip),
            "fact" | "facts" => Some(Kind::Fact),
            "quote" | "quotes" => Some(Kind::Quote),
            "cheer" | "encouragement" => Some(Kind::Cheer),
            _ => None,
        }
    }

    /// Every kind, for a config that names none.
    pub(crate) fn all() -> Vec<Kind> {
        vec![Kind::Tip, Kind::Fact, Kind::Quote, Kind::Cheer]
    }
}

/// The longest a line may be.
///
/// Not a matter of taste: the line rides inside a single terminal row that is erased with
/// `\r`, and a line that wraps becomes two rows the erase cannot reach. Anything longer
/// is dropped rather than truncated — half a fact is not a fact.
pub(crate) const MAX_LEN: usize = 70;

/// One line.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Line {
    pub(crate) kind: Kind,
    pub(crate) text: String,
}

impl Line {
    /// A line, if it is one. Empty, multi-line and over-long text is refused here so
    /// nothing downstream has to think about it.
    pub(crate) fn new(kind: Kind, text: &str) -> Option<Line> {
        let text = text.trim();
        let one_line = !text.contains('\n');
        (one_line && !text.is_empty() && text.chars().count() <= MAX_LEN)
            .then(|| Line { kind, text: text.to_string() })
    }
}

/// The lines this machine has, and when they were written.
#[derive(Clone, Debug, Default)]
pub(crate) struct Pool {
    pub(crate) lines: Vec<Line>,
    /// Unix seconds. `0` when there is no pool yet.
    pub(crate) written: u64,
}

/// Below this many usable lines the pool is refilled — few enough that a session starts
/// repeating itself is the trigger, rather than a fixed schedule.
pub(crate) const THIN: usize = 8;

/// How long a pool is used before it is written again. Long: these are lines, not data,
/// and a fresh set every day would be a model call every day for no gain.
pub(crate) const STALE: Duration = Duration::from_secs(14 * 24 * 3600);

impl Pool {
    /// The cached pool. Missing, unreadable or malformed → an empty one, which is the
    /// same thing as far as anything downstream is concerned.
    pub(crate) fn load() -> Pool {
        std::fs::read_to_string(path()).ok().map(|t| Pool::parse(&t)).unwrap_or_default()
    }

    pub(crate) fn parse(text: &str) -> Pool {
        let Ok(doc) = Toml::parse(text) else { return Pool::default() };
        let written = doc.get("written").and_then(|v| v.as_int()).unwrap_or(0).max(0) as u64;
        let empty: &[Toml] = &[];
        let lines = doc
            .get("line")
            .and_then(|v| v.as_array())
            .unwrap_or(empty)
            .iter()
            .filter_map(|t| {
                let kind = Kind::read(t.get("kind").and_then(|v| v.as_str())?)?;
                Line::new(kind, t.get("text").and_then(|v| v.as_str())?)
            })
            .collect();
        Pool { lines, written }
    }

    pub(crate) fn to_toml(&self) -> String {
        let mut out = format!("# Written by the model and reused. Delete this file to have it written again.\nwritten = {}\n", self.written);
        for line in &self.lines {
            out.push_str(&format!("\n[[line]]\nkind = {:?}\ntext = {:?}\n", line.kind.word(), line.text));
        }
        out
    }

    /// Write it where the next run will find it.
    ///
    /// Temp-then-rename, because the writer is a detached thread and the process may
    /// exit under it: a half-written file would be a pool that never parses again, and
    /// the feature would stay off until somebody deleted it by hand.
    pub(crate) fn save(&self) {
        let path = path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("toml.new");
        if std::fs::write(&tmp, self.to_toml()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// The lines of the kinds asked for.
    pub(crate) fn of(&self, kinds: &[Kind]) -> Vec<&Line> {
        self.lines.iter().filter(|l| kinds.contains(&l.kind)).collect()
    }

    /// Whether this pool is worth writing again — too few lines, or old.
    pub(crate) fn needs_refill(&self, now: u64) -> bool {
        self.lines.len() < THIN || now.saturating_sub(self.written) > STALE.as_secs()
    }
}

/// Where the pool lives.
///
/// Under `cache/` because it is regenerable by definition: delete it and the next run
/// writes another. The thing to CUSTOMIZE is `[motivation]` in the config, not this.
pub(crate) fn path() -> std::path::PathBuf {
    crate::config::Config::cache_dir().join("motivation.toml")
}

/// What a person can change.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub(crate) enabled: bool,
    pub(crate) kinds: Vec<Kind>,
    pub(crate) after: Duration,
    pub(crate) every: Duration,
}

/// Decides what is on screen, and when.
///
/// **Pure**: no clock and no terminal inside it. The caller says how long the wait has
/// lasted and gets back the line to show, which is what makes the pacing — the part that
/// decides whether this feature is pleasant or maddening — something a test can state
/// rather than something anybody has to sit and watch.
pub(crate) struct Muse {
    lines: Vec<String>,
    after: Duration,
    every: Duration,
    /// Which line is showing, and how far into the wait it went up. `None` = nothing.
    showing: Option<(usize, Duration)>,
    /// Where the rotation is. Started somewhere arbitrary so two runs in a row do not
    /// open with the same line.
    next: usize,
}

impl Muse {
    /// A muse over `pool`, or one that never says anything — which is what an empty
    /// pool, an unconfigured model and `enabled = false` all come to.
    pub(crate) fn new(pool: &Pool, settings: &Settings, seed: u64) -> Muse {
        let lines: Vec<String> = match settings.enabled {
            true => pool.of(&settings.kinds).into_iter().map(|l| l.text.clone()).collect(),
            false => Vec::new(),
        };
        let next = match lines.is_empty() {
            true => 0,
            false => (seed % lines.len() as u64) as usize,
        };
        Muse { lines, after: settings.after, every: settings.every, showing: None, next }
    }

    /// A muse with nothing to say — the shape every disabled path takes, so no caller
    /// has to carry an `Option`.
    pub(crate) fn silent() -> Muse {
        Muse {
            lines: Vec::new(),
            after: Duration::MAX,
            every: Duration::MAX,
            showing: None,
            next: 0,
        }
    }

    /// The line to show `waited` into the current wait, if any.
    pub(crate) fn line(&mut self, waited: Duration) -> Option<&str> {
        if self.lines.is_empty() {
            return None;
        }
        if waited < self.after {
            // Either the wait is young, or a new one has started under us. Both mean the
            // last line's turn is over — the next wait opens with a fresh one rather
            // than resuming a line whose moment has passed.
            self.showing = None;
            return None;
        }
        let turn_over = match self.showing {
            None => true,
            Some((_, since)) => waited.saturating_sub(since) >= self.every,
        };
        if turn_over {
            self.showing = Some((self.next, waited));
            self.next = (self.next + 1) % self.lines.len();
        }
        self.showing.map(|(i, _)| self.lines[i].as_str())
    }

    /// Whether this muse can ever say anything — so a caller can skip building a label
    /// it will never use.
    pub(crate) fn mute(&self) -> bool {
        self.lines.is_empty()
    }
}

/// A muse for this run: the cached pool, the config, and — when the pool is thin and a
/// model is configured — a background refill that this run will not wait for.
///
/// The refill is detached on purpose. Nothing about the run you are watching may slow
/// down for a decoration, so this run uses whatever is on disk now and the next one gets
/// the benefit.
pub(crate) fn for_run(cfg: &crate::config::Config) -> Muse {
    let settings = cfg.motivation();
    if !settings.enabled {
        return Muse::silent();
    }
    let pool = Pool::load();
    if pool.needs_refill(crate::flowruns::now()) {
        refill::in_background(cfg);
    }
    Muse::new(&pool, &settings, seed())
}

/// Somewhere arbitrary to start the rotation. Not randomness for its own sake: without
/// it every run on a machine opens with the same line, which is the fastest way to make
/// a feature like this feel like wallpaper.
fn seed() -> u64 {
    crate::cli::agentloop::fnv1a(&format!("{}-{}", std::process::id(), crate::flowruns::now()))
}

#[cfg(test)]
mod tests;
