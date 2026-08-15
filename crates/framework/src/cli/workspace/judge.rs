//! Auto mode's approver: the AI judge answers the guard's `Confirm` first, the
//! human whenever it declines.
//!
//! A Decorator over [`crate::guard::Approver`] — the guard itself is untouched.
//! `Deny` never reaches an approver at all, `Allow` never asks one; the ONLY
//! thing this changes is who answers the guard's confirm-tier question. The
//! judge can remove an interruption; it can never add a permission. And nothing
//! is ever decided silently: every verdict — either way — is said out loud in
//! the conversation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use platform::transport::Transport;

/// The decorated approver. Disabled (plan/build), it is a transparent pass-through
/// to the human; enabled (auto), the judge speaks first.
pub(crate) struct Judged<T: Transport> {
    /// The judge's own client — sharing the sitting's transport, so the scenario
    /// world scripts verdicts exactly like any other completion.
    client: crate::ai::Client<T>,
    /// Flipped by the mode switch. Shared, because the approver is installed once
    /// and the mode changes many times.
    enabled: Arc<AtomicBool>,
    /// The human — the workspace's ask panel or the terminal's y/N.
    inner: Arc<dyn crate::guard::Approver>,
    /// Where verdicts are said; `None` says them on stderr (headless).
    events: Option<Sender<super::ui::Event>>,
    /// The workspace root, named in the judge's trust boundary.
    root: String,
}

impl<T: Transport> Judged<T> {
    pub(crate) fn new(
        client: crate::ai::Client<T>,
        enabled: Arc<AtomicBool>,
        inner: Arc<dyn crate::guard::Approver>,
        events: Option<Sender<super::ui::Event>>,
        root: String,
    ) -> Judged<T> {
        Judged { client, enabled, inner, events, root }
    }

    fn say(&self, line: String) {
        match &self.events {
            Some(events) => {
                let _ = events.send(super::ui::Event::Append(line));
            }
            None => eprintln!("{line}"),
        }
    }
}

impl<T: Transport> crate::guard::Approver for Judged<T> {
    fn approve(&self, act: &str, reason: &str) -> bool {
        if !self.enabled.load(Ordering::Relaxed) {
            return self.inner.approve(act, reason);
        }
        let (dim, r) = (crate::cli::style::muted(), crate::cli::style::reset());
        match crate::ai::judge::judge_with(&self.client, act, reason, &self.root) {
            Some(v) if v.safe => {
                self.say(format!("{dim}\u{26a1} auto-approved {act} \u{2014} {}{r}", v.reason));
                true
            }
            verdict => {
                // Unsafe, undecodable, or no reply at all — every one of them is
                // "ask the human". The keyboard stays sovereign.
                let why = verdict.map(|v| v.reason).unwrap_or_else(|| "no verdict arrived".into());
                self.say(format!("{dim}the judge declined ({why}) \u{2014} asking you{r}"));
                self.inner.approve(act, reason)
            }
        }
    }
}

#[cfg(test)]
mod tests;
