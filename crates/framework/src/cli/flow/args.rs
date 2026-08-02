// ─────────────────────────────── the surface ───────────────────────────────

/// What an `ai flow …` invocation asks for.
use crate::cli::agentloop::args::flag_value;

#[derive(Debug, PartialEq)]
pub(crate) enum FlowCmd {
    /// Bare `@flow` — the installed flows.
    List,
    Help,
    /// Verify one flow, or every installed flow when no name is given.
    Check(Option<String>),
    /// Draw a flow's graph.
    Graph { name: String, view: Option<String> },
    /// Past runs.
    Runs,
    Clear,
    Show { id: String, view: Option<String> },
    /// Every node of a run, side by side.
    Nodes(String),
    /// One node, in full.
    Node { id: String, node: String },
    /// Follow a run that is still going.
    Watch { id: String, view: Option<String> },
    /// Run one node again, and everything that depended on it.
    Retry { id: String, node: String },
    Log { id: String, node: Option<String>, follow: bool },
    Resume(String),
    Run(Box<FlowSpec>),
}

/// Pull `--view <word>` (or `--view=<word>`) out of argv, leaving the rest.
///
/// Every `@flow` verb that draws something accepts it, so it is taken once here rather
/// than in each branch — and taking it here is also what stops its value being read as
/// a run id by the subcommands that take one positionally.
fn take_view(args: &[String]) -> Result<(Vec<String>, Option<String>), String> {
    let (mut rest, mut view) = (Vec::with_capacity(args.len()), None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--view" => {
                let v = it.next().ok_or("--view needs a word: graph or list")?;
                view = Some(view_word(v)?);
            }
            other if other.starts_with("--view=") => view = Some(view_word(&other["--view=".len()..])?),
            other => rest.push(other.to_string()),
        }
    }
    Ok((rest, view))
}

/// The two words a view can be. An unrecognised one is refused rather than quietly
/// falling back: a flag someone typed is a thing they meant.
pub(crate) fn view_word(v: &str) -> Result<String, String> {
    match v.trim().to_ascii_lowercase().as_str() {
        "graph" => Ok("graph".into()),
        "list" => Ok("list".into()),
        other => Err(format!("--view takes `graph` or `list`, not {other:?}")),
    }
}

/// `[]` → the last run · `[x]` → node `x` of the last run · `[a, b]` → node `b` of run
/// `a`. Naming the node alone is the common case, and a run id is never a bare word
/// anyone types twice.
fn id_and_node(plain: &[String]) -> (String, Option<String>) {
    match plain {
        [] => ("last".into(), None),
        [only] => ("last".into(), Some(only.clone())),
        [id, node, ..] => (id.clone(), Some(node.clone())),
    }
}

/// A flow to run.
#[derive(Debug, PartialEq, Default)]
pub(crate) struct FlowSpec {
    pub(crate) name: String,
    /// The text typed after the flow name — `{{input}}`.
    pub(crate) input: String,
    /// Bounds left unset fall back to the file's `[bounds]`, then `[flow]` config.
    pub(crate) timeout: Option<u64>,
    pub(crate) budget: Option<u64>,
    pub(crate) concurrency: Option<usize>,
    /// `--view graph|list`, overriding `[flow] view` for this command alone.
    pub(crate) view: Option<String>,
    pub(crate) bg: bool,
    pub(crate) dry_run: bool,
    /// Set on the detached child so it can stamp its job record on exit.
    pub(crate) job_record: Option<String>,
}

/// Read `ai flow …` argv.
///
/// Subcommands win over flow names, and a flow file named after one is refused by
/// the verifier — so `@flow show` is never ambiguous, and the ambiguity is reported
/// where it can be explained rather than resolved by a coin toss.
/// `installed` is passed in rather than read from disk, so the whole rule is a pure
/// function of (what was typed, what exists) — which is what lets the cases below be a
/// table in a test instead of a config directory in a fixture.
pub(crate) fn parse_flow_args(raw: &[String], installed: &[String]) -> Result<FlowCmd, String> {
    // `--view` is lifted out FIRST, because every subcommand accepts it and because a
    // flag's value must never be mistaken for a positional word: `@flow show --view
    // list` would otherwise read "list" as the run id.
    let (args, view) = take_view(raw)?;
    let args = args.as_slice();
    let word = |i: usize| args.get(i).filter(|a| !a.starts_with('-')).cloned();
    let plain = |from: usize| -> Vec<String> {
        args.get(from..).unwrap_or_default().iter().filter(|a| !a.starts_with('-')).cloned().collect()
    };
    let id_or_last = |from: usize| plain(from).first().cloned().unwrap_or_else(|| "last".into());
    match args.first().map(String::as_str) {
        None => return Ok(FlowCmd::List),
        Some("list") if args.len() == 1 => return Ok(FlowCmd::List),
        Some("help") | Some("--help") | Some("-h") => return Ok(FlowCmd::Help),
        Some("check") => return Ok(FlowCmd::Check(word(1))),
        Some("graph") | Some("draw") => {
            return match word(1) {
                Some(name) => Ok(FlowCmd::Graph { name, view }),
                None => Err("graph needs a flow name — try `@flow graph implement`".into()),
            }
        }
        Some("runs") if args.len() == 1 => return Ok(FlowCmd::Runs),
        Some("clear") if args.len() == 1 => return Ok(FlowCmd::Clear),
        Some("show") => return Ok(FlowCmd::Show { id: id_or_last(1), view }),
        Some("nodes") => return Ok(FlowCmd::Nodes(id_or_last(1))),
        Some("watch") => return Ok(FlowCmd::Watch { id: id_or_last(1), view }),
        Some("node") => {
            let (id, node) = id_and_node(&plain(1));
            return match node {
                Some(node) => Ok(FlowCmd::Node { id, node }),
                None => Err("node needs a node to look at — try `@flow node last verify`".into()),
            };
        }
        Some("retry") => {
            let (id, node) = id_and_node(&plain(1));
            return match node {
                Some(node) => Ok(FlowCmd::Retry { id, node }),
                None => Err("retry needs a node to run again — try `@flow retry last verify`".into()),
            };
        }
        Some("resume") | Some("continue") => return Ok(FlowCmd::Resume(id_or_last(1))),
        Some("log") | Some("logs") => {
            let follow = args.iter().any(|a| a == "-f" || a == "--follow");
            let plain = plain(1);
            return Ok(FlowCmd::Log {
                id: plain.first().cloned().unwrap_or_else(|| "last".into()),
                node: plain.get(1).cloned(),
                follow,
            });
        }
        _ => {}
    }
    let mut spec = FlowSpec { view, ..FlowSpec::default() };
    let mut words: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bg" => spec.bg = true,
            "--dry-run" | "--plan" => spec.dry_run = true,
            "--job-record" => spec.job_record = Some(flag_value(&mut it, "--job-record")?),
            "--timeout" => {
                let v = flag_value(&mut it, "--timeout")?;
                let secs = corelib::datetime::duration(&v)
                    .ok_or_else(|| format!("--timeout needs a duration like 30m or 90s, got {v:?}"))?;
                spec.timeout = Some(secs.max(30));
            }
            "--budget" => {
                let v = flag_value(&mut it, "--budget")?;
                spec.budget = Some(v.parse().map_err(|_| format!("--budget needs a token count, got {v:?}"))?);
            }
            "--concurrency" => {
                let v = flag_value(&mut it, "--concurrency")?;
                let n: usize = v.parse().map_err(|_| format!("--concurrency needs a whole number, got {v:?}"))?;
                spec.concurrency = Some(n.clamp(1, 16));
            }
            w => words.push(w.to_string()),
        }
    }
    let Some((name, rest)) = words.split_first() else {
        return Err("a flow needs a name or a goal — `@flow` on its own lists them".into());
    };
    match first_word(name, installed) {
        // `@flow document this project` — a flow you have, and the rest is its input.
        // One argument keeps its flag-looking words (`@flow ship "raise --max to 10"`);
        // several loose ones are a sentence to rejoin.
        First::Flow => {
            spec.name = name.clone();
            spec.input = match rest {
                [only] => only.clone(),
                many => many.join(" "),
            };
        }
        // `@flow revieew the parser` — the guard that has always been here. A typo must
        // never quietly become a different flow, and it must never become a GOAL either:
        // building and running a graph for a misspelling is the same footgun wearing a
        // newer coat.
        First::Typo(did_you_mean) => {
            return Err(format!("no flow '{name}'{did_you_mean}\n  or say what you want done and one will be built for it"))
        }
        // Anything else is a goal, however it was typed — `@flow explain this project`
        // and `@flow "explain this project"` are the same request, and only one of them
        // used to be understood.
        First::Goal => spec.input = std::iter::once(name).chain(rest).cloned().collect::<Vec<_>>().join(" "),
    }
    Ok(FlowCmd::Run(Box::new(spec)))
}

/// What the first word after `@flow` turns out to be.
enum First {
    /// A flow that is installed.
    Flow,
    /// Close enough to one that it is a misspelling, not a goal — carrying the
    /// suggestion, ready to print.
    Typo(String),
    /// Neither: the whole line is something to build a graph for.
    Goal,
}

/// Read the first word. `installed` is passed in so the rule is testable without a
/// config directory — which is also what makes the table of cases in the tests possible.
fn first_word(word: &str, installed: &[String]) -> First {
    if installed.iter().any(|n| n == word) {
        return First::Flow;
    }
    let refs: Vec<&str> = installed.iter().map(String::as_str).collect();
    match crate::flow::verify::nearest(word, &refs) {
        // `nearest` returns an empty string when nothing is close.
        s if s.trim().is_empty() => First::Goal,
        did_you_mean => First::Typo(did_you_mean),
    }
}

pub(crate) fn flow_usage() -> String {
    [
        "usage: @flow <name> \"<input>\"       run a flow",
        "       @flow … --bg | --dry-run     detach it (`@flow watch` attaches) | spend nothing",
        "       @flow … --timeout 30m --budget TOKENS --concurrency N",
        "       @flow … --view graph|list    draw the shape, or one dense row per node",
        "       @flow                        list the installed flows",
        "       @flow check [<name>]         verify a flow (or all of them) — no model needed",
        "       @flow graph <name>           the graph, drawn, with what each node reaches",
        "       @flow runs                   recent runs",
        "       @flow show <id>              one run: the graph, with what each node cost",
        "       @flow nodes [<id>]           every node of a run, side by side",
        "       @flow node [<id>] <node>     one node in full: cost, model, what it said",
        "       @flow watch [<id>]           attach to a run that is still going (Ctrl-C detaches)",
        "       @flow log <id> [<node>] [-f] a node's full output",
        "       @flow resume <id>            run only what did not complete",
        "       @flow retry [<id>] <node>    run one node again, and what depended on it",
        "       @flow clear                  prune finished runs",
    ]
    .join("\n")
}
