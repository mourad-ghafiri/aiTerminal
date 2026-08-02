//! Building a graph for a goal — `@flow explain this project`.
//!
//! A flow file is a small, strict TOML document, and a goal is a sentence. This is the
//! one model call that turns the second into the first.
//!
//! **What comes back is an ordinary flow.** It goes through [`crate::flow::parse`] and
//! [`crate::flow::verify`] exactly as a file somebody wrote does — same grammar, same
//! checks, same refusals. There is no second kind of flow and no second way to run one:
//! a graph nobody wrote by hand is held to every rule one written by hand is held to,
//! which is what makes it safe to run something a model invented thirty seconds ago.
//!
//! The verifier is also the repair channel. Its errors are written to be read by a
//! person — "node 'check' needs 'draft', which does not exist" — so a graph that fails
//! gets **one** attempt with those errors handed straight back. A second failure prints
//! them and stops, having spent two small calls and no agent runs at all.
//!
//! This replaced routing a goal to one of the installed flows. Two answers to one
//! question is how a command stops being predictable, and picking the nearest of five
//! shipped graphs for an arbitrary goal produced runs that were plausible and wrong.

use crate::ai::defs::Agent;

/// How many times the verifier may send a graph back. One: the errors it produces are
/// specific, so a model that cannot use them once will not use them twice, and every
/// further round is money spent to reach the same refusal.
pub(crate) const REPAIRS: u32 = 1;

/// What the model is told a flow is.
///
/// Deliberately the whole grammar and nothing about style. Every field here is one the
/// parser accepts and the verifier checks; anything else it invents is refused, so the
/// cost of leaving something out is a graph that does not run rather than one that runs
/// wrongly.
const GRAMMAR: &str = "\
A flow is a TOML document describing a GRAPH of work. Reply with the document and nothing \
else — no prose, no code fence.\n\n\
description = \"one line: what this graph does\"\n\
input       = \"required\"\n\n\
[[node]]\n\
id     = \"read\"              # a short slug, letters/digits/dashes, unique\n\
agent  = \"explorer\"          # MUST be one of the agents listed below\n\
prompt = \"\"\"\n\
What this node is asked. Use {{input}} for the goal, and {{other.output}} to read what an\n\
earlier node produced.\n\
\"\"\"\n\n\
[[node]]\n\
id     = \"check\"\n\
agent  = \"reviewer\"\n\
needs  = [\"read\"]            # runs after these; nodes that need nothing run TOGETHER\n\
final  = true                # this node's answer is the flow's answer\n\
prompt = \"Check {{read.output}}\"\n\n\
Also available on a node:\n\
- `run = \"cargo test\"` instead of `agent`+`prompt` — a shell command, costing no tokens.\n\
- `when = 'read.output contains \"FAIL\"'` — only run if this holds. Other forms: \
`x.failed`, `x.passed`, `x.skipped`, `x.exit == 0`.\n\
- `goto = \"check\"` + `max = 2` — after this node, run `check` again, at most twice.\n\
- `retry = 1`, `timeout = \"10m\"`, `max_steps = 20`, `optional = true`, `solo = true`.\n\n\
Rules:\n\
- Use ONLY the agents listed below, spelled exactly. Inventing one means the graph is refused.\n\
- Prefer the SMALLEST graph that does the job. Three nodes that each do something beat eight \
that shuffle text between them.\n\
- Put work that does not depend on other work in nodes that do not `need` each other — that \
is the entire reason this is a graph and not a list.\n\
- Exactly one node carries `final = true`.\n\
- Every `{{x.output}}` must name a node that this node `needs`, directly or through others.";

/// A graph built for a goal: the document as it was written, and the flow it parsed to.
#[derive(Debug)]
pub(crate) struct Built {
    /// The TOML, kept verbatim — it is written into the run's record so what was built
    /// can be read, and a build that was refused is still there to look at.
    pub(crate) toml: String,
    pub(crate) flow: crate::flow::Flow,
    /// The verifier's findings for `flow`.
    pub(crate) report: crate::flow::verify::Report,
    /// How many repair rounds it took. `0` means it verified first time.
    pub(crate) repairs: u32,
}

/// Build a graph for `goal`.
///
/// `verify` is passed in rather than reached for, so the whole build/repair loop is
/// testable against a scripted transport and a stub verifier without a model, an agent
/// or a config directory.
pub(crate) fn build_with<T: platform::transport::Transport>(
    client: &crate::ai::Client<T>,
    goal: &str,
    agents: &[Agent],
    verify: &dyn Fn(&crate::flow::Flow) -> crate::flow::verify::Report,
) -> Result<Built, String> {
    if agents.is_empty() {
        return Err("no agents are installed, so there is nothing to build a graph out of".into());
    }
    let name = slug(goal);
    let mut complaint: Option<String> = None;
    let mut last: Option<String> = None;
    for round in 0..=REPAIRS {
        let ask = match &complaint {
            None => format!("Goal: {goal}\n\nThe agents you may use:\n{}", roster(agents)),
            // The verifier's own words, unedited. They name the node and say what is
            // wrong with it, which is more than a paraphrase would carry.
            Some(errors) => format!(
                "That graph will not run. The checker said:\n{errors}\n\nWrite the whole document \
                 again, fixed. Same goal: {goal}"
            ),
        };
        let reply = client
            .complete(&request(client, &ask))
            .map_err(|e| format!("building a graph for this goal failed: {e}"))?;
        let toml = document(&reply);
        last = Some(toml.clone());
        match crate::flow::parse(&name, &toml) {
            Err(e) => complaint = Some(e),
            Ok(flow) => {
                let report = verify(&flow);
                if report.ok() {
                    return Ok(Built { toml, flow, report, repairs: round });
                }
                complaint = Some(report.errors.join("\n"));
                // The last round's graph is still returned — refused, but readable. A
                // goal that produced nothing at all is a worse answer than one that
                // produced a graph you can look at and see the problem with.
                if round == REPAIRS {
                    return Ok(Built { toml, flow, report, repairs: round });
                }
            }
        }
    }
    Err(format!(
        "the model did not write a runnable graph for this goal \u{2014} {}\n{}",
        complaint.unwrap_or_else(|| "nothing usable came back".into()),
        last.map(|t| format!("what it wrote:\n{}", crate::cli::agentloop::show::clip_tail(&t, 400)))
            .unwrap_or_default()
    ))
}

/// The request. One question asked once, so nothing here is worth caching.
fn request<T: platform::transport::Transport>(client: &crate::ai::Client<T>, ask: &str) -> crate::ai::ChatRequest {
    crate::ai::ChatRequest {
        model: client.model().id.clone(),
        max_tokens: 2000,
        system: Some(GRAMMAR.to_string()),
        messages: vec![crate::ai::Message::user(ask.to_string())],
        temperature: Some(0.0),
        top_p: None,
        top_k: None,
        thinking: false,
        images: Vec::new(),
        cache: crate::ai::CacheHints::none(),
    }
}

/// The agents, as the model needs them: what each is for, and what it can reach.
///
/// The tools matter as much as the description. A graph that asks `explorer` to write a
/// file is a graph the verifier refuses, and the only way for the model to avoid writing
/// one is to know that `explorer` cannot write.
fn roster(agents: &[Agent]) -> String {
    agents
        .iter()
        .map(|a| format!("- {} — {} (tools: {})", a.name, a.description, a.tools.join(", ")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The document out of a reply, fence or no fence.
///
/// Asking for "no code fence" is a request, not a guarantee, and a model that wraps its
/// answer has still answered.
pub(crate) fn document(reply: &str) -> String {
    let text = reply.trim();
    let Some(open) = text.find("```") else { return text.to_string() };
    let after = &text[open + 3..];
    // The fence's language word, if it wrote one, is on the rest of that line.
    let body = after.split_once('\n').map_or(after, |(_, rest)| rest);
    body.split("```").next().unwrap_or(body).trim().to_string()
}

/// A flow name for a goal — `make the export emit JSON` → `make-the-export-emit`.
///
/// Only ever a label: a built graph lives in its run's own record and is never looked up
/// by this, so two goals that slug the same collide with nothing. It has to satisfy
/// [`crate::flow::tmpl::id_ok`], which is what the header, the record and the parser's
/// error messages all use it for.
pub(crate) fn slug(goal: &str) -> String {
    let words: Vec<String> = goal
        .split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase())
        .filter(|w| !w.is_empty())
        .take(5)
        .collect();
    let s = words.join("-");
    match crate::flow::tmpl::id_ok(&s) {
        true => s,
        // A goal written entirely in a script with no ASCII letters in it still gets a
        // run; it just gets a plain name.
        false => "built".to_string(),
    }
}

#[cfg(test)]
mod tests;
