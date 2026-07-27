//! What happened, in a form a person can act on.
//!
//! A failing scenario names the journey, the step number, the verb, and what was
//! expected versus what happened — so the first line of output is the bug report.

pub struct Report {
    suite: String,
    entries: Vec<Entry>,
}

struct Entry {
    file: String,
    name: String,
    outcome: Outcome,
}

enum Outcome {
    Passed,
    Failed { step: usize, label: String, why: String },
    NotLoaded { why: String },
}

impl Report {
    pub fn new(suite: &str) -> Report {
        Report { suite: suite.to_string(), entries: Vec::new() }
    }

    pub fn pass(&mut self, file: &str, name: &str) {
        self.entries.push(Entry { file: file.into(), name: name.into(), outcome: Outcome::Passed });
    }

    pub fn fail(&mut self, file: &str, name: &str, step: usize, label: &str, why: &str) {
        self.entries.push(Entry {
            file: file.into(),
            name: name.into(),
            outcome: Outcome::Failed { step, label: label.into(), why: why.into() },
        });
    }

    pub fn fail_load(&mut self, file: &str, why: &str) {
        self.entries.push(Entry {
            file: file.into(),
            name: "(could not load)".into(),
            outcome: Outcome::NotLoaded { why: why.into() },
        });
    }

    pub fn total(&self) -> usize {
        self.entries.len()
    }

    pub fn failed(&self) -> usize {
        self.entries.iter().filter(|e| !matches!(e.outcome, Outcome::Passed)).count()
    }

    pub fn ok(&self) -> bool {
        self.failed() == 0
    }

    pub fn render(&self) -> String {
        let mut out = format!("\n  {} scenarios\n", self.suite);
        for e in &self.entries {
            match &e.outcome {
                Outcome::Passed => out.push_str(&format!("  ✓ {}\n", e.name)),
                Outcome::Failed { step, label, why } => {
                    out.push_str(&format!("  ✗ {}\n", e.name));
                    out.push_str(&format!("      {} · step {step} · {label}\n", e.file));
                    out.push_str(&format!("      {why}\n"));
                }
                Outcome::NotLoaded { why } => {
                    out.push_str(&format!("  ✗ {} — {why}\n", e.file));
                }
            }
        }
        let passed = self.total() - self.failed();
        out.push_str(&format!("\n  {passed}/{} passed\n", self.total()));
        out
    }
}
