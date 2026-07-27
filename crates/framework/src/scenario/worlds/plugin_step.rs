//! Installing a plugin from a scenario step.
//!
//! Shared by the plugin and shell worlds, which both need to say "here is a plugin"
//! before asserting anything — one owns what it composes into, the other what it
//! generates. The vocabulary must be identical in both, so it lives here.
//!
//! A step mirrors the on-disk folder layout: the manifest lines are `plugin.toml`, and
//! the optional `zsh` / `bash` keys are its `shell.zsh` / `shell.bash` siblings — which
//! is why they are not manifest fields, and why a scenario cannot smuggle shell code in
//! through the TOML.

use corelib::wire::Toml;

use super::super::world;
use crate::plugin::{Manifest, PluginRegistry};

/// Handle a plugin-install step, or return `None` if this step is not one.
///
/// Returning `Option<Result<..>>` keeps each world's `apply` a flat chain of verb
/// matches: `None` means "not mine, keep looking", and the world still owns its own
/// unknown-verb error.
pub fn install(registry: &mut PluginRegistry, step: &Toml) -> Option<Result<(), String>> {
    if let Some(lines) = world::list(step, "plugin") {
        return Some(parse(&lines, step).map(|m| registry.add_trusted(m)));
    }
    if let Some(lines) = world::list(step, "untrusted_plugin") {
        return Some(parse(&lines, step).map(|m| registry.add_untrusted(m)));
    }
    if let Some(lines) = world::list(step, "bad_plugin") {
        return Some(match Manifest::parse(&lines.join("\n")) {
            Err(_) => Ok(()),
            Ok(_) => Err("this manifest was expected to be rejected, but it parsed".into()),
        });
    }
    None
}

fn parse(lines: &[String], step: &Toml) -> Result<Manifest, String> {
    let mut m = Manifest::parse(&lines.join("\n")).map_err(|e| format!("this manifest does not parse: {e}"))?;
    m.shell_zsh = world::text(step, "zsh");
    m.shell_bash = world::text(step, "bash");
    Ok(m)
}
