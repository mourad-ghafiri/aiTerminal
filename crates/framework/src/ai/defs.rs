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

    let want_skills = field_list(&fm, "skills");
    if !want_skills.is_empty() {
        for s in load_skills_in(skills_dirs).iter().filter(|s| want_skills.contains(&s.name)) {
            system.push_str("\n\n## Skill: ");
            system.push_str(&s.name);
            system.push('\n');
            system.push_str(&s.body);
        }
    }
    let want_prompts = field_list(&fm, "prompts");
    if !want_prompts.is_empty() {
        for p in load_prompts_in(prompts_dirs).iter().filter(|p| want_prompts.contains(&p.name)) {
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
