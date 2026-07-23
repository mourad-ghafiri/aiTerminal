//! The **Host seam** — a placeholder trait every native object's `invoke` takes by
//! `&mut dyn Host`. With the app/view layer gone, no family needs live window
//! state: all tools are pure over [`CapCtx`](super::CapCtx). The seam is kept so a
//! future embedder can thread live capabilities through without re-plumbing every
//! object signature; [`NullHost`] is the only implementation today.

/// Live host capabilities (none currently required).
pub trait Host {}

/// The no-op host used by the agent tool path and tests.
pub struct NullHost;

impl Host for NullHost {}
