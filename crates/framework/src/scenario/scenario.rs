//! A scenario: a real user journey, written as data.
//!
//! Scenarios live in `scenarios/<feature>/*.toml` and describe what a *person* and a
//! *program* do, not which bytes go where. That keeps them readable by someone who wants
//! to know how the product behaves, and keeps them honest — a scenario cannot quietly
//! reach past the public surface to make itself pass.
//!
//! The engine keeps `setup` and each `step` as raw TOML: only the feature's [`World`]
//! knows what its verbs mean.
//!
//! [`World`]: super::world::World

use corelib::wire::Toml;

/// One parsed scenario file.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub name: String,
    /// Free-form labels, for a human scanning the folder.
    #[allow(dead_code)]
    pub tags: Vec<String>,
    pub setup: Toml,
    pub steps: Vec<Toml>,
    /// The file it came from, for the failure report.
    pub file: String,
}

impl Scenario {
    pub fn parse(file: &str, text: &str) -> Result<Scenario, String> {
        let doc = Toml::parse(text).map_err(|e| format!("{file}: {e}"))?;
        let name = doc
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("{file}: a scenario needs a `name`"))?
            .to_string();
        let tags = doc
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let setup = doc.get("setup").cloned().unwrap_or_else(|| Toml::Table(Vec::new()));
        let steps: Vec<Toml> = doc
            .get("step")
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("{file}: no `[[step]]` tables"))?
            .to_vec();
        if steps.is_empty() {
            return Err(format!("{file}: no `[[step]]` tables"));
        }
        Ok(Scenario { name, tags, setup, steps, file: file.to_string() })
    }
}
