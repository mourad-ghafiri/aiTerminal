use super::*;

#[test]
fn ai_scenarios() {
    check("ai", 27);
}

#[test]
fn cli_scenarios() {
    check("cli", 24);
}

#[test]
fn memory_scenarios() {
    check("memory", 8);
}

#[test]
fn flow_scenarios() {
    check("flow", 21);
}

#[test]
fn gate_scenarios() {
    check("gate", 35);
}

#[test]
fn terminal_scenarios() {
    check("terminal", 22);
}

#[test]
fn security_scenarios() {
    check("security", 15);
}

#[test]
fn loop_scenarios() {
    check("loop", 13);
}

#[test]
fn markdown_scenarios() {
    check("markdown", 20);
}

#[test]
fn config_scenarios() {
    check("config", 16);
}

#[test]
fn plugin_scenarios() {
    check("plugins", 14);
}

#[test]
fn shell_scenarios() {
    check("shell", 10);
}

#[test]
fn jobs_scenarios() {
    check("jobs", 20);
}

#[test]
fn keymap_scenarios() {
    check("keymap", 10);
}

#[test]
fn theme_scenarios() {
    check("theme", 8);
}

#[test]
fn every_folder_has_a_world() {
    // A folder with no world would silently never run.
    let root = scenarios_root();
    for entry in std::fs::read_dir(&root).expect("scenarios/ must exist").flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        assert!(
            REGISTRY.iter().any(|(f, _)| *f == name),
            "scenarios/{name}/ has no world registered — it would never run"
        );
    }
}

#[test]
fn a_scenario_with_an_unknown_verb_is_rejected_loudly() {
    // A typo in a scenario must fail the suite, never pass silently.
    let s = Scenario::parse("bad.toml", "name = \"x\"\n[[step]]\nchhat = \"ls\"\n").unwrap();
    let mut w = worlds::gate::build(&s.setup).unwrap();
    let err = w.apply(&s.steps[0]).unwrap_err();
    assert!(err.contains("no known verb"), "{err}");
}

#[test]
fn a_scenario_without_steps_is_rejected() {
    assert!(Scenario::parse("x.toml", "name = \"x\"\n").is_err());
}
