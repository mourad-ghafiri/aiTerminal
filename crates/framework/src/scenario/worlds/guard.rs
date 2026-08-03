//! The AI guard — what may run, what may be touched, and what may leave.
//!
//! Everything here is pure policy over text and path *strings*. The world has no shell, no
//! process and no filesystem: a destructive command is a string handed to `judge`, and the
//! assertion is that the verdict is `deny`. A "secret" is a value with the right shape and
//! no entropy, invented by the scenario.
//!
//! A scenario configures the guard in **the product's own vocabulary** — the same
//! `[[guard.command]]` / `[[guard.path]]` / `[[guard.secret]]` tables a user writes in
//! `config.toml` and a plugin writes in `plugin.toml` — so a journey proves the parser as
//! well as the policy, and there is only ever one way to write a rule.
//!
//! ```toml
//! [setup]
//! home = "/nowhere/person"          # what `~` means here; also builds the built-in floor
//!
//! [[setup.guard.path]]
//! pattern = "/clients/"
//! rule    = "deny"
//!
//! [[step]]
//! read = "~/clients/acme/notes.md"
//! [[step]]
//! expect_verdict = "deny"
//! ```
//!
//! Steps: `run` · `read` · `write` (judge an act) · `hide` · `restore` (the vault's two
//! directions) · `mask` · `scrub`.
//! Assertions: `expect_verdict` · `expect_reason` · `expect_auto` · `expect_text` ·
//! `expect_kept` · `expect_not_text` · `expect_error` · `expect_briefing`.

use std::path::PathBuf;

use corelib::wire::Toml;

use super::super::world::{self, World};
use crate::guard::{Act, Base, Decision, Guard, RuleSet};

pub struct GuardWorld {
    guard: Guard,
    /// Where `~` points, so a path step reads like a path a person would type.
    home: Option<PathBuf>,
    /// The verdict from the most recent `run` / `read` / `write`.
    verdict: Option<Decision>,
    /// The text the most recent `hide` / `restore` / `mask` / `scrub` produced.
    text: String,
    /// Why the most recent `restore` refused, if it did.
    error: String,
}

pub fn build(setup: &Toml) -> Result<Box<dyn World>, String> {
    let home = world::text(setup, "home").map(PathBuf::from);
    let cwd = world::text(setup, "cwd").map(PathBuf::from);
    let empty = Toml::Table(Vec::new());
    let rules = RuleSet::parse(setup.get("guard").unwrap_or(&empty));
    let (guard, skipped) = Guard::compile(&[&rules], Base { home: home.clone(), cwd });
    // A rule that will not compile is a warning in the product and a FAILURE here: a
    // scenario whose policy half-loaded would pass for the wrong reason. The one journey
    // that is *about* a bad pattern says so with `bad_rules = true`.
    if !skipped.is_empty() && !world::flag(setup, "bad_rules").unwrap_or(false) {
        return Err(format!("these rules do not compile: {}", skipped.join(" \u{b7} ")));
    }
    Ok(Box::new(GuardWorld { guard, home, verdict: None, text: String::new(), error: String::new() }))
}

impl GuardWorld {
    /// A path as the scenario wrote it, with `~` meaning the setup's `home`.
    fn path(&self, raw: &str) -> PathBuf {
        match raw.strip_prefix("~/") {
            Some(rest) => self.home.clone().unwrap_or_default().join(rest),
            None => PathBuf::from(raw),
        }
    }

    /// Put the values back, keeping BOTH answers: the text on success, the reason on
    /// refusal — so a journey can assert either without a second verb.
    fn put_back(&mut self, text: &str) -> Result<(), String> {
        match self.guard.vault().restore(text) {
            Ok(out) => {
                self.text = out;
                self.error.clear();
            }
            Err(why) => {
                self.error = why;
                self.text.clear();
            }
        }
        Ok(())
    }
}

impl World for GuardWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // ── what a run wants to do ──────────────────────────────────────────
        if let Some(cmd) = world::text(step, "run") {
            self.verdict = Some(self.guard.judge(Act::Run(&cmd)));
            return Ok(());
        }
        if let Some(p) = world::text(step, "read") {
            self.verdict = Some(self.guard.judge(Act::Read(&self.path(&p))));
            return Ok(());
        }
        if let Some(p) = world::text(step, "write") {
            self.verdict = Some(self.guard.judge(Act::Write(&self.path(&p))));
            return Ok(());
        }

        // ── what leaves, and what comes back ────────────────────────────────
        if let Some(text) = world::text(step, "hide") {
            self.text = self.guard.hide(&text);
            return Ok(());
        }
        if let Some(text) = world::text(step, "mask") {
            self.text = self.guard.mask(&text);
            return Ok(());
        }
        if let Some(text) = world::text(step, "scrub") {
            self.text = self.guard.scrub(&text);
            return Ok(());
        }
        // Bare `restore = true` puts the values back into whatever the last step produced —
        // which is how the round trip reads as one journey rather than two halves.
        if let Some(true) = world::flag(step, "restore") {
            let text = self.text.clone();
            return self.put_back(&text);
        }
        if let Some(text) = world::text(step, "restore") {
            return self.put_back(&text);
        }

        // ── what must be true ───────────────────────────────────────────────
        if let Some(want) = world::text(step, "expect_verdict") {
            let got = match self.verdict.as_ref().ok_or("nothing has been judged yet")? {
                Decision::Allow => "allow",
                Decision::Confirm { .. } => "confirm",
                Decision::Deny { .. } => "deny",
            };
            return world::expect_eq(got, &want, "the verdict");
        }
        if let Some(want) = world::list(step, "expect_reason") {
            let reason = self.verdict.as_ref().ok_or("nothing has been judged yet")?.why().to_string();
            return world::expect_contains(&reason, &want, "the verdict's reason");
        }
        if let Some(want) = world::flag(step, "expect_auto") {
            let cmd = world::text(step, "cmd").ok_or("expect_auto needs a `cmd`")?;
            let got = self.guard.auto_runs(&cmd);
            if got != want {
                return Err(format!("{cmd:?} auto-runs: expected {want}, got {got}"));
            }
            return Ok(());
        }
        if let Some(want) = world::text(step, "expect_text") {
            return world::expect_eq(&self.text, &want, "the text");
        }
        if let Some(want) = world::list(step, "expect_kept") {
            return world::expect_contains(&self.text, &want, "the text");
        }
        if let Some(bad) = world::list(step, "expect_not_text") {
            return world::expect_missing(&self.text, &bad, "the text");
        }
        if let Some(want) = world::list(step, "expect_error") {
            return world::expect_contains(&self.error, &want, "the refusal");
        }
        if let Some(want) = world::list(step, "expect_briefing") {
            return world::expect_contains(&self.guard.briefing(), &want, "what the model is told");
        }

        Err(world::unknown_verb(step))
    }
}
