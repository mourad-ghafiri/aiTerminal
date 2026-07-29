//! `@flow` — a workflow declared as a **graph**.
//!
//! A flow is a TOML file of `[[node]]` entries under `~/.aiTerminal/ai/flows/`.
//! Each node is one unit of work — an agent run, a shell command, or a pause for a
//! human — and `needs` names the nodes that must settle first. That single change,
//! from a list to a graph, is what makes the useful shapes expressible:
//!
//! - nodes that need nothing from each other run **at the same time**;
//! - `when` puts the routing decision on the **edge**, as data, instead of asking a
//!   model to decide what happens next and hoping it decides the same way twice;
//! - a `run` node costs **no tokens**, so the deterministic parts of a pipeline stay
//!   deterministic and the model is spent only where judgement is actually needed;
//! - `goto` points one edge backwards, bounded by `max`, so "test, fix, test again"
//!   is a flow rather than something you supervise by hand.
//!
//! Nothing here executes anything. This module turns a file into a [`Flow`], and
//! [`verify`] proves the graph is runnable before a single token is spent — the
//! difference between finding a typo now and finding it after three agent runs.

pub(crate) mod board;
pub(crate) mod expr;
pub(crate) mod pick;
pub(crate) mod render;
pub(crate) mod tmpl;
pub(crate) mod verify;

use corelib::wire::Toml;
use expr::Expr;
use tmpl::Template;

/// Whether the flow needs the text typed after its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Input {
    #[default]
    Optional,
    Required,
}

/// Ceilings for a whole run. A flow can run away in three directions — too much
/// wall clock, too many tokens, too many nodes at once — so there is a bound for
/// each, and a file may set its own where the shape demands it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct Bounds {
    pub timeout: Option<u64>,
    pub budget: Option<u64>,
    pub concurrency: Option<usize>,
}

/// What a node actually does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Kind {
    /// A full agent run — the expensive kind.
    Agent { agent: String, prompt: Template },
    /// A command through the same guard everything else goes through. Zero tokens:
    /// this is how a graph keeps its deterministic backbone deterministic.
    Run { command: Template },
    /// Stop and ask a person. Gates the *action*, never the investigation.
    Approve { show: Template, question: String },
}

impl Kind {
    pub fn word(&self) -> &'static str {
        match self {
            Kind::Agent { .. } => "agent",
            Kind::Run { .. } => "run",
            Kind::Approve { .. } => "approve",
        }
    }
}

/// One node of the graph.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Node {
    pub id: String,
    pub kind: Kind,
    /// Nodes that must settle before this one is considered.
    pub needs: Vec<String>,
    /// The condition on this node's incoming edge, as written and as parsed.
    pub when: Option<Expr>,
    pub when_src: String,
    /// A backward edge and its bound: after this node, run `goto` again, at most
    /// `max` times.
    pub goto: Option<String>,
    pub max: u32,
    /// Fan out over a list: one run of this node per item, in parallel.
    pub over: Option<Template>,
    /// What each item is called inside this node's templates.
    pub item: String,
    /// Re-run this node on failure, at most this many extra times.
    pub retry: u32,
    /// Per-node wall clock.
    pub timeout: Option<u64>,
    /// Per-node cap on the agent's tool loop.
    pub max_steps: Option<u32>,
    /// This node's output is the flow's answer.
    pub last: bool,
    /// Never run alongside another node.
    pub solo: bool,
    /// A failure here neither blocks dependents nor fails the run.
    pub optional: bool,
}

impl Node {
    /// The node fans out over a list.
    pub fn is_map(&self) -> bool {
        self.over.is_some()
    }

    /// Every template this node carries — what the verifier walks for references.
    pub fn templates(&self) -> Vec<&Template> {
        let mut out = match &self.kind {
            Kind::Agent { prompt, .. } => vec![prompt],
            Kind::Run { command } => vec![command],
            Kind::Approve { show, .. } => vec![show],
        };
        if let Some(over) = &self.over {
            out.push(over);
        }
        out
    }
}

/// A whole flow, parsed and ready to be verified.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Flow {
    pub name: String,
    pub description: String,
    pub input: Input,
    pub bounds: Bounds,
    pub nodes: Vec<Node>,
}

impl Flow {
    /// The index of a node by id.
    pub fn index(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    /// The node whose output is the flow's answer: the one marked `final`, else the
    /// last node nothing depends on, else simply the last declared.
    pub fn answer_node(&self) -> Option<usize> {
        if let Some(i) = self.nodes.iter().position(|n| n.last) {
            return Some(i);
        }
        let leaf = (0..self.nodes.len())
            .filter(|i| !self.nodes.iter().any(|n| n.needs.contains(&self.nodes[*i].id)))
            .next_back();
        leaf.or_else(|| self.nodes.len().checked_sub(1))
    }
}

// ─────────────────────────────── parsing ───────────────────────────────

/// Read a flow document.
///
/// Errors name the node they came from: "node 3" sends you to the wrong place in a
/// file whose nodes are not written in execution order — that is rather the point
/// of a graph.
pub(crate) fn parse(name: &str, text: &str) -> Result<Flow, String> {
    let doc = Toml::parse(text).map_err(|e| format!("flow '{name}': {e}"))?;
    let empty: &[Toml] = &[];
    let entries = doc.get("node").and_then(|v| v.as_array()).unwrap_or(empty);
    if entries.is_empty() {
        // The one place the old format is mentioned: not supported, but not a dead
        // end either — the rewrite is printed rather than left to be guessed.
        if doc.get("step").and_then(|v| v.as_array()).is_some_and(|s| !s.is_empty()) {
            return Err(legacy_error(name, &doc));
        }
        return Err(format!(
            "flow '{name}': no [[node]] entries — a flow is a graph of nodes"
        ));
    }
    let mut nodes = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        nodes.push(node(name, i, entry)?);
    }
    let b = doc.get("bounds");
    let dur = |key: &str| -> Result<Option<u64>, String> {
        match b.and_then(|b| b.get(key)) {
            None => Ok(None),
            Some(v) if v.as_int().is_some() => Ok(v.as_int().map(|n| n.max(0) as u64)),
            Some(v) => corelib::datetime::duration(v.as_str().unwrap_or_default())
                .map(Some)
                .ok_or_else(|| format!("flow '{name}': [bounds] {key} is a duration like 30m or 90s")),
        }
    };
    let input = match doc.get("input").and_then(|v| v.as_str()) {
        None | Some("optional") => Input::Optional,
        Some("required") => Input::Required,
        Some(other) => {
            return Err(format!("flow '{name}': input = {other:?} — say \"required\" or \"optional\""))
        }
    };
    Ok(Flow {
        name: name.to_string(),
        description: doc.get("description").and_then(|v| v.as_str()).unwrap_or_default().trim().to_string(),
        input,
        bounds: Bounds {
            timeout: dur("timeout")?,
            budget: b.and_then(|b| b.get("budget")).and_then(|v| v.as_int()).map(|v| v.max(0) as u64),
            concurrency: b
                .and_then(|b| b.get("concurrency"))
                .and_then(|v| v.as_int())
                .map(|v| v.clamp(1, 16) as usize),
        },
        nodes,
    })
}

fn node(flow: &str, i: usize, t: &Toml) -> Result<Node, String> {
    let str_of = |k: &str| t.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
    let id = str_of("id").unwrap_or_default().to_string();
    // Everything else is reported against the id, so establish it first.
    if id.is_empty() {
        return Err(format!("flow '{flow}': node {} has no `id`", i + 1));
    }
    if !tmpl::id_ok(&id) {
        return Err(format!(
            "flow '{flow}': node id {id:?} — letters, digits, '-' and '_' only, starting with a letter or digit"
        ));
    }
    let at = |msg: String| format!("flow '{flow}': node '{id}': {msg}");
    let tpl = |k: &str, s: &str| Template::parse(s).map_err(|e| at(format!("{k}: {e}")));

    let agent = str_of("agent");
    let run = str_of("run");
    let approve = str_of("kind").is_some_and(|k| k == "approve");
    let kind = match (agent, run, approve) {
        (Some(a), None, false) => {
            let prompt = str_of("prompt").ok_or_else(|| at("an agent node needs a `prompt`".into()))?;
            Kind::Agent { agent: a.to_string(), prompt: tpl("prompt", prompt)? }
        }
        (None, Some(c), false) => Kind::Run { command: tpl("run", c)? },
        (None, None, true) => Kind::Approve {
            show: tpl("show", str_of("show").unwrap_or_default())?,
            question: str_of("prompt").unwrap_or("Continue?").to_string(),
        },
        (None, None, false) => {
            return Err(at("needs one of `agent = \"…\"`, `run = \"…\"` or `kind = \"approve\"`".into()))
        }
        _ => return Err(at("has more than one kind — a node is an agent, a command, or an approval".into())),
    };
    // Only once the kind is settled, so a node with two kinds is told *that* rather
    // than being told one of its two kinds is a stray key.
    keys_belong(&at, t, kind.word())?;

    let when_src = str_of("when").unwrap_or_default().to_string();
    let when = if when_src.is_empty() {
        None
    } else {
        Some(expr::parse(&when_src).map_err(|e| at(format!("when = {when_src:?}: {e}")))?)
    };

    let list = |k: &str| -> Vec<String> {
        t.get(k)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    let num = |k: &str| t.get(k).and_then(|v| v.as_int());

    let over = match str_of("over") {
        Some(o) => Some(tpl("over", o)?),
        None => None,
    };
    let item = str_of("as").unwrap_or("item").to_string();
    if over.is_some() && !tmpl::id_ok(&item) {
        return Err(at(format!("as = {item:?} is not a name")));
    }
    let goto = str_of("goto").map(str::to_string);
    let max = num("max").unwrap_or(3).clamp(0, 25) as u32;
    if goto.is_some() && max == 0 {
        return Err(at("goto with max = 0 can never loop — give it a bound above zero".into()));
    }

    let timeout = match t.get("timeout") {
        None => None,
        Some(v) if v.as_int().is_some() => v.as_int().map(|n| n.max(0) as u64),
        Some(v) => Some(
            corelib::datetime::duration(v.as_str().unwrap_or_default())
                .ok_or_else(|| at("timeout is a duration like 10m or 90s".into()))?,
        ),
    };

    Ok(Node {
        id,
        kind,
        needs: list("needs"),
        when,
        when_src,
        goto,
        max,
        over,
        item,
        retry: num("retry").unwrap_or(0).clamp(0, 5) as u32,
        timeout,
        max_steps: num("max_steps").map(|v| v.clamp(1, 100) as u32),
        last: t.get("final").and_then(|v| v.as_bool()).unwrap_or(false),
        solo: t.get("solo").and_then(|v| v.as_bool()).unwrap_or(false),
        optional: t.get("optional").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

/// Refuse a key that does nothing here.
///
/// A `prompt` on a `run` node is not a harmless extra — it is someone who believes
/// their prompt is being used. Silently ignoring config is how a file comes to say
/// one thing while the tool does another, so every key must belong to the kind of
/// node it is written on, and a misspelling is caught in the file rather than
/// halfway through a run.
fn keys_belong(at: &dyn Fn(String) -> String, t: &Toml, kind: &str) -> Result<(), String> {
    const COMMON: &[&str] =
        &["id", "needs", "when", "goto", "max", "retry", "timeout", "final", "solo", "optional"];
    let extra: &[&str] = match kind {
        "agent" => &["agent", "prompt", "over", "as", "max_steps"],
        "run" => &["run", "over", "as"],
        _ => &["kind", "show", "prompt"],
    };
    let Some(table) = t.as_table() else { return Ok(()) };
    for (key, _) in table {
        if COMMON.contains(&key.as_str()) || extra.contains(&key.as_str()) {
            continue;
        }
        let a = if kind == "approve" { "an" } else { "a" };
        let elsewhere = ["agent", "prompt", "run", "kind", "show", "over", "as", "max_steps"].contains(&key.as_str());
        return Err(at(if elsewhere {
            format!("`{key}` does nothing on {a} {kind} node — it would be silently ignored")
        } else {
            format!("unknown key `{key}` ({a} {kind} node takes: {}, {})", extra.join(", "), COMMON.join(", "))
        }));
    }
    Ok(())
}

/// What a `[[step]]` file gets instead of silent support: a refusal, and the exact
/// graph it should have been.
///
/// The old format was a chain, so the rewrite is mechanical — each step becomes a
/// node that needs the one before it, and the implicit `chain = true` blob becomes
/// an explicit reference to the one output that was actually wanted. Printing it
/// costs one function; leaving people to guess costs them an afternoon.
fn legacy_error(name: &str, doc: &Toml) -> String {
    let empty: &[Toml] = &[];
    let steps = doc.get("step").and_then(|v| v.as_array()).unwrap_or(empty);
    let chained = doc.get("chain").and_then(|v| v.as_bool()).unwrap_or(true);
    let mut out = format!(
        "flow '{name}' uses the old [[step]] format, which is gone — a flow is now a graph of nodes.\n\n\
         Each step becomes a node that needs the one before it{}:\n",
        if chained { ", and `chain = true` becomes a reference to the output you actually want" } else { "" }
    );
    let mut previous: Option<String> = None;
    for (i, s) in steps.iter().enumerate() {
        let get = |k: &str| s.get(k).and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
        let agent = if get("agent").is_empty() { "explorer".to_string() } else { get("agent") };
        let id = {
            let label = get("label");
            let raw = if label.is_empty() { agent.clone() } else { label };
            let cleaned: String =
                raw.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
            if tmpl::id_ok(&cleaned) { cleaned } else { format!("step{}", i + 1) }
        };
        out.push_str("\n  [[node]]\n");
        out.push_str(&format!("  id     = {id:?}\n"));
        out.push_str(&format!("  agent  = {agent:?}\n"));
        if let Some(prev) = &previous {
            out.push_str(&format!("  needs  = [{prev:?}]\n"));
        }
        let mut prompt = get("prompt");
        if chained {
            if let Some(prev) = &previous {
                prompt.push_str(&format!("\\n\\n{{{{{prev}.output}}}}"));
            }
        }
        out.push_str(&format!("  prompt = {prompt:?}\n"));
        previous = Some(id);
    }
    out.push_str(&format!("\ncheck it with:  @flow check {name}\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIP: &str = r#"
description = "Explore, implement, verify"
input = "required"

[bounds]
timeout = "30m"
budget = 400000
concurrency = 4

[[node]]
id     = "map"
agent  = "explorer"
prompt = "Map the code for: {{input}}"

[[node]]
id     = "build"
agent  = "coder"
needs  = ["map"]
prompt = "Implement it:\n{{map.output}}"
retry  = 1

[[node]]
id    = "verify"
run   = "cargo test"
needs = ["build"]

[[node]]
id     = "fix"
agent  = "coder"
needs  = ["verify"]
when   = "verify.failed"
prompt = "Fix:\n{{verify.output}}"
goto   = "verify"
max    = 3

[[node]]
id     = "summary"
agent  = "reviewer"
needs  = ["verify"]
when   = "verify.passed"
final  = true
prompt = "Summarise {{build.output}}"
"#;

    fn ship() -> Flow {
        parse("ship", SHIP).expect("the reference flow parses")
    }

    #[test]
    fn a_graph_file_becomes_a_graph() {
        let f = ship();
        assert_eq!(f.description, "Explore, implement, verify");
        assert_eq!(f.input, Input::Required);
        assert_eq!(f.bounds, Bounds { timeout: Some(1800), budget: Some(400_000), concurrency: Some(4) });
        assert_eq!(f.nodes.len(), 5);
        assert_eq!(f.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["map", "build", "verify", "fix", "summary"]);
    }

    #[test]
    fn the_three_kinds_of_node_are_read_as_written() {
        let f = ship();
        assert_eq!(f.nodes[0].kind.word(), "agent");
        assert_eq!(f.nodes[2].kind.word(), "run");
        let approve = parse("g", "[[node]]\nid = \"gate\"\nkind = \"approve\"\nshow = \"{{a.output}}\"\nprompt = \"Ship it?\"\n").unwrap();
        assert_eq!(approve.nodes[0].kind.word(), "approve");
        match &approve.nodes[0].kind {
            Kind::Approve { question, .. } => assert_eq!(question, "Ship it?"),
            other => panic!("wrong kind: {other:?}"),
        }
    }

    #[test]
    fn edges_conditions_and_the_backward_edge_all_survive_the_file() {
        let f = ship();
        let fix = &f.nodes[3];
        assert_eq!(fix.needs, vec!["verify"]);
        assert_eq!(fix.when_src, "verify.failed");
        assert_eq!(fix.when.as_ref().unwrap().nodes(), vec!["verify"], "the condition names what it watches");
        assert_eq!(fix.goto.as_deref(), Some("verify"));
        assert_eq!(fix.max, 3);
        assert_eq!(f.nodes[1].retry, 1);
    }

    #[test]
    fn a_reference_is_structure_not_a_string_replace() {
        let f = ship();
        let refs: Vec<_> = f.nodes[1].templates()[0].refs().into_iter().cloned().collect();
        assert_eq!(refs, vec![tmpl::Ref::Node { id: "map".into(), field: tmpl::Field::Output }]);
    }

    #[test]
    fn the_answer_comes_from_the_node_marked_final() {
        let f = ship();
        assert_eq!(f.answer_node().map(|i| f.nodes[i].id.clone()), Some("summary".into()));
        // With nothing marked, the last leaf wins — never an arbitrary middle node.
        let plain = parse("p", "[[node]]\nid=\"a\"\nrun=\"true\"\n\n[[node]]\nid=\"b\"\nrun=\"true\"\nneeds=[\"a\"]\n").unwrap();
        assert_eq!(plain.answer_node().map(|i| plain.nodes[i].id.clone()), Some("b".into()));
    }

    #[test]
    fn a_map_node_declares_what_it_fans_out_over_and_what_each_item_is_called() {
        let f = parse(
            "audit",
            "[[node]]\nid=\"list\"\nrun=\"git ls-files\"\n\n[[node]]\nid=\"each\"\nagent=\"reviewer\"\nneeds=[\"list\"]\nover=\"{{list.output}}\"\nas=\"file\"\nprompt=\"Review {{file}}\"\n",
        )
        .unwrap();
        assert!(!f.nodes[0].is_map());
        assert!(f.nodes[1].is_map());
        assert_eq!(f.nodes[1].item, "file");
        assert_eq!(f.nodes[1].templates()[0].refs(), vec![&tmpl::Ref::Var("file".into())]);
    }

    #[test]
    fn a_malformed_node_says_which_node_and_what_is_missing() {
        for (src, want) in [
            ("[[node]]\nagent=\"a\"\nprompt=\"p\"\n", "node 1 has no `id`"),
            ("[[node]]\nid=\"a b\"\nrun=\"true\"\n", "letters, digits"),
            ("[[node]]\nid=\"a\"\n", "needs one of"),
            ("[[node]]\nid=\"a\"\nagent=\"x\"\n", "needs a `prompt`"),
            ("[[node]]\nid=\"a\"\nagent=\"x\"\nrun=\"true\"\nprompt=\"p\"\n", "more than one kind"),
            ("[[node]]\nid=\"a\"\nrun=\"true\"\nwhen=\"nonsense\"\n", "when = \"nonsense\""),
            ("[[node]]\nid=\"a\"\nrun=\"{{oops\"\n", "unclosed"),
            ("[[node]]\nid=\"a\"\nrun=\"true\"\ngoto=\"a\"\nmax=0\n", "can never loop"),
            ("[[node]]\nid=\"a\"\nrun=\"true\"\ntimeout=\"soon\"\n", "duration"),
            ("input = \"maybe\"\n[[node]]\nid=\"a\"\nrun=\"true\"\n", "required\" or \"optional"),
            ("[bounds]\ntimeout=\"soon\"\n[[node]]\nid=\"a\"\nrun=\"true\"\n", "duration"),
        ] {
            let err = parse("f", src).map(|_| ()).expect_err(&format!("{src:?} must not parse"));
            assert!(err.contains(want), "{src:?} said {err:?}, wanted {want:?}");
            assert!(err.starts_with("flow 'f'"), "every error names the flow: {err:?}");
        }
    }

    #[test]
    fn a_key_that_would_be_ignored_is_an_error_instead() {
        // Every one of these is someone believing a setting is in effect. Ignoring
        // them quietly is how a file ends up saying one thing and doing another.
        for (src, want) in [
            ("[[node]]\nid=\"a\"\nrun=\"true\"\nprompt=\"hi\"\n", "`prompt` does nothing on a run node"),
            ("[[node]]\nid=\"a\"\nrun=\"true\"\nmax_steps=4\n", "does nothing on a run node"),
            ("[[node]]\nid=\"a\"\nkind=\"approve\"\nover=\"{{b.output}}\"\n", "does nothing on an approve node"),
            ("[[node]]\nid=\"a\"\nagent=\"x\"\nprompt=\"p\"\nneed=[\"b\"]\n", "unknown key `need`"),
            ("[[node]]\nid=\"a\"\nagent=\"x\"\nprompt=\"p\"\nwhne=\"b.passed\"\n", "unknown key `whne`"),
        ] {
            let err = parse("f", src).map(|_| ()).expect_err(&format!("{src:?} must not parse"));
            assert!(err.contains(want), "{src:?} said {err:?}, wanted {want:?}");
        }
        // And the keys that DO belong are all accepted together.
        let full = "[[node]]\nid=\"a\"\nrun=\"true\"\n\n[[node]]\nid=\"b\"\nagent=\"coder\"\nprompt=\"p\"\nneeds=[\"a\"]\nwhen=\"a.passed\"\ngoto=\"a\"\nmax=2\nover=\"{{a.output}}\"\nas=\"it\"\nretry=1\ntimeout=\"5m\"\nmax_steps=9\nfinal=true\nsolo=true\noptional=true\n";
        assert!(parse("f", full).is_ok(), "{:?}", parse("f", full).err());
    }

    #[test]
    fn a_file_with_no_nodes_is_refused() {
        let err = parse("empty", "description = \"nothing\"\n").unwrap_err();
        assert!(err.contains("no [[node]] entries"));
    }

    #[test]
    fn an_old_step_file_is_refused_with_the_graph_it_should_have_been() {
        // No compatibility code runs — but nobody is left guessing either.
        let old = r#"
description = "Explore then review"
chain = true

[[step]]
label  = "map"
agent  = "explorer"
prompt = "Map the code for: {{input}}"

[[step]]
label  = "review"
agent  = "reviewer"
prompt = "Review it"
"#;
        let err = parse("review", old).unwrap_err();
        assert!(err.contains("old [[step]] format"), "it says what happened");
        assert!(err.contains("[[node]]"), "and shows the new shape");
        assert!(err.contains("id     = \"map\""), "keeping the labels as ids");
        assert!(err.contains("needs  = [\"map\"]"), "chaining becomes an explicit edge");
        assert!(err.contains("{{map.output}}"), "and the blob becomes one named reference");
        assert!(err.contains("@flow check review"), "with somewhere to go next");
        // The printed rewrite is a real flow, not prose that looks like one.
        let start = err.find("[[node]]").unwrap();
        let end = err.find("\ncheck it with").unwrap();
        let rewritten: String = err[start..end].lines().map(|l| l.trim_start()).collect::<Vec<_>>().join("\n");
        let back = parse("review", &rewritten).expect("the suggested rewrite parses");
        assert_eq!(back.nodes.len(), 2);
        assert_eq!(back.nodes[1].needs, vec!["map"]);
    }

    #[test]
    fn a_step_file_without_chain_does_not_invent_references() {
        let old = "chain = false\n\n[[step]]\nagent = \"a\"\nprompt = \"one\"\n\n[[step]]\nagent = \"b\"\nprompt = \"two\"\n";
        let err = parse("f", old).unwrap_err();
        assert!(!err.contains(".output}}"), "nothing was chained, so nothing is referenced");
        assert!(err.contains("needs  = [\"a\"]"), "the order is still an edge");
    }
}
