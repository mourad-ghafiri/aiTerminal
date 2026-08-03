//! Configuration and profiles — what a file says, and what a profile changes.
//!
//! The overlay is the part that bites people: a profile layers on top of the global
//! file, and *how* it layers differs per section. Scalars override in place, the model
//! pool is replaced wholesale, and keybindings and guard rules append. A scenario
//! states which, so the rule cannot drift.
//!
//! Entirely in memory: `Config::from_toml` + `apply_toml` never touch disk.

use corelib::wire::Toml;

use super::super::world::{self, World};
use crate::config::Config;

pub struct ConfigWorld {
    config: Config,
    /// The result of the most recent `set` — the line-surgical write-back.
    edited: String,
}

pub fn build(_setup: &Toml) -> Result<Box<dyn World>, String> {
    Ok(Box::new(ConfigWorld { config: Config::default(), edited: String::new() }))
}

impl World for ConfigWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // ── what the files say ───────────────────────────────────────────────
        if let Some(lines) = world::list(step, "config") {
            self.config = Config::from_toml(&lines.join("\n"));
            return Ok(());
        }
        if let Some(lines) = world::list(step, "profile") {
            // Exactly what `Config::load` does after reading the global file.
            self.config.apply_toml(&lines.join("\n"));
            return Ok(());
        }
        if world::flag(step, "shipped_defaults") == Some(true) {
            self.config = Config::from_toml(crate::config::DEFAULT_CONFIG);
            return Ok(());
        }

        // ── editing a file in place ──────────────────────────────────────────
        if let Some(lines) = world::list(step, "edit") {
            let section = world::text(step, "section").ok_or("edit needs a `section`")?;
            let key = world::text(step, "key").ok_or("edit needs a `key`")?;
            let value = world::text(step, "value").ok_or("edit needs a `value`")?;
            self.edited = crate::gui::persist::upsert_line(&lines.join("\n"), &section, &key, &value);
            return Ok(());
        }

        // ── what must be true ────────────────────────────────────────────────
        if let Some(want) = world::text(step, "expect") {
            let field = world::text(step, "field").ok_or("expect needs a `field`")?;
            let got = self.field(&field)?;
            return world::expect_eq(&got, &want, &format!("`{field}`"));
        }
        if world::flag(step, "expect_defaults") == Some(true) {
            if self.config != Config::default() {
                return Err("the parsed config differs from the code defaults".into());
            }
            return Ok(());
        }
        if let Some(want) = world::list(step, "expect_edited") {
            let got: Vec<String> = self.edited.lines().map(str::to_string).collect();
            return world::expect_lines(&got, &want, "the edited file");
        }

        Err(world::unknown_verb(step))
    }
}

impl ConfigWorld {
    /// A named field, rendered as text so a scenario reads naturally.
    fn field(&self, name: &str) -> Result<String, String> {
        let c = &self.config;
        Ok(match name {
            "theme" => c.theme.clone(),
            "locale" => c.locale.clone(),
            "font_family" => c.font_family.clone(),
            "font_size" => c.font_size.to_string(),
            "cursor_style" => c.cursor_style.clone(),
            "zoom" => c.zoom.to_string(),
            "tab_bar" => c.tab_bar.clone(),
            "shell" => c.shell.clone(),
            "scrollback" => c.scrollback.to_string(),
            "ai_strategy" => c.ai_strategy.clone(),
            "ai_share_terminal_context" => c.ai_share_terminal_context.to_string(),
            "ai_memory" => c.ai_memory.to_string(),
            "ai_show_reasoning" => c.ai_show_reasoning.to_string(),
            "ai_command_mode" => c.ai_command_mode.clone(),
            "ai_network" => c.ai_network.to_string(),
            "ai_budget" => c.ai_budget.map(|b| b.to_string()).unwrap_or_else(|| "none".into()),
            "ai_models" => c.ai_pool.iter().map(|m| m.id.clone()).collect::<Vec<_>>().join(","),
            "ai_weights" => c.ai_pool.iter().map(|m| m.weight.to_string()).collect::<Vec<_>>().join(","),
            "ai_keys" => c
                .ai_pool
                .iter()
                .map(|m| m.api_key.clone().unwrap_or_else(|| "-".into()))
                .collect::<Vec<_>>()
                .join(","),
            "plugins_enabled" => c.plugins_enabled.to_string(),
            "plugins_disabled" => c.plugins_disabled.join(","),
            "shell_integration" => c.shell_integration.to_string(),
            "log_level" => c.log_level.clone(),
            "log_retention_days" => c.log_retention_days.to_string(),
            "guard_commands" => c.guard.commands.iter().map(|r| r.pattern.clone()).collect::<Vec<_>>().join(","),
            "guard_command_rules" => c.guard.commands.iter().map(|r| format!("{:?}", r.rule).to_lowercase()).collect::<Vec<_>>().join(","),
            "guard_paths" => c.guard.paths.iter().map(|r| r.pattern.clone()).collect::<Vec<_>>().join(","),
            "guard_path_rules" => c.guard.paths.iter().map(|r| format!("{:?}", r.rule).to_lowercase()).collect::<Vec<_>>().join(","),
            "guard_secrets" => c.guard.secrets.iter().map(|r| r.pattern.clone()).collect::<Vec<_>>().join(","),
            "guard_secret_scopes" => c.guard.secrets.iter().map(|r| format!("{:?}", r.scope).to_lowercase()).collect::<Vec<_>>().join(","),
            "keybindings" => c.keybindings.iter().map(|(k, a)| format!("{k}={a}")).collect::<Vec<_>>().join(","),
            "gates_enabled" => c.gates_enabled.to_string(),
            "gates_attach" => c.gates_attach.to_string(),
            "gates_plain_text" => c.gates_plain_text.clone(),
            "gates_screenshot" => c.gates_screenshot.clone(),
            "gates_require_pairing" => c.gates_require_pairing.to_string(),
            "gate_channels" => c.gates.iter().map(|g| g.channel.clone()).collect::<Vec<_>>().join(","),
            "gate_tokens" => c.gates.iter().map(|g| g.token.clone()).collect::<Vec<_>>().join(","),
            "gate_allow" => c.gates.iter().flat_map(|g| g.allow.clone()).collect::<Vec<_>>().join(","),
            other => return Err(format!("no such config field: {other:?}")),
        })
    }
}
