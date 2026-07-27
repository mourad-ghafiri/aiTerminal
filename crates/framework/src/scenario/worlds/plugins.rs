//! Plugins — a `plugin.toml` is pure data, and this is what it composes into.
//!
//! Deliberately hermetic: it never calls `evaluate`, which spawns a process to read the
//! clock. A scenario hands the status bar a variable bag directly, which covers the
//! whole composition layer — templates, `when` truthiness, alignment, trust — without
//! anything being executed.

use corelib::wire::Toml;

use super::super::world::{self, World};
use super::plugin_step;
use crate::plugin::{PluginRegistry, Vars};

pub struct PluginWorld {
    registry: PluginRegistry,
    vars: Vars,
}

pub fn build(_setup: &Toml) -> Result<Box<dyn World>, String> {
    Ok(Box::new(PluginWorld { registry: PluginRegistry::new(), vars: Vars::default() }))
}

impl World for PluginWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // ── installing plugins ───────────────────────────────────────────────
        if let Some(done) = plugin_step::install(&mut self.registry, step) {
            return done;
        }
        if let Some(name) = world::text(step, "disable") {
            if !self.registry.set_enabled(&name, false) {
                return Err(format!("no plugin named {name:?} to disable"));
            }
            return Ok(());
        }

        // ── the variables the status bar reads ───────────────────────────────
        if let Some(pairs) = world::list(step, "vars") {
            for p in &pairs {
                let (k, v) = p.split_once('=').ok_or_else(|| format!("vars entry {p:?} needs key=value"))?;
                self.vars.set(k.trim(), v.trim());
            }
            return Ok(());
        }

        // ── what must be true ────────────────────────────────────────────────
        if let Some(want) = world::list(step, "expect_aliases") {
            return world::expect_lines(&pairs(self.registry.aliases()), &want, "the aliases");
        }
        if let Some(want) = world::list(step, "expect_abbreviations") {
            return world::expect_lines(&pairs(self.registry.abbreviations()), &want, "the abbreviations");
        }
        if let Some(want) = world::list(step, "expect_completions") {
            let got: Vec<String> = self.registry.completions().iter().map(|c| c.command.clone()).collect();
            return world::expect_lines(&got, &want, "the completions");
        }
        if let Some(want) = world::list(step, "expect_keybindings") {
            let got: Vec<String> =
                self.registry.keybindings().iter().map(|k| format!("{}={}", k.key, k.action)).collect();
            return world::expect_lines(&got, &want, "the keybindings");
        }
        if let Some(want) = world::list(step, "expect_denied") {
            let got: Vec<String> = self.registry.deny_commands().iter().map(|d| d.pattern.clone()).collect();
            return world::expect_lines(&got, &want, "the deny rules");
        }
        if let Some(want) = world::list(step, "expect_redactions") {
            let got: Vec<String> = self.registry.redact_rules().iter().map(|r| r.pattern.clone()).collect();
            return world::expect_lines(&got, &want, "the redaction rules");
        }
        if let Some(want) = world::list(step, "expect_snippets") {
            let bash = world::flag(step, "bash").unwrap_or(false);
            let got: Vec<String> = self.registry.shell_snippets(bash).iter().map(|(n, _)| n.clone()).collect();
            return world::expect_lines(&got, &want, "the shell snippets");
        }
        if let Some(want) = world::list(step, "expect_status_left") {
            let got: Vec<String> =
                self.registry.status_line(&self.vars).left.iter().map(|s| s.text.clone()).collect();
            return world::expect_lines(&got, &want, "the left status segments");
        }
        if let Some(want) = world::list(step, "expect_status_right") {
            let got: Vec<String> =
                self.registry.status_line(&self.vars).right.iter().map(|s| s.text.clone()).collect();
            return world::expect_lines(&got, &want, "the right status segments");
        }
        if let Some(want) = world::list(step, "expect_names") {
            return world::expect_lines(&self.registry.names(), &want, "the loaded plugins");
        }

        Err(world::unknown_verb(step))
    }
}

fn pairs(v: Vec<(String, String)>) -> Vec<String> {
    v.into_iter().map(|(a, b)| format!("{a}={b}")).collect()
}
