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
        if let Some(id) = world::text(step, "record_flow_run") {
            return self.record_flow(step, &id);
        }
        if let Some(id) = world::text(step, "record_loop_run") {
            return self.record_loop(step, &id);
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
        if let Some(want) = world::int(step, "expect_flow_runs") {
            let got = crate::flowruns::list().len() as i64;
            if got != want {
                return Err(format!("{got} flow run(s) recorded, expected {want}"));
            }
            return Ok(());
        }
        if let Some(want) = world::int(step, "expect_loop_runs") {
            let got = crate::loops::list().len() as i64;
            if got != want {
                return Err(format!("{got} loop run(s) recorded, expected {want}"));
            }
            return Ok(());
        }
        // `["<run>", "<node>", "<state>"]` — the fact `@flow retry` and `@flow resume` are
        // for. Nothing printed says it; the record is where it is true.
        if let Some(want) = world::list(step, "expect_flow_node_state") {
            let [id, node, state] = &want[..] else {
                return Err("expect_flow_node_state takes [\"<run>\", \"<node>\", \"<state>\"]".into());
            };
            let run = crate::flowruns::read(id).ok_or(format!("there is no flow run '{id}'"))?;
            let got = run.node(node).ok_or(format!("run '{id}' has no node '{node}'"))?;
            return world::expect_eq(got.state.word(), state, &format!("{id}/{node}"));
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
        // `@agent`, `@flow`, `@job` and `@loop` are verbs of the `ai` command; the rest
        // are their own. Exactly the mapping `builtin/shell` installs.
        let code = match head {
            "profile" => crate::cli::profile(&rest),
            "theme" => crate::cli::theme(&rest),
            "config" => crate::cli::config(&rest),
            "plugin" => crate::cli::plugin(&rest),
            "gate" => crate::cli::gate(&rest),
            "md" => crate::cli::md(&rest),
            "agent" | "flow" | "job" | "loop" | "ai" => {
                let argv: Vec<String> = std::iter::once(head.to_string()).chain(rest).collect();
                crate::cli::ai(&argv)
            }
            other => return Err(format!("no command `@{other}` — this world runs the @-command surface")),
        };
        self.code = Some(code);
        self.last = line.to_string();
        Ok(())
    }

    /// Lay down a finished flow run, through the same writer the engine uses.
    ///
    /// The reading verbs — `runs`, `show`, `nodes`, `node`, `log`, `retry`, `resume` — need
    /// a record to read, and every node of every shipped flow is an agent, so producing one
    /// the honest way would need a model. Writing the record instead keeps the thing under
    /// test where it belongs: the commands, not the engine that fills this in. `nodes` is
    /// `"<id>:<state>"` per entry, so a scenario can say which parts did not finish and
    /// then prove `resume` picked exactly those.
    fn record_flow(&mut self, step: &Toml, id: &str) -> Result<(), String> {
        let flow = world::text(step, "flow").unwrap_or_else(|| "review".into());
        let status = world::text(step, "status").unwrap_or_else(|| "done".into());
        let nodes = world::list(step, "nodes").unwrap_or_default();
        let mut recorded = Vec::new();
        for (i, spec) in nodes.iter().enumerate() {
            let (node, state) = spec.split_once(':').unwrap_or((spec.as_str(), "done"));
            let state = crate::flowruns::NodeState::read(state);
            // A node that never ran produced nothing, and a real run writes nothing for it.
            // Giving every node an output regardless would hand the readers a record no run
            // can produce, and the "there is nothing to show, and here is why" path — which
            // is most of what you meet on a run that broke — would never be reached.
            let ran = matches!(state, crate::flowruns::NodeState::Done | crate::flowruns::NodeState::Failed);
            recorded.push(crate::flowruns::NodeRun {
                id: node.to_string(),
                state,
                agent: "reviewer".into(),
                model: if ran { "a-model".into() } else { String::new() },
                input_tokens: if ran { 1000 + i as u64 } else { 0 },
                output_tokens: if ran { 100 + i as u64 } else { 0 },
                attempts: if ran { 1 } else { 0 },
                ms: if ran { 250 } else { 0 },
                output: if ran { format!("what {node} concluded") } else { String::new() },
                ..Default::default()
            });
            // The full text lives in its own file — that is what `@flow log` reads, and a
            // record without it would let `log` pass by finding nothing to print.
            if ran {
                crate::flowruns::write_node(id, node, "the prompt", &format!("what {node} concluded, in full"));
            }
        }
        let run = crate::flowruns::Run {
            id: id.to_string(),
            flow,
            input: world::text(step, "input").unwrap_or_else(|| "a change worth reviewing".into()),
            status,
            cwd: self.home.display().to_string(),
            started: 1_700_000_000,
            finished: Some(1_700_000_060),
            pid: 0,
            timeout: 1800,
            budget: None,
            concurrency: 4,
            nodes: recorded,
        };
        crate::flowruns::write(id, &run);
        Ok(())
    }

    /// The same, for a loop run — `@loop show|log|list|clear` read one of these.
    fn record_loop(&mut self, step: &Toml, id: &str) -> Result<(), String> {
        let run = crate::loops::Run {
            id: id.to_string(),
            goal: world::text(step, "goal").unwrap_or_else(|| "make the tests pass".into()),
            agent: "coder".into(),
            status: world::text(step, "status").unwrap_or_else(|| "done".into()),
            verifier: crate::loops::Verifier::Check {
                command: "cargo test".into(),
                source: crate::loops::Source::Explicit,
            },
            bounds: crate::loops::Bounds { max: 8, budget: None, timeout: 1800 },
            cwd: self.home.display().to_string(),
            started: 1_700_000_000,
            finished: Some(1_700_000_060),
            pid: 0,
            progress: crate::loops::Progress {
                iterations: 2,
                input_tokens: 2000,
                output_tokens: 200,
                tools: 3,
                feedback: "the verifier is happy".into(),
                ..Default::default()
            },
        };
        crate::loops::write_iteration(id, 8, 1, "the first attempt", "still failing");
        crate::loops::write_iteration(id, 8, 2, "the second attempt", "the verifier is happy");
        crate::loops::write(id, &run);
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
