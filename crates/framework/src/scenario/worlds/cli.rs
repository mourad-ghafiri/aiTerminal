//! The `@`-command surface — profiles, themes, config, plugins, documents, jobs.
//!
//! This is the part of aiTerminal a new user meets first, and the part every recent bug
//! report came from. It had no scenarios: the `plugins` world proves the *registry*, the
//! `config` world proves the *loader*, and nothing at all proved that `@plugin disable x`
//! followed by `@plugin enable x` gets you back where you started. It did not, for a
//! while, and nobody's test noticed.
//!
//! So a step here is a command as typed, run through the very dispatch the shell calls —
//! and what is asserted afterwards is **what changed**, not what was printed. A message
//! is words, and words are pinned by unit tests where they are built; "the profile exists
//! and is now the active one" is the behaviour, and it is what a journey should be about.
//!
//! Each scenario gets a fresh `$HOME` under the process-wide lock (see
//! [`test_home`](crate::test_home)), held for the world's lifetime — so these never race
//! the other HOME-touching tests and never leak a temp directory into one.

use corelib::wire::Toml;

use super::super::world::{self, World};

pub struct CliWorld {
    /// Holds `$HOME` pointed at a scratch directory, restored when the world drops.
    _home: crate::test_home::HomeGuard,
    home: std::path::PathBuf,
    /// The exit status of the most recent command, for `expect_exit`.
    code: Option<i32>,
    /// What that command was, so a failed expectation names it.
    last: String,
}

pub fn build(setup: &Toml) -> Result<Box<dyn World>, String> {
    let tag = world::text(setup, "home").unwrap_or_else(|| "cli".into());
    let (guard, home) = crate::test_home::lock_home(&format!("scenario-{tag}"));
    // The same bootstrap the binary does on any command: seed the config, install the
    // catalog. A scenario that had to do this itself would be testing its own setup.
    crate::config::Config::ensure_default();
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    Ok(Box::new(CliWorld { _home: guard, home, code: None, last: String::new() }))
}

impl World for CliWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // ── doing something ────────────────────────────────────────────────────
        if let Some(line) = world::text(step, "run") {
            return self.run(&line);
        }
        if let Some(path) = world::text(step, "write_file") {
            let body = world::text(step, "body").unwrap_or_default();
            return self.write(&path, body.as_bytes());
        }
        if let Some(path) = world::text(step, "write_bytes") {
            // A file that is deliberately not text — for the commands that have to cope
            // with one rather than panic on it.
            let bytes: Vec<u8> =
                world::text(step, "bytes").unwrap_or_default().split(',').filter_map(|b| b.trim().parse().ok()).collect();
            return self.write(&path, &bytes);
        }
        if let Some(path) = world::text(step, "make_dir") {
            let full = self.home.join(&path);
            return std::fs::create_dir_all(full).map_err(|e| e.to_string());
        }

        // ── what changed ───────────────────────────────────────────────────────
        if let Some(want) = world::int(step, "expect_exit") {
            let got = self.code.ok_or("nothing has been run yet — add a `run` step")?;
            if got as i64 != want {
                return Err(format!("`{}` exited {got}, expected {want}", self.last));
            }
            return Ok(());
        }
        if let Some(want) = world::list(step, "expect_profiles") {
            let mut got: Vec<String> = crate::profile::list().into_iter().map(|p| p.id).collect();
            got.sort();
            let mut want = want;
            want.sort();
            return world::expect_lines(&got, &want, "the profiles on disk");
        }
        if let Some(want) = world::text(step, "expect_active_profile") {
            return world::expect_eq(&crate::profile::active_id(), &want, "the active profile");
        }
        if let Some(want) = world::text(step, "expect_theme") {
            return world::expect_eq(&crate::config::Config::load().theme, &want, "the configured theme");
        }
        if let Some(name) = world::text(step, "expect_plugin_enabled") {
            return self.expect_plugin(&name, true);
        }
        if let Some(name) = world::text(step, "expect_plugin_disabled") {
            return self.expect_plugin(&name, false);
        }
        if let Some(want) = world::list(step, "expect_plugins_listed") {
            let names: Vec<String> = self.registry().loaded().into_iter().map(|(n, ..)| n).collect();
            return world::expect_contains(&names.join("\n"), &want, "the plugins the registry knows");
        }
        if let Some(want) = world::int(step, "expect_plugin_count") {
            let got = self.registry().loaded().len() as i64;
            if got != want {
                return Err(format!("the registry holds {got} plugin(s), expected {want}"));
            }
            return Ok(());
        }
        if let Some(path) = world::text(step, "expect_file") {
            let full = self.home.join(&path);
            return full.exists().then_some(()).ok_or(format!("{path} does not exist"));
        }
        if let Some(path) = world::text(step, "expect_no_file") {
            let full = self.home.join(&path);
            return (!full.exists()).then_some(()).ok_or(format!("{path} exists, and should not"));
        }
        if let Some(want) = world::int(step, "expect_jobs") {
            let got = crate::jobs::list().len() as i64;
            if got != want {
                return Err(format!("{got} job(s) recorded, expected {want}"));
            }
            return Ok(());
        }
        if let Some(want) = world::text(step, "expect_job_status") {
            let got = crate::jobs::list().first().map(|j| j.status.clone()).ok_or("there are no jobs")?;
            return world::expect_eq(&got, &want, "the newest job's status");
        }
        if world::flag(step, "expect_job_unscheduled") == Some(true) {
            let job = crate::jobs::list().into_iter().next().ok_or("there are no jobs")?;
            return match job.schedule.is_none() && job.next_at.is_none() {
                true => Ok(()),
                false => Err("the job is still scheduled to fire again".into()),
            };
        }
        Err(world::unknown_verb(step))
    }
}

impl CliWorld {
    /// One command, as typed at the prompt — `@` and all, so a scenario reads the way the
    /// docs do — through the same dispatch the shell integration calls.
    fn run(&mut self, line: &str) -> Result<(), String> {
        // `{home}` is the scratch directory. A command that takes a PATH takes it
        // relative to the working directory, which a scenario has no business changing —
        // so it names the file the same way it wrote it.
        let line = &line.replace("{home}", &self.home.display().to_string());
        let argv = split(line).ok_or_else(|| format!("{line:?} has an unbalanced quote"))?;
        let (head, rest) = argv.split_first().ok_or("a `run` step needs a command")?;
        let head = head.trim_start_matches('@');
        let rest: Vec<String> = rest.to_vec();
        // `@flow`, `@job` and `@loop` are verbs of the `ai` command; the rest are their
        // own. Exactly the mapping `builtin/shell` installs.
        let code = match head {
            "profile" => crate::cli::profile(&rest),
            "theme" => crate::cli::theme(&rest),
            "config" => crate::cli::config(&rest),
            "plugin" => crate::cli::plugin(&rest),
            "gate" => crate::cli::gate(&rest),
            "md" => crate::cli::md(&rest),
            "flow" | "job" | "loop" | "ai" => {
                let argv: Vec<String> = std::iter::once(head.to_string()).chain(rest).collect();
                crate::cli::ai(&argv)
            }
            other => return Err(format!("no command `@{other}` — this world runs the @-command surface")),
        };
        self.code = Some(code);
        self.last = line.to_string();
        Ok(())
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let full = self.home.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(full, bytes).map_err(|e| e.to_string())
    }

    fn registry(&self) -> crate::plugin::PluginRegistry {
        crate::plugin::load_registry(&crate::config::Config::load())
    }

    /// A plugin's enablement AS THE PRODUCT SEES IT — from the loaded registry, not from
    /// the store's `.disabled` file. That distinction is the whole bug: a plugin can be
    /// marked enabled on disk and still be missing from everything that uses it.
    fn expect_plugin(&self, name: &str, want: bool) -> Result<(), String> {
        let registry = self.registry();
        let found = registry.loaded().into_iter().find(|(n, ..)| n == name);
        match found {
            None => Err(format!("the registry has no plugin '{name}' at all — it cannot be described, listed or turned back on")),
            Some((_, _, _, _, enabled)) if enabled == want => Ok(()),
            Some(_) => Err(format!("plugin '{name}' is {}, expected {}", state(!want), state(want))),
        }
    }
}

fn state(on: bool) -> &'static str {
    if on {
        "enabled"
    } else {
        "disabled"
    }
}

/// Split a command line on whitespace, honouring double quotes — enough for anything a
/// person types at the prompt.
fn split(line: &str) -> Option<Vec<String>> {
    let (mut out, mut cur, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in line.trim().chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            ' ' if !quoted => {
                if !cur.is_empty() || any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            _ => cur.push(c),
        }
    }
    if quoted {
        return None;
    }
    if !cur.is_empty() || any {
        out.push(cur);
    }
    Some(out)
}

#[cfg(test)]
mod tests;
