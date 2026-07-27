//! The command guard and the redactor — what may run, and what may leave.
//!
//! Both are pure policy over text, so a scenario drives them exactly as the product
//! does. Nothing here executes anything: a destructive command is a *string* handed to
//! `check_command`, and the assertion is that the verdict is `deny`.

use corelib::wire::Toml;

use super::super::world::{self, World};
use crate::security::{Policy, RedactScope, Verdict};

pub struct SecurityWorld {
    policy: Policy,
    /// The verdict from the most recent `check`.
    verdict: Option<Verdict>,
    /// The result of the most recent `redact`.
    redacted: String,
}

pub fn build(setup: &Toml) -> Result<Box<dyn World>, String> {
    let mut policy = Policy::new();
    let add = |p: &mut Policy, key: &str, f: fn(&mut Policy, &str) -> Result<(), String>| -> Result<(), String> {
        for pat in world::list(setup, key).unwrap_or_default() {
            f(p, &pat).map_err(|e| format!("{key} pattern {pat:?}: {e}"))?;
        }
        Ok(())
    };
    add(&mut policy, "allow", |p, s| p.add_allow(s))?;
    add(&mut policy, "deny", |p, s| p.add_deny(s))?;
    add(&mut policy, "confirm", |p, s| p.add_confirm(s))?;
    add(&mut policy, "safe", |p, s| p.add_safe(s))?;

    // `redact = ["pattern"]` masks in every scope; the scoped forms mask only one.
    for (key, scope) in
        [("redact", RedactScope::All), ("redact_ai", RedactScope::Ai), ("redact_terminal", RedactScope::Terminal)]
    {
        for pat in world::list(setup, key).unwrap_or_default() {
            policy
                .add_redaction(&pat, "«redacted»", scope, false)
                .map_err(|e| format!("{key} pattern {pat:?}: {e}"))?;
        }
    }
    for lit in world::list(setup, "redact_literal").unwrap_or_default() {
        policy
            .add_redaction(&lit, "«redacted»", RedactScope::All, true)
            .map_err(|e| format!("redact_literal {lit:?}: {e}"))?;
    }
    Ok(Box::new(SecurityWorld { policy, verdict: None, redacted: String::new() }))
}

impl World for SecurityWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        if let Some(cmd) = world::text(step, "check") {
            self.verdict = Some(self.policy.check_command(&cmd));
            return Ok(());
        }
        if let Some(text) = world::text(step, "redact") {
            let scope = match world::text(step, "scope").unwrap_or_else(|| "ai".into()).as_str() {
                "ai" => RedactScope::Ai,
                "terminal" => RedactScope::Terminal,
                "all" => RedactScope::All,
                other => return Err(format!("unknown scope {other:?}")),
            };
            self.redacted = self.policy.redact(&text, scope);
            return Ok(());
        }

        if let Some(want) = world::text(step, "expect_verdict") {
            let got = match self.verdict.as_ref().ok_or("nothing has been checked yet")? {
                Verdict::Allow => "allow".to_string(),
                Verdict::Confirm { .. } => "confirm".to_string(),
                Verdict::Deny { .. } => "deny".to_string(),
            };
            return world::expect_eq(&got, &want, "the verdict");
        }
        if let Some(want) = world::list(step, "expect_reason") {
            let reason = match self.verdict.as_ref().ok_or("nothing has been checked yet")? {
                Verdict::Confirm { reason } | Verdict::Deny { reason } => reason.clone(),
                Verdict::Allow => String::new(),
            };
            return world::expect_contains(&reason, &want, "the verdict's reason");
        }
        if let Some(want) = world::flag(step, "expect_safe") {
            let cmd = world::text(step, "cmd").ok_or("expect_safe needs a `cmd`")?;
            let got = self.policy.is_safe_command(&cmd);
            if got != want {
                return Err(format!("{cmd:?} auto-safe: expected {want}, got {got}"));
            }
            return Ok(());
        }
        if let Some(want) = world::text(step, "expect_text") {
            return world::expect_eq(&self.redacted, &want, "the redacted text");
        }
        if let Some(bad) = world::list(step, "expect_not_text") {
            return world::expect_missing(&self.redacted, &bad, "the redacted text");
        }
        if let Some(want) = world::list(step, "expect_kept") {
            return world::expect_contains(&self.redacted, &want, "the redacted text");
        }

        Err(world::unknown_verb(step))
    }
}
