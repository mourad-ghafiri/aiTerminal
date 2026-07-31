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
