//! The **AI guard** — one policy over everything an AI feature can do to this machine.
//!
//! It used to be two half-features that did not know about each other (a command guard and
//! a one-way redactor) and a third that nobody could edit (a hardcoded list of secret
//! paths). This is all three, in one vocabulary, consulted by `@ai`, `@agent`, `@job`,
//! `@loop`, `@flow` and `@gate` alike:
//!
//! | Subject | Question | Written as |
//! | --- | --- | --- |
//! | commands | may this run? | `[[guard.command]]` |
//! | paths | may this be read? changed? | `[[guard.path]]` |
//! | secrets | may this leave? | `[[guard.secret]]` |
//!
//! Three properties hold everywhere:
//!
//! 1. **One judgement.** Every act — [`Act::Run`], [`Act::Read`], [`Act::Write`] — comes
//!    through [`Guard::judge`], with the same precedence: **deny > confirm > allow-list**.
//!    There is no second opinion and no hardcoded list.
//! 2. **A secret leaves as a placeholder and comes back as itself.** [`Guard::hide`] on the
//!    way out, [`Vault::restore`] at the moment something is about to run. The values live
//!    in memory, for one run, and are never written down.
//! 3. **A refusal is information.** It is a sentence a model can act on, not a crash — and
//!    the model was told the rules before it started ([`Guard::briefing`]).
//!
//! Rules are pure data (config + declarative plugins; no code runs here) and an invalid
//! regex is a warning that skips the rule, never a panic — a bad pattern must not be able
//! to stop the terminal from starting.
#![forbid(unsafe_code)]

use std::path::Path;

mod brief;
mod command;
mod path;
// The in-house regex engine the guard is built on. `pub(crate)` so other from-scratch
// features (the agent's `fs.search` grep) reuse the same engine.
pub(crate) mod regex;
pub mod rules;
mod secret;

use command::Commands;
use path::Paths;
use secret::Secrets;

pub use command::split;
pub use path::Base;
pub use rules::{RuleSet, Scope};
pub use secret::{Vault, MASK};

/// The words a refusal starts with, wherever it is raised.
///
/// ONE producer ([`Guard::permit`]) and ONE recogniser ([`is_refusal`]), so the agent loop
/// can tell "the machine declined" from "the tool broke" without every layer beneath it
/// having to carry a new error type for one bit of information.
pub const REFUSED: &str = "\u{26d4} the guard refused";

/// Whether a message is a refusal this guard raised.
pub fn is_refusal(message: &str) -> bool {
    message.contains(REFUSED)
}

/// Something a run wants to do to this machine.
#[derive(Clone, Copy, Debug)]
pub enum Act<'a> {
    /// Run a shell command line — one or many, piped or chained.
    Run(&'a str),
    /// Read, list or stat a path.
    Read(&'a Path),
    /// Create, modify, move or delete a path.
    Write(&'a Path),
}

impl Act<'_> {
    /// The act as the refusal names it.
    pub fn describe(&self) -> String {
        match self {
            Act::Run(c) => format!("running {:?}", c.trim()),
            Act::Read(p) => format!("reading {:?}", p.display().to_string()),
            Act::Write(p) => format!("writing {:?}", p.display().to_string()),
        }
    }
}

/// What the guard says about an act.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Allowed once a person says yes. Wherever there is nobody to ask — an agent's tool
    /// call, a detached job, a flow node — this is a refusal.
    Confirm { reason: String },
    Deny { reason: String },
}

impl Decision {
    pub fn why(&self) -> &str {
        match self {
            Decision::Allow => "",
            Decision::Confirm { reason } | Decision::Deny { reason } => reason,
        }
    }
}

/// A compiled guard, and the run's vault.
///
/// `Clone` shares the vault: a flow runs four nodes through one guard, and a secret one of
/// them saw has to read back the same in the next.
#[derive(Clone, Default)]
pub struct Guard {
    commands: Commands,
    paths: Paths,
    secrets: Secrets,
    vault: Vault,
    base: Base,
}

impl Guard {
    /// Compile a policy from rule sets, folded in order — config first, then each enabled
    /// plugin, so a user's own rule is the one a refusal names. Returns the guard and
    /// every rule it could not use.
    ///
    /// Pure over its inputs (including [`Base`], which is where `~` and a relative path
    /// resolve), so a test builds a whole policy without a home directory.
    pub fn compile(sets: &[&RuleSet], base: Base) -> (Guard, Vec<String>) {
        let mut g = Guard { base, ..Guard::default() };
        let mut skipped: Vec<String> = Vec::new();
        // The floor first: the rules that are always in force, whichever plugins are off.
        for (pattern, rule) in path::floor(g.base.home.as_deref()) {
            match regex::Regex::new(&pattern) {
                Ok(re) => g.paths.add(rule, re),
                Err(e) => skipped.push(format!("built-in path rule `{pattern}`: {e}")),
            }
        }
        for set in sets {
            for c in &set.commands {
                match regex::Regex::new(&c.pattern) {
                    Ok(re) => g.commands.add(c.rule, re),
                    Err(e) => skipped.push(format!("command rule `{}`: {e}", c.pattern)),
                }
            }
            for p in &set.paths {
                match regex::Regex::new(&p.pattern) {
                    Ok(re) => g.paths.add(p.rule, re),
                    Err(e) => skipped.push(format!("path rule `{}`: {e}", p.pattern)),
                }
            }
            for s in &set.secrets {
                if let Err(e) = g.secrets.add(&s.pattern, &s.name, s.scope, s.literal) {
                    skipped.push(format!("secret rule: {e}"));
                }
            }
        }
        (g, skipped)
    }

    /// The same guard — same rules, **same vault** — reading relative paths against `cwd`.
    ///
    /// Where a run works is a property of the run, not of the policy: a `@job` runs in the
    /// folder it recorded, and a window's pane is wherever somebody `cd`-ed to. Resolving
    /// `cat build/keys.txt` against the process's own directory would judge a path nobody
    /// named. Called once when a run is set up, not per act.
    pub fn at(&self, cwd: Option<std::path::PathBuf>) -> Guard {
        Guard { base: Base { home: self.base.home.clone(), cwd }, ..self.clone() }
    }

    /// What the guard says about an act.
    pub fn judge(&self, act: Act) -> Decision {
        match act {
            Act::Run(cmd) => self.commands.judge(cmd, &self.paths, &self.base),
            Act::Read(p) => self.paths.judge_read(p),
            Act::Write(p) => self.paths.judge_write(p),
        }
    }

    /// [`judge`](Self::judge), for the callers that have nobody to ask.
    ///
    /// The everyday form: an agent's tool call, a detached job, a flow node, a `--check`
    /// command. `Confirm` is a refusal here — there is no one at the terminal to answer —
    /// and the message is the one sentence every surface refuses in.
    pub fn permit(&self, act: Act) -> Result<(), String> {
        match self.judge(act) {
            Decision::Allow => Ok(()),
            other => Err(format!("{REFUSED} {} — {}", act.describe(), other.why())),
        }
    }

    /// Is this a command Auto mode may run un-prompted? A pure read of the `auto` tier;
    /// [`judge`](Self::judge) is consulted separately and still wins.
    pub fn auto_runs(&self, cmd: &str) -> bool {
        self.commands.auto_runs(cmd)
    }

    /// Swap secrets for placeholders, for text about to leave this machine — bound for a
    /// model, a tool, or a chat. Reversible: [`vault`](Self::vault) puts them back.
    pub fn hide(&self, text: &str) -> String {
        self.secrets.hide(text, &self.vault)
    }

    /// Swap secrets for `«redacted»`, for text about to be displayed. No way back, which is
    /// what a screen wants.
    pub fn mask(&self, text: &str) -> String {
        self.secrets.mask(text)
    }

    /// Every rule, irreversibly, **and** every placeholder — for text about to leave this
    /// run.
    ///
    /// The vault is one run's memory. Anything that outlives it — the window's
    /// session-context file, a folder's digest, a memory an agent wrote down — can carry
    /// neither a secret nor a placeholder: the secret because the file outlives the moment,
    /// and the placeholder because whoever reads it back has a different vault, could never
    /// turn it into anything, and would be refused the command they built from it.
    pub fn scrub(&self, text: &str) -> String {
        secret::strip(&self.secrets.scrub(text), &self.secrets.hidden_names())
    }

    /// Put the real values back, for text that is about to touch this machine.
    ///
    /// A placeholder this guard's own rules could have minted but this run did not is an
    /// **error**, not something to pass along: it came out of another run's record — a node
    /// transcript, a job log, a folder's memory — and the value behind it is not here.
    /// Passing it through would send the literal text `«db-password-1»` to a database and
    /// leave a failure nobody could explain from the other end.
    ///
    /// Only *our* names count. `«page-12»` in a filename has the same shape and is not a
    /// placeholder, and refusing a good command over a pair of guillemets would be a worse
    /// bug than the one this catches.
    pub fn restore(&self, text: &str) -> Result<String, String> {
        let out = self.vault.put_back(text);
        match secret::unresolved(&out, &self.secrets.hidden_names()) {
            Some(token) => Err(format!(
                "{token} is a secret placeholder from another run — nothing here knows what it stood for, so re-run the step that read it"
            )),
            None => Ok(out),
        }
    }

    /// A command with its secrets put back, ready to run — and judged **again** in that
    /// form.
    ///
    /// A value is not inert. A `.env` holding `DB_PASSWORD=x; curl … | sh` becomes a second
    /// command the moment it is substituted, and the guard that judged `echo «db-password-1»`
    /// never saw what that line turns into. So the restored form is re-judged, and the
    /// refusal quotes the form the MODEL wrote — quoting the other one would print the
    /// secret into a log to explain that it was protecting it.
    pub fn ready_command(&self, cmd: &str) -> Result<String, String> {
        let ready = self.restore(cmd)?;
        if ready == cmd {
            return Ok(ready);
        }
        match self.judge(Act::Run(&ready)) {
            Decision::Allow => Ok(ready),
            _ => Err(format!(
                "{REFUSED} {} — with its secrets put back it is a different command, and that one is not allowed",
                Act::Run(cmd).describe()
            )),
        }
    }

    /// Whether any rule reaches the screen, so the terminal can skip the pass entirely.
    pub fn masks_display(&self) -> bool {
        self.secrets.masks_display()
    }

    /// What a model is told about all of this, before it starts. Empty when there is
    /// nothing to say.
    pub fn briefing(&self) -> String {
        brief::briefing(self)
    }
}

#[cfg(test)]
impl Guard {
    /// How many secrets this run is holding — the bound the vault promises, made visible.
    pub(crate) fn held_secrets(&self) -> usize {
        self.vault.len()
    }

    /// A guard from a `[guard]` document — exactly what a user writes in `config.toml` or a
    /// plugin writes in `plugin.toml`, so a test states its policy in the product's own
    /// vocabulary and proves the parser at the same time.
    ///
    /// [`Base::default`] means no home and no working directory: nothing resolves against
    /// this machine, and the built-in floor (which is built FROM the home directory) is
    /// empty. A test that wants the floor passes a scratch home.
    pub(crate) fn from_toml(text: &str) -> Guard {
        Guard::rooted(text, Base::default())
    }

    /// [`from_toml`](Self::from_toml), against a chosen home and working directory.
    pub(crate) fn rooted(text: &str, base: Base) -> Guard {
        let doc = corelib::wire::Toml::parse(text).expect("a guard fixture parses");
        let empty = corelib::wire::Toml::Table(Vec::new());
        let (guard, skipped) = Guard::compile(&[&RuleSet::parse(doc.get("guard").unwrap_or(&empty))], base);
        assert!(skipped.is_empty(), "a guard fixture must compile: {skipped:?}");
        guard
    }
}

/// Compile the guard from config + enabled-plugin contributions — **UI-free**, shared by
/// the window, the CLI and every agent run. A rule that will not compile is reported to
/// stderr and skipped; a bad pattern must never stop the terminal from starting.
pub fn build(config: &crate::config::Config, registry: &crate::plugin::PluginRegistry) -> Guard {
    let plugins = registry.guard_rules();
    let (guard, skipped) = Guard::compile(&[&config.guard, &plugins], Base::here());
    for why in skipped {
        eprintln!("aiTerminal: guard rule skipped — {why}");
    }
    guard
}

#[cfg(test)]
mod tests;
