//! `ai-defs` — on-disk loading of agent + skill definitions.
//!
//! Reads `*.md` files (frontmatter header + Markdown body) from caller-supplied
//! directories into plain specs. It is **Config-free** (every function takes the
//! directory explicitly) and **does not depend upward**: [`build_agent`] returns a
//! [`RawAgent`] carrying tool *names* only — the App injects each tool's
//! description (it owns `framework::caps::describe`), so this crate never reaches
//! into the Framework layer above it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use corelib::wire::Frontmatter;

/// A loaded agent header: identity + system body + the capabilities it declares (the tools it
/// may call, the skills/prompts spliced into its prompt, its step budget). An agent is
/// model-agnostic — it never pins a model; the model is the user's config pool + the `/model`
/// session pin. The declared `tools`/`skills`/`prompts` are exactly what the runtime enforces
/// (see [`build_agent_in`]) — so the inspector shows the truth, not a guess.
#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    pub name: String,
    pub description: String,
    pub system: String,
    pub tools: Vec<String>,
    pub skills: Vec<String>,
    pub prompts: Vec<String>,
    pub max_steps: u32,
}

/// A reusable skill: a Markdown capability doc spliced into an agent's prompt.
#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub body: String,
}

/// A reusable prompt: a Markdown body spliced into an agent's system prompt
/// (mirrors [`Skill`], but a separately-installable registry item).
#[derive(Clone, Debug, PartialEq)]
pub struct Prompt {
    pub name: String,
    pub body: String,
}

/// A raw agent spec: system prompt + declared tool NAMES + step cap. Tool
/// descriptions are filled in by the App, which has the capability registry.
#[derive(Clone, Debug, PartialEq)]
pub struct RawAgent {
    pub system: String,
    pub tools: Vec<String>,
    pub max_steps: u32,
}

/// Load all agents from `dir/*.md` (frontmatter + body), sorted by name.
pub fn load_agents(dir: &Path) -> Vec<Agent> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            let Some(name) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            if let Ok(text) = std::fs::read_to_string(&p) {
                let fm = Frontmatter::parse(&text);
                out.push(Agent {
                    name: name.to_string(),
                    description: fm.header.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    system: fm.body.trim().to_string(),
                    tools: resolved_tools(&fm),
                    skills: field_list(&fm, "skills"),
                    prompts: field_list(&fm, "prompts"),
                    max_steps: resolved_max_steps(&fm),
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// A named agent under `dir`, if present.
pub fn agent(dir: &Path, name: &str) -> Option<Agent> {
    load_agents(dir).into_iter().find(|a| a.name == name)
}

/// The most tool-calling turns an agent file may ask for. Past this it is not an agent
/// with a lot to do, it is a typo with a credit card.
const MAX_STEPS_CEILING: u32 = 60;

/// Everything wrong with the installed agents, one problem per line.
///
/// An agent is a file somebody edits, and until now nothing checked it. A misspelled
/// tool was handed to the model with a generic description and failed only when the
/// model tried to call it — three minutes and a few thousand tokens into a run. A
/// missing skill silently produced a weaker prompt with no sign anything was wrong.
/// Both are knowable for free, before anything starts.
///
/// `known_tool` is passed in rather than reached for: this module is the AI engine's
/// on-disk loader and deliberately does not know what a capability registry is.
pub fn validate(
    agents_dir: &Path,
    skills_dir: &Path,
    prompts_dir: &Path,
    known_tool: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let skills: Vec<String> = load_skills(skills_dir).into_iter().map(|s| s.name).collect();
    let prompts: Vec<String> = load_prompts(prompts_dir).into_iter().map(|p| p.name).collect();
    let mut out = Vec::new();
    for a in load_agents(agents_dir) {
        let at = |what: String| format!("agent '{}': {what}", a.name);
        if a.description.trim().is_empty() {
            out.push(at("no description — it is what `@agent` and every not-found error show".into()));
        }
        if a.system.trim().is_empty() {
            out.push(at("no body — the system prompt would be empty".into()));
        }
        for t in &a.tools {
            if !known_tool(t) {
                out.push(at(format!("declares tool '{t}', which does not exist")));
            }
        }
        for s in &a.skills {
            if !skills.contains(s) {
                out.push(at(format!("names skill '{s}', which is not installed")));
            }
        }
        for p in &a.prompts {
            if !prompts.contains(p) {
                out.push(at(format!("names prompt '{p}', which is not installed")));
            }
        }
        // The ceiling a tool loop cannot run past. Only the upper end is checkable here:
        // `resolved_max_steps` floors the declared value at 1, so a zero has already
        // become a one by the time this sees it — a check for it would be a branch that
        // can never run, which is worse than no check because it reads like one.
        if a.max_steps > MAX_STEPS_CEILING {
            out.push(at(format!("max_steps = {} — more than {MAX_STEPS_CEILING} is a bill nobody agreed to", a.max_steps)));
        }
    }
    out
}


/// Load all skills from `dir/*.md`.
pub fn load_skills(dir: &Path) -> Vec<Skill> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            if let (Some(name), Ok(text)) =
                (p.file_stem().and_then(|s| s.to_str()), std::fs::read_to_string(&p))
            {
                let fm = Frontmatter::parse(&text);
                out.push(Skill { name: name.to_string(), body: fm.body.trim().to_string() });
            }
        }
    }
    // Sorted, like every other loader here. `read_dir` yields whatever order the
    // filesystem feels like, which made listings differ between machines.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Load all prompts from `dir/*.md` (frontmatter + body), sorted by name. Mirrors
/// [`load_skills`] — a prompt is just a body installed as its own registry item.
pub fn load_prompts(dir: &Path) -> Vec<Prompt> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("md") {
                continue;
            }
            if let (Some(name), Ok(text)) =
                (p.file_stem().and_then(|s| s.to_str()), std::fs::read_to_string(&p))
            {
                let fm = Frontmatter::parse(&text);
                out.push(Prompt { name: name.to_string(), body: fm.body.trim().to_string() });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}




/// Read a frontmatter array field (e.g. `tools`, `skills`) into a `Vec<String>`.
fn field_list(fm: &Frontmatter, key: &str) -> Vec<String> {
    fm.header
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// The tools an agent is actually granted: its declared `tools`, or the read-only
/// [`DEFAULT_SAFE_TOOLS`](super::tools::DEFAULT_SAFE_TOOLS) when it declares none. The single
/// source of truth for both the runtime spec ([`build_agent_in`]) and the inspector
/// ([`load_agents`]) — so what the UI shows is exactly what the agent may call.
fn resolved_tools(fm: &Frontmatter) -> Vec<String> {
    let tools = field_list(fm, "tools");
    if tools.is_empty() {
        super::tools::DEFAULT_SAFE_TOOLS.iter().map(|s| s.to_string()).collect()
    } else {
        tools
    }
}

/// An agent's step budget: declared `max_steps`, default 6, floored at 1.
fn resolved_max_steps(fm: &Frontmatter) -> u32 {
    fm.header.get("max_steps").and_then(|v| v.as_int()).unwrap_or(6).max(1) as u32
}

/// Build the raw spec for `agent_name`: its system prompt with the named skills
/// (from `skills_dir`) and prompts (from `prompts_dir`) spliced in, its declared
/// tool names, and its step cap. Returns `None` if the agent file is missing. The
/// single-dir case of [`build_agent_in`].
pub fn build_agent(agents_dir: &Path, skills_dir: &Path, prompts_dir: &Path, agent_name: &str) -> Option<RawAgent> {
    build_agent_in(&[agents_dir.to_path_buf()], &[skills_dir.to_path_buf()], &[prompts_dir.to_path_buf()], agent_name)
}

// ===== workspace-aware loading (a dir LIST, project dir first → wins) ==========
//
// Callers pass the global dirs; the list form remains for tests + flexibility.
// These `*_in` variants load every dir and keep the FIRST definition per name, so a
// project file shadows the global one. The single-dir fns above are the 1-element case.

/// Keep the first item per `key` (stable order) — project-first dir lists shadow global.
fn dedup_first<T>(items: Vec<T>, key: impl Fn(&T) -> String) -> Vec<T> {
    let mut seen: HashSet<String> = HashSet::new();
    items.into_iter().filter(|i| seen.insert(key(i))).collect()
}

/// Skills across `dirs`, project-first (first definition per name wins).
pub fn load_skills_in(dirs: &[PathBuf]) -> Vec<Skill> {
    dedup_first(dirs.iter().flat_map(|d| load_skills(d)).collect(), |s| s.name.clone())
}

/// Prompts across `dirs` (first-wins), sorted by name.
pub fn load_prompts_in(dirs: &[PathBuf]) -> Vec<Prompt> {
    let mut v = dedup_first(dirs.iter().flat_map(|d| load_prompts(d)).collect(), |p| p.name.clone());
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// Agents across `dirs` (first-wins), sorted by name.
pub fn load_agents_in(dirs: &[PathBuf]) -> Vec<Agent> {
    let mut v = dedup_first(dirs.iter().flat_map(|d| load_agents(d)).collect(), |a| a.name.clone());
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// A named agent resolved across `dirs` (project-first).
pub fn agent_in(dirs: &[PathBuf], name: &str) -> Option<Agent> {
    load_agents_in(dirs).into_iter().find(|a| a.name == name)
}

/// Build a raw agent spec resolving the agent file + its skills/prompts across dir
/// LISTS (project-first). The agent `<name>.md` is taken from the first dir that has it;
/// spliced skills/prompts come from the merged, project-first sets.
pub fn build_agent_in(agents_dirs: &[PathBuf], skills_dirs: &[PathBuf], prompts_dirs: &[PathBuf], agent_name: &str) -> Option<RawAgent> {
    // A plain, filesystem-safe name only — `@../x` or `a/b` must never join into
    // a path outside the agents dirs (the same contract job ids enforce).
    if agent_name.is_empty() || !agent_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let path = agents_dirs.iter().map(|d| d.join(format!("{agent_name}.md"))).find(|p| p.is_file())?;
    let text = std::fs::read_to_string(path).ok()?;
    let fm = Frontmatter::parse(&text);
    let mut system = fm.body.trim().to_string();

    // Skills are spliced in the order the AGENT declared them, not the order the directory
    // happened to yield.
    //
    // Two things were wrong with the old way. It was non-deterministic — `read_dir` order
    // differs between machines and shifts as files are touched — so the same agent sent a
    // different system prompt on two machines, and prompt caching missed for a reason
    // nobody could see. And it threw away intent: `coder` declares
    // `["concise", "orchestration", "code-review", …]`, and that order reads as a priority.
    let want_skills = field_list(&fm, "skills");
    if !want_skills.is_empty() {
        let have = load_skills_in(skills_dirs);
        for want in &want_skills {
            let Some(s) = have.iter().find(|s| &s.name == want) else { continue };
            system.push_str("\n\n## Skill: ");
            system.push_str(&s.name);
            system.push('\n');
            system.push_str(&s.body);
        }
    }
    // Declared order here too, for the same reasons.
    let want_prompts = field_list(&fm, "prompts");
    if !want_prompts.is_empty() {
        let have = load_prompts_in(prompts_dirs);
        for want in &want_prompts {
            let Some(p) = have.iter().find(|p| &p.name == want) else { continue };
            system.push_str("\n\n## Prompt: ");
            system.push_str(&p.name);
            system.push('\n');
            system.push_str(&p.body);
        }
    }
    // An agent that declares no `tools` is granted the read-only DEFAULT_SAFE_TOOLS, so
    // it can browse + reason without side effects (the loop still refuses anything else).
    // `resolved_tools`/`resolved_max_steps` are shared with `load_agents` so the inspector
    // shows exactly what the runtime enforces.
    Some(RawAgent { system, tools: resolved_tools(&fm), max_steps: resolved_max_steps(&fm) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    

    /// An agent file, with everything valid unless a test says otherwise.
    fn agent_file(front: &str, body: &str) -> String {
        format!("---\n{front}\n---\n{body}")
    }

    #[test]
    fn an_agent_file_that_would_not_work_is_reported_rather_than_run() {
        // Agent files are user-editable Markdown, which means they will be edited wrongly:
        // a renamed tool, a skill nobody installed, a `max_steps` somebody meant as a
        // token budget. Every one of these RUNS otherwise — the loop silently refuses the
        // tool mid-flight, or the agent has nothing to say — and the person is left with a
        // bad run instead of a message.
        let root = std::env::temp_dir().join(format!("tt-aidefs-validate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (agents, skills, prompts) = (root.join("agents"), root.join("skills"), root.join("prompts"));
        write(&skills, "testing.md", "How to test.");
        write(&prompts, "concise.md", "Be brief.");

        write(&agents, "good.md", &agent_file("description = \"fine\"\ntools = [\"fs.read\"]\nskills = [\"testing\"]\nprompts = [\"concise\"]\nmax_steps = 8", "You are fine."));
        write(&agents, "no-desc.md", &agent_file("description = \"\"\nmax_steps = 8", "A body."));
        write(&agents, "no-body.md", &agent_file("description = \"has one\"\nmax_steps = 8", "   "));
        write(&agents, "bad-tool.md", &agent_file("description = \"d\"\ntools = [\"fs.reed\"]\nmax_steps = 8", "Body."));
        write(&agents, "bad-skill.md", &agent_file("description = \"d\"\nskills = [\"testign\"]\nmax_steps = 8", "Body."));
        write(&agents, "bad-prompt.md", &agent_file("description = \"d\"\nprompts = [\"concice\"]\nmax_steps = 8", "Body."));
        write(&agents, "runaway.md", &agent_file("description = \"d\"\nmax_steps = 500", "Body."));

        // An agent that declares no tools is granted the default safe set, so the
        // validator has to know those too or every such file looks broken.
        let known = |t: &str| t == "fs.read" || crate::ai::tools::DEFAULT_SAFE_TOOLS.contains(&t);
        let problems = validate(&agents, &skills, &prompts, &known);
        let all = problems.join("\n");
        for (agent, why) in [
            ("no-desc", "no description"),
            ("no-body", "no body"),
            ("bad-tool", "'fs.reed', which does not exist"),
            ("bad-skill", "'testign', which is not installed"),
            ("bad-prompt", "'concice', which is not installed"),
            ("runaway", "max_steps = 500"),
        ] {
            assert!(all.contains(&format!("agent '{agent}'")) && all.contains(why), "{agent}: {why} is missing from:\n{all}");
        }
        // Every complaint names the agent it is about — a list of problems you cannot
        // attribute is a list nobody can act on.
        assert!(problems.iter().all(|p| p.starts_with("agent '")), "{problems:?}");
        assert!(!all.contains("agent 'good'"), "a valid agent is not complained about:\n{all}");

        // `max_steps = 0` is floored to 1 at load rather than reported, so an agent is
        // never built that cannot call a tool at all. The validator does not pretend to
        // catch what it cannot see.
        write(&agents, "zero.md", &agent_file("description = \"d\"\nmax_steps = 0", "Body."));
        assert_eq!(load_agents(&agents).iter().find(|a| a.name == "zero").unwrap().max_steps, 1);
        assert!(!validate(&agents, &skills, &prompts, &known).join("\n").contains("agent 'zero'"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skills_are_spliced_in_the_order_the_agent_declared() {
        // Two things this pins. The prompt must be IDENTICAL every build — `read_dir` order
        // differs between machines and shifts as files are touched, so the same agent used
        // to send a different system prompt on two laptops and miss prompt caching for a
        // reason nobody could see. And the order is the AUTHOR's: `["b", "a"]` means the
        // author wanted b first, not whatever the directory yielded.
        let root = std::env::temp_dir().join(format!("tt-aidefs-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (agents, skills, prompts) = (root.join("agents"), root.join("skills"), root.join("prompts"));
        write(&skills, "alpha.md", "ALPHA BODY");
        write(&skills, "beta.md", "BETA BODY");
        write(&skills, "gamma.md", "GAMMA BODY");
        std::fs::create_dir_all(&prompts).unwrap();
        // Declared out of alphabetical order, and not naming every installed skill.
        write(&agents, "s.md", "---\nskills = [\"gamma\", \"alpha\"]\n---\nBODY");

        let built = build_agent(&agents, &skills, &prompts, "s").expect("builds");
        let gamma = built.system.find("## Skill: gamma").expect("gamma is spliced");
        let alpha = built.system.find("## Skill: alpha").expect("alpha is spliced");
        assert!(gamma < alpha, "declared order wins:\n{}", built.system);
        assert!(!built.system.contains("beta"), "an undeclared skill is not spliced");

        // Same inputs, same bytes — every time.
        for _ in 0..5 {
            let again = build_agent(&agents, &skills, &prompts, "s").unwrap();
            assert_eq!(again.system, built.system, "the system prompt must be deterministic");
        }
        // And a skill that is not installed is skipped rather than breaking the build.
        write(&agents, "t.md", "---\nskills = [\"alpha\", \"nope\"]\n---\nBODY");
        let t = build_agent(&agents, &skills, &prompts, "t").expect("builds anyway");
        assert!(t.system.contains("ALPHA BODY") && !t.system.contains("nope"));

        // Listings are sorted, whatever the filesystem says.
        let names: Vec<String> = load_skills(&skills).into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn agent_declaring_no_tools_shows_the_default_safe_set() {
        let root = std::env::temp_dir().join(format!("tt-aidefs-def-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let agents = root.join("agents");
        // No `tools`/`max_steps`/`description` declared → runtime defaults, mirrored for display.
        write(&agents, "scout.md", "---\nskills = [\"concise\"]\n---\nYou scout.");
        let loaded = load_agents(&agents);
        assert_eq!(loaded[0].description, "");
        assert_eq!(loaded[0].max_steps, 6, "default step budget");
        assert_eq!(loaded[0].tools, super::super::tools::DEFAULT_SAFE_TOOLS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        // display == runtime: the built spec grants the same default set.
        let raw = build_agent(&agents, &agents, &agents, "scout").unwrap();
        assert_eq!(loaded[0].tools, raw.tools);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn workspace_dirs_shadow_global_first_wins() {
        let root = std::env::temp_dir().join(format!("tt-aidefs-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let proj = root.join("proj"); // an earlier dir wins over a later one
        let glob = root.join("glob"); // global ~/.aiTerminal/ai/<kind>
        // Both define `coder.md` + a `git` skill; the workspace copy must win.
        write(&proj.join("agents"), "coder.md", "---\ntools = [\"fs.read\"]\nskills = [\"git\"]\n---\nPROJECT coder.");
        write(&glob.join("agents"), "coder.md", "---\ntools = [\"sys.run\"]\nskills = [\"git\"]\n---\nGLOBAL coder.");
        write(&glob.join("agents"), "writer.md", "---\n---\nGLOBAL writer."); // global-only
        write(&proj.join("skills"), "git.md", "---\n---\nPROJECT git skill.");
        write(&glob.join("skills"), "git.md", "---\n---\nGLOBAL git skill.");

        let adirs = vec![proj.join("agents"), glob.join("agents")];
        let sdirs = vec![proj.join("skills"), glob.join("skills")];
        let pdirs = vec![proj.join("prompts"), glob.join("prompts")];

        // The project `coder` shadows the global one, and pulls the PROJECT git skill.
        let raw = build_agent_in(&adirs, &sdirs, &pdirs, "coder").unwrap();
        assert!(raw.system.starts_with("PROJECT coder."));
        assert_eq!(raw.tools, vec!["fs.read".to_string()]);
        assert!(raw.system.contains("PROJECT git skill."));
        assert!(!raw.system.contains("GLOBAL git skill."));

        // The merged listing is deduped (one `coder`) but still includes global-only agents.
        let agents = load_agents_in(&adirs);
        assert_eq!(agents.iter().filter(|a| a.name == "coder").count(), 1);
        assert_eq!(agents.iter().find(|a| a.name == "coder").unwrap().system, "PROJECT coder.");
        assert!(agents.iter().any(|a| a.name == "writer"));

        // A global-only agent still resolves through the merged lists.
        assert!(build_agent_in(&adirs, &sdirs, &pdirs, "writer").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn agent_names_cannot_traverse_out_of_the_agents_dirs() {
        // The name is joined into a path — anything but a plain [A-Za-z0-9_-]+
        // name must be rejected BEFORE the join, or `@../../x` loads an arbitrary
        // .md file as a system prompt.
        let root = std::env::temp_dir().join(format!("tt-aidefs-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let agents = root.join("agents");
        write(&agents, "coder.md", "---\n---\nYou code.");
        // An escape target OUTSIDE the agents dir that would resolve via `..`.
        std::fs::write(root.join("evil.md"), "---\n---\nEVIL prompt.").unwrap();
        for bad in ["../evil", "a/b", "a\\b", "..", ".", "", "co der", "x\u{e9}"] {
            assert!(build_agent(&agents, &agents, &agents, bad).is_none(), "{bad:?} must be rejected");
        }
        // Plain names (letters/digits/dash/underscore) still resolve.
        assert!(build_agent(&agents, &agents, &agents, "coder").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }
}
