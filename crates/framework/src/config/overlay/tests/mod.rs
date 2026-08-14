use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tt-overlay-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_offering_counts_without_loading_and_notices_instructions() {
    let root = scratch("offer");
    assert_eq!(Workspace::offering(&root), Offering::default(), "an empty folder offers nothing");

    std::fs::create_dir_all(root.join(".aiTerminal/agents")).unwrap();
    std::fs::create_dir_all(root.join(".aiTerminal/mcp")).unwrap();
    std::fs::write(root.join(".aiTerminal/agents/reviewer.md"), "# r").unwrap();
    std::fs::write(root.join(".aiTerminal/mcp/docs.toml"), "command = \"x\"").unwrap();
    std::fs::write(root.join("AGENTS.md"), "conventions").unwrap();
    let o = Workspace::offering(&root);
    assert_eq!((o.agents, o.mcp, o.instructions), (1, 1, Some("AGENTS.md")));

    // Ours wins over the convention the moment it exists.
    std::fs::write(root.join("aiTerminal.md"), "ours").unwrap();
    assert_eq!(Workspace::offering(&root).instructions, Some("aiTerminal.md"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn trust_gates_the_overlay_and_the_dirs_are_project_first() {
    let root = scratch("dirs");
    std::fs::create_dir_all(root.join(".aiTerminal/agents")).unwrap();

    let untrusted = Workspace::open(&root, false);
    assert!(!untrusted.overlaid(), "declining trust means global config only");
    assert_eq!(untrusted.agents_dirs().len(), 1);

    let trusted = Workspace::open(&root, true);
    assert!(trusted.overlaid());
    let dirs = trusted.agents_dirs();
    assert_eq!(dirs.len(), 2);
    assert_eq!(dirs[0], root.join(".aiTerminal/agents"), "the project dir comes FIRST — first-per-name wins");
    // A kind the project does not overlay stays global-only, no phantom dir.
    assert_eq!(trusted.flows_dirs().len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_fingerprint_moves_only_with_what_executes() {
    let root = scratch("finger");
    std::fs::create_dir_all(root.join(".aiTerminal/mcp")).unwrap();
    let before = Workspace::fingerprint(&root);
    // Prose does not re-open the trust question…
    std::fs::write(root.join("aiTerminal.md"), "notes").unwrap();
    assert_eq!(Workspace::fingerprint(&root), before);
    // …an mcp declaration does.
    std::fs::write(root.join(".aiTerminal/mcp/new.toml"), "command = \"srv\"").unwrap();
    let after = Workspace::fingerprint(&root);
    assert_ne!(after, before);
    // …and so does the project config.
    std::fs::write(root.join(".aiTerminal/config.toml"), "[ai]\n").unwrap();
    assert_ne!(Workspace::fingerprint(&root), after);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn instructions_layer_global_then_project_with_ours_winning() {
    let root = scratch("instr");
    std::fs::write(root.join("AGENTS.md"), "convention text").unwrap();
    let ws = Workspace::open(&root, true);
    let text = ws.instructions("global text");
    let g = text.find("global text").expect("global present");
    let p = text.find("convention text").expect("project present");
    assert!(g < p, "project comes after — nearer wins by position");
    assert!(text.contains("(AGENTS.md)"));

    std::fs::write(root.join("aiTerminal.md"), "ours text").unwrap();
    let text = ws.instructions("global text");
    assert!(text.contains("ours text") && !text.contains("convention text"), "aiTerminal.md wins outright");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tighten_keeps_the_refusals_and_drops_every_loosening_tier() {
    let doc = corelib::wire::Toml::parse(
        "[[guard.command]]\npattern = \"^wipe-the-lot\"\nrule = \"deny\"\n\
         [[guard.command]]\npattern = \"^touch-the-config\"\nrule = \"confirm\"\n\
         [[guard.command]]\npattern = \".*\"\nrule = \"allow\"\n\
         [[guard.command]]\npattern = \"^ls\"\nrule = \"auto\"\n\
         [[guard.path]]\npattern = \"secrets/\"\nrule = \"deny\"\n\
         [[guard.path]]\npattern = \".*\"\nrule = \"allow\"\n\
         [[guard.secret]]\npattern = \"tok-[a-z0-9]+\"\n",
    )
    .unwrap();
    let (kept, dropped) = tighten(RuleSet::parse(doc.get("guard").unwrap()));
    assert_eq!(kept.commands.len(), 2, "deny + confirm stay");
    assert_eq!(kept.paths.len(), 1, "the path deny stays");
    assert_eq!(kept.secrets.len(), 1, "a secret rule always tightens");
    assert_eq!(dropped.len(), 3, "allow, auto and the path allow are all named: {dropped:?}");
    assert!(dropped.iter().all(|d| d.contains("may not allow-list")), "{dropped:?}");
}

#[test]
fn a_project_config_overrides_ai_but_its_guard_section_never_reaches_apply() {
    let root = scratch("cfg");
    std::fs::create_dir_all(root.join(".aiTerminal")).unwrap();
    std::fs::write(
        root.join(".aiTerminal/config.toml"),
        "[ai]\nmemory = false\n[guard]\n[[guard.command]]\npattern = \".*\"\nrule = \"allow\"\n",
    )
    .unwrap();
    let base = crate::config::Config::default();
    assert!(base.ai_memory, "the default this project flips");
    let ws = Workspace::open(&root, true);
    let cfg = ws.config(&base);
    assert!(!cfg.ai_memory, "the [ai] override lands");
    assert!(cfg.guard.commands.is_empty(), "the project's guard section must NOT ride in through apply_toml");
    // The allow rule is dropped on the tightening path too — nothing kept.
    assert!(ws.project_rules().is_none(), "an all-loosening ruleset contributes nothing");
    let _ = std::fs::remove_dir_all(&root);
}
