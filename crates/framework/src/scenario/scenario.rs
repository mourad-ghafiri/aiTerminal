//! A scenario: a real user journey, written as data.
//!
//! Scenarios live in `scenarios/<feature>/*.toml` and describe what a *person* and a
//! *program* do, not which bytes go where. That keeps them readable by someone who
//! wants to know how the product behaves, and keeps them honest — a scenario cannot
//! quietly reach past the public surface to make itself pass.

use corelib::wire::Toml;

use super::step::Step;

/// How the world starts before the first step.
#[derive(Clone, Debug)]
pub struct Setup {
    /// Begin already paired with chat 7 (the overwhelmingly common case, and not worth
    /// re-typing the handshake in every file).
    pub paired: bool,
    /// Chat ids pre-authorized in config — the `allow` list, which skips pairing.
    pub allow: Vec<String>,
    /// `[gates] plain_text = "run"`.
    pub plain_runs: bool,
    /// `[gates] attach`.
    pub attach: bool,
    /// Guard patterns.
    pub deny: Vec<String>,
    pub confirm: Vec<String>,
    /// Redaction patterns, applied to everything leaving the machine.
    pub redact: Vec<String>,
    /// Terminal width for the mirror.
    pub cols: u16,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            paired: false,
            allow: Vec::new(),
            plain_runs: true,
            attach: true,
            deny: Vec::new(),
            confirm: Vec::new(),
            redact: Vec::new(),
            cols: 80,
        }
    }
}

/// One parsed scenario file.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub name: String,
    pub tags: Vec<String>,
    pub setup: Setup,
    pub steps: Vec<Step>,
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

        let mut setup = Setup::default();
        if let Some(s) = doc.get("setup") {
            let flag = |k: &str, d: bool| s.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
            let list = |k: &str| {
                s.get(k)
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>())
                    .unwrap_or_default()
            };
            setup.paired = flag("paired", false);
            setup.plain_runs = flag("plain_text_runs", true);
            setup.attach = flag("attach", true);
            setup.allow = list("allow");
            setup.deny = list("deny");
            setup.confirm = list("confirm");
            setup.redact = list("redact");
            if let Some(c) = s.get("cols").and_then(|v| v.as_int()) {
                setup.cols = c.clamp(20, 400) as u16;
            }
        }

        let raw = doc.get("step").and_then(|v| v.as_array()).ok_or_else(|| format!("{file}: no `[[step]]` tables"))?;
        let mut steps = Vec::with_capacity(raw.len());
        for (i, t) in raw.iter().enumerate() {
            steps.push(Step::parse(t).map_err(|e| format!("{file}: step {}: {e}", i + 1))?);
        }
        Ok(Scenario { name, tags, setup, steps, file: file.to_string() })
    }
}
