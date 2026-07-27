//! Scenario tests — real user journeys, played against the real decision layer.
//!
//! A unit test proves a function; a scenario proves a *product*. These read as a
//! sequence of things a person does and a program does, and they run the same
//! [`Gate`](crate::gate::driver::Gate), the same mirror terminal, and the same relay
//! ordering the shipped binary uses.
//!
//! They are compiled only under `cfg(test)` — nothing here ships.
//!
//! ```text
//! cargo test -p framework scenario -- --nocapture
//! ```

mod report;
mod scenario;
mod step;
mod world;

pub use report::Report;
pub use scenario::Scenario;
use world::World;

/// Run every `*.toml` in a scenario folder.
pub fn run_dir(dir: &std::path::Path) -> Report {
    let mut report = Report::new(dir.file_name().and_then(|n| n.to_str()).unwrap_or("scenarios"));
    let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
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
            Ok(s) => run_one(&s, &mut report),
            Err(e) => report.fail_load(&name, &e),
        }
    }
    report
}

fn run_one(s: &Scenario, report: &mut Report) {
    let mut world = World::new(&s.setup);
    for (i, step) in s.steps.iter().enumerate() {
        if let Err(why) = world.apply(step) {
            report.fail(&s.file, &s.name, i + 1, &step.label(), &why);
            return;
        }
    }
    report.pass(&s.file, &s.name);
}

/// The repository's `scenarios/` folder.
fn scenarios_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scenarios")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gate_scenario_passes() {
        let report = run_dir(&scenarios_root().join("gate"));
        println!("{}", report.render());
        assert!(report.total() >= 30, "expected at least 30 gate scenarios, found {}", report.total());
        assert!(report.ok(), "{} scenario(s) failed — see above", report.failed());
    }

    #[test]
    fn a_scenario_with_an_unknown_verb_is_rejected_loudly() {
        // A typo in a scenario must fail the suite, never pass silently.
        let bad = "name = \"x\"\n[[step]]\nchhat = \"ls\"\n";
        let err = Scenario::parse("bad.toml", bad).unwrap_err();
        assert!(err.contains("no known verb"), "{err}");
    }

    #[test]
    fn a_scenario_without_steps_is_rejected() {
        assert!(Scenario::parse("x.toml", "name = \"x\"\n").is_err());
    }
}
