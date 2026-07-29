//! Scenario tests — real user journeys, played against the real code.
//!
//! A unit test proves a function; a scenario proves a **product**. These read as a
//! sequence of things a person does and a program does, and they run the same types the
//! shipped binary runs.
//!
//! One folder per feature under `scenarios/`, one [`World`] per folder. The engine owns
//! discovery, parsing and reporting; a world owns only its verbs. Adding a feature is a
//! folder, a file in `worlds/`, and a line in [`REGISTRY`].
//!
//! Nothing here spawns a process, opens a socket, or touches a PTY — which is what makes
//! it safe to write a scenario *about* a destructive command: the string exists only as
//! text asserted never to reach a buffer.
//!
//! Compiled only under `cfg(test)`; nothing ships.
//!
//! ```text
//! cargo test -p framework scenario -- --nocapture
//! ```

mod report;
mod scenario;
mod world;
mod worlds;

pub use report::Report;
pub use scenario::Scenario;
use world::{Factory, World};

/// Feature folder → the world that gives its verbs meaning.
const REGISTRY: &[(&str, Factory)] = &[
    ("ai", worlds::ai::build),
    ("config", worlds::config::build),
    ("gate", worlds::gate::build),
    ("jobs", worlds::jobs::build),
    ("keymap", worlds::keymap::build),
    ("markdown", worlds::markdown::build),
    ("plugins", worlds::plugins::build),
    ("security", worlds::security::build),
    ("shell", worlds::shell::build),
    ("terminal", worlds::terminal::build),
    ("theme", worlds::theme::build),
];

/// Run every `*.toml` in one feature's folder.
pub fn run_feature(feature: &str) -> Report {
    let mut report = Report::new(feature);
    let Some((_, factory)) = REGISTRY.iter().find(|(name, _)| *name == feature) else {
        report.fail_load(feature, "no world is registered for this folder");
        return report;
    };

    let dir = scenarios_root().join(feature);
    let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
            .collect(),
        Err(e) => {
            report.fail_load(&dir.display().to_string(), &e.to_string());
            return report;
        }
    };
    // Numbered filenames, so the report reads in the order the journeys were designed.
    files.sort();

    for path in files {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                report.fail_load(&name, &e.to_string());
                continue;
            }
        };
        match Scenario::parse(&name, &text) {
            Ok(s) => run_one(&s, *factory, &mut report),
            Err(e) => report.fail_load(&name, &e),
        }
    }
    report
}

fn run_one(s: &Scenario, factory: Factory, report: &mut Report) {
    let mut world: Box<dyn World> = match factory(&s.setup) {
        Ok(w) => w,
        Err(e) => return report.fail_load(&s.file, &format!("bad [setup]: {e}")),
    };
    for (i, step) in s.steps.iter().enumerate() {
        if let Err(why) = world.apply(step) {
            report.fail(&s.file, &s.name, i + 1, &world::label(step), &why);
            return;
        }
    }
    report.pass(&s.file, &s.name);
}

/// The repository's `scenarios/` folder.
fn scenarios_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scenarios")
}

/// Run one feature's scenarios and fail the test if any journey does.
fn check(feature: &str, minimum: usize) {
    let report = run_feature(feature);
    println!("{}", report.render());
    assert!(report.total() >= minimum, "expected at least {minimum} {feature} scenarios, found {}", report.total());
    assert!(report.ok(), "{} {feature} scenario(s) failed — see above", report.failed());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_scenarios() {
        check("ai", 23);
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
}
