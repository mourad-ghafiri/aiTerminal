//! The `task.*` native family — the agent's **delegation** seam. `task.run` lets the
//! orchestrating agent spawn a named sub-agent (e.g. `explorer`, `reviewer`, `tester`)
//! and fold its report back into its own loop — Claude-Code's Agent/Task tool, on our
//! pure [`run_agent`](crate::ai::run_agent).
//!
//! The method is REGISTERED here only for the tool catalog (`describe`/`danger`/the
//! allow-list) — its `invoke` deliberately errors, because execution is **intercepted
//! upstream** in the GUI's `GatedRunner`, the only place that owns the sub-agent
//! machinery (the model settings, the workspace, the cancel token, the depth cap).
//! Delegated sub-agents run **non-interactive + safe-list-gated**, so a delegate can
//! never run an unapproved risky command — the security model holds across the seam.

use corelib::wire::Json;

use super::host::Host;
use super::object::{MethodSpec, NativeObject};
use super::CapCtx;

pub struct TaskObj;

const SPECS: &[MethodSpec] = &[MethodSpec {
    method: "task.run",
    describe: "Delegate a sub-task to a named sub-agent and get its report back. Args: `agent` (e.g. explorer, reviewer, tester) + `prompt`; OR `tasks` = a JSON array of `{agent, prompt}` to fan out IN PARALLEL. Sub-agents are read/safe-only — use them to map the codebase, review a diff, or run tests, then synthesize.",
}];

impl NativeObject for TaskObj {
    fn family(&self) -> &'static str {
        "task"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    /// Never reached through the plain capability path: `task.run` is intercepted in the
    /// agent loop's gated runner (where the sub-agent machinery lives). Calling it outside
    /// an agent context is an error, not a side effect.
    fn invoke(&self, _method: &str, _args: &[(String, String)], _ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        Err("task.run runs only inside an agent loop (handled by the harness orchestrator)".into())
    }
}
