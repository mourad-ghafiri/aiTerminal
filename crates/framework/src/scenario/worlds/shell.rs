//! Shell integration — the text that gets sourced into your shell.
//!
//! Pure: a plugin set goes in, the generated init script comes out. Nothing is written
//! to disk and no shell is started; a scenario asserts what would be sourced.

use corelib::wire::Toml;

use super::super::world::{self, World};
use super::plugin_step;
use crate::plugin::PluginRegistry;
use crate::shell::Integration;

pub struct ShellWorld {
    registry: PluginRegistry,
    /// The generated init text, once `generate` has run.
    script: String,
}

pub fn build(_setup: &Toml) -> Result<Box<dyn World>, String> {
    Ok(Box::new(ShellWorld { registry: PluginRegistry::new(), script: String::new() }))
}

impl World for ShellWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        if let Some(done) = plugin_step::install(&mut self.registry, step) {
            return done;
        }
        if let Some(dialect) = world::text(step, "generate") {
            let bash = dialect == "bash";
            let ctx = Integration {
                aliases: self.registry.aliases(),
                abbrs: self.registry.abbreviations(),
                completions: self.registry.completions(),
                snippets: self.registry.shell_snippets(bash),
            };
            self.script = match dialect.as_str() {
                "zsh" => crate::shell::zsh_integration(&ctx),
                "bash" => crate::shell::bash_integration(&ctx),
                other => return Err(format!("unknown shell dialect {other:?}")),
            };
            return Ok(());
        }

        if let Some(want) = world::list(step, "expect_contains") {
            return world::expect_contains(&self.script, &want, "the generated init script");
        }
        if let Some(bad) = world::list(step, "expect_not_contains") {
            return world::expect_missing(&self.script, &bad, "the generated init script");
        }
        if world::flag(step, "expect_valid_syntax") == Some(true) {
            return self.check_syntax(step);
        }

        Err(world::unknown_verb(step))
    }
}

impl ShellWorld {
    /// Ask the shell itself whether the generated script parses.
    ///
    /// This is the one place a scenario runs anything, and it is `-n`: the shell parses
    /// the file and exits without executing a single command. Worth it, because a
    /// generated script with a quoting slip breaks every new pane, and no amount of
    /// substring matching would catch it.
    fn check_syntax(&self, step: &Toml) -> Result<(), String> {
        let dialect = world::text(step, "dialect").unwrap_or_else(|| "zsh".into());
        let dir = std::env::temp_dir().join(format!("tt-scenario-shell-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("integration.{dialect}"));
        std::fs::write(&path, &self.script).map_err(|e| e.to_string())?;

        let out = std::process::Command::new(&dialect).arg("-n").arg(&path).output();
        let _ = std::fs::remove_file(&path);
        match out {
            // Not installed on this machine — a missing shell is not a product failure.
            Err(_) => Ok(()),
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => Err(format!(
                "{dialect} cannot parse the generated script: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
        }
    }
}
