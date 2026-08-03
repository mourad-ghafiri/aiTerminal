// ===== @flow — a workflow declared as a graph ================================
//
// Graph engineering in one sentence: stop writing a chain of prompts and start declaring the
// graph the work actually is. Six pieces make that real here:
//
//   1. A DAG, NOT A LINE.  `needs` is a dependency, so nodes that need nothing from each other
//      run AT THE SAME TIME. Three reviews cost one round of wall clock instead of three.
//   2. ROUTING ON THE EDGE.  `when` is data this tool parses, not an instruction a model
//      interprets — because an agent asked to decide what happens next decides differently
//      each time, and nothing about the run can be audited afterwards.
//   3. A DETERMINISTIC BACKBONE.  A `run` node is a command through the same guard everything
//      else uses, and costs no tokens. The model is spent only where judgement is needed.
//   4. BOUNDED CYCLES.  `goto` points one edge backwards with a `max`, so "test, fix, test
//      again" is a flow rather than something you sit and supervise.
//   5. PROVED BEFORE IT SPENDS.  Everything checkable without a model is checked first
//      (`flow::verify`): a dangling edge, a reference to a node that does not run first, an
//      agent that is not installed, a command the guard refuses. Exit 2, zero tokens.
//   6. STATE THAT SURVIVES.  Every node's result is written to `ai/flow-runs/<id>/` the moment
//      it lands, so `@flow show` reads the shape, `@flow log` reads a node, and `@flow resume`
//      runs only what did not complete — the fix for the old chain's all-or-nothing failure.

/// Load flow `name` from `~/.aiTerminal/ai/flows/<name>.toml`.
pub(crate) mod args;
pub(crate) mod exec;
pub(crate) mod run;
pub(crate) mod show;

use crate::cli::style::{accent, muted, reset};

pub(crate) fn load_flow(name: &str) -> Result<crate::flow::Flow, String> {
    if !crate::flow::tmpl::id_ok(name) {
        return Err(format!("'{name}' is not a flow name"));
    }
    let path = crate::config::Config::flows_dir().join(format!("{name}.toml"));
    match std::fs::read_to_string(&path) {
        Ok(text) => crate::flow::parse(name, &text),
        Err(_) => {
            let installed = flow_names();
            let refs: Vec<&str> = installed.iter().map(String::as_str).collect();
            // A typo must never quietly become a different flow: this used to fall
            // through to the `implement` pipeline, so a misspelling ran a
            // code-editing graph over the repository.
            Err(format!(
                "no flow '{name}'{}{}",
                crate::flow::verify::nearest(name, &refs),
                if installed.is_empty() {
                    format!(" — add one to {}", crate::config::Config::flows_dir().display())
                } else {
                    String::new()
                }
            ))
        }
    }
}

/// Every installed flow's name, sorted.
pub(crate) fn flow_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(crate::config::Config::flows_dir())
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
                .filter_map(|p| p.file_stem()?.to_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// What the verifier needs from outside itself: the installed agents and the guard.
struct FlowWorld {
    guard: std::sync::Arc<crate::guard::Guard>,
    agents: Vec<crate::ai::defs::Agent>,
}

/// The world the verifier checks against — the installed agents and the command guard.
///
/// Exposed because building a graph verifies it too, and a graph built against a
/// different world than the one that will run it is a graph that passes here and is
/// refused there.
pub(crate) fn world() -> impl crate::flow::verify::World {
    FlowWorld::build()
}

impl FlowWorld {
    fn build() -> FlowWorld {
        let cfg = crate::config::Config::load();
        let registry = crate::plugin::load_registry(&cfg);
        FlowWorld {
            guard: std::sync::Arc::new(crate::guard::build(&cfg, &registry)),
            agents: crate::ai::defs::load_agents(&crate::config::Config::agents_dir()),
        }
    }
}

impl crate::flow::verify::World for FlowWorld {
    fn agent_tools(&self, name: &str) -> Option<Vec<String>> {
        self.agents.iter().find(|a| a.name == name).map(|a| a.tools.clone())
    }
    fn guard(&self, command: &str) -> crate::flow::verify::Guard {
        use crate::flow::verify::Guard;
        match self.guard.judge(crate::guard::Act::Run(&command)) {
            crate::guard::Decision::Allow => Guard::Allow,
            crate::guard::Decision::Confirm { reason } => Guard::Confirm(reason),
            crate::guard::Decision::Deny { reason } => Guard::Deny(reason),
        }
    }
    fn agent_names(&self) -> Vec<String> {
        self.agents.iter().map(|a| a.name.clone()).collect()
    }
}

/// Load and verify in one step — the gate every path that spends money goes through.
pub(crate) fn checked_flow(name: &str) -> Result<(crate::flow::Flow, crate::flow::verify::Report), String> {
    verified(load_flow(name)?)
}

/// Verify a flow however it was come by — read from `ai/flows/`, or built for a goal and
/// never written there at all. One gate, so a graph nobody wrote by hand is held to
/// exactly the checks one written by hand is.
pub(crate) fn verified(flow: crate::flow::Flow) -> Result<(crate::flow::Flow, crate::flow::verify::Report), String> {
    let report = crate::flow::verify::verify(&flow, &FlowWorld::build());
    Ok((flow, report))
}

/// The graph a RUN was made with.
///
/// Two places it can come from and one function that knows it: the record's own
/// `flow.toml` when the graph was built for that run, and `ai/flows/<name>.toml` when it
/// was a flow somebody installed. Every reader — `show`, `nodes`, `node`, `log`, `retry`,
/// `watch`, `resume` — comes through here, which is what lets all of them work on a built
/// graph without any of them knowing there is such a thing.
pub(crate) fn run_graph(run: &crate::flowruns::Run) -> Result<crate::flow::Flow, String> {
    match run.own_graph() {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("run {}: its own graph cannot be read: {e}", run.id))?;
            crate::flow::parse(&run.flow, &text)
        }
        None => load_flow(&run.flow),
    }
}

/// Print a verification report. Errors first: they are why nothing ran.
pub(crate) fn print_report(name: &str, report: &crate::flow::verify::Report, nodes: usize) {
    let (dim, r) = (muted(), reset());
    for e in &report.errors {
        eprintln!("  {}\u{2717}{r} {e}", accent());
    }
    for w in &report.warnings {
        eprintln!("  {dim}\u{26a0}  {w}{r}");
    }
    if report.ok() && report.warnings.is_empty() {
        println!("  {dim}\u{2713} {name} \u{b7} {nodes} node(s) \u{b7} worst case {} agent run(s){r}", report.worst_case_runs);
    }
}
