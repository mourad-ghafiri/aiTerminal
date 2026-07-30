//! **The website is a promise.** This runs it.
//!
//! Every bug a user has reported lately was something the site told them to type. Not a
//! subtle one — the very first claim tried by hand was broken:
//!
//! ```text
//! ❯ @plugin disable weather     → disabled plugin 'weather'
//! ❯ @plugin enable weather      → no plugin 'weather'      (and no way back)
//! ```
//!
//! Reviewing pages does not catch that; only running them does. So this walks
//! `index.html`, `website/docs.html` and `website/use-cases.html`, pulls out every
//! `@command` shown in a code block, and **executes it** against a throwaway `$HOME`
//! through the same dispatch the shell calls.
//!
//! Scope, stated rather than implied: commands that need a model are skipped **by name**
//! and printed, so the hole is visible instead of silent. What is left is the surface a
//! new user meets first — profiles, themes, config, plugins, flows, jobs — which is
//! exactly where the reports have been coming from.

use std::collections::BTreeSet;

/// A command the site tells a user to type, and where it says it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Claim {
    /// The `@…` line as written on the page.
    line: String,
    source: String,
}

/// The pages that make promises.
const PAGES: [&str; 3] = ["index.html", "website/docs.html", "website/use-cases.html"];

/// Repo root, from this crate's manifest.
fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Strip HTML tags and decode the handful of entities the pages use, so a claim reads
/// as the user sees it rather than as markup.
fn plain(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

/// Every `@command` line inside a `<pre><code>` block on `page`.
fn claims_in(page: &str) -> Vec<Claim> {
    let Ok(html) = std::fs::read_to_string(root().join(page)) else { return Vec::new() };
    let mut out = Vec::new();
    for block in html.split("<pre>").skip(1) {
        let Some(code) = block.split("</pre>").next() else { continue };
        for raw in plain(code).lines() {
            // The prompt glyph is chrome; what follows is what a user types.
            let line = raw.trim().trim_start_matches('❯').trim();
            if line.starts_with('@') {
                out.push(Claim { line: line.to_string(), source: page.to_string() });
            }
        }
    }
    out
}

/// How a claim is treated by the audit.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Run it; it must succeed.
    Run(Vec<String>),
    /// Skipped, with the reason — reported so the gap is visible.
    Skip(&'static str),
}

/// Commands whose first word needs a configured model. Listed rather than detected, so
/// adding an AI command does not silently widen the skip set.
const NEEDS_MODEL: [&str; 3] = ["@ai", "@loop", "@agent"];

/// `@flow`/`@job` subcommands that run WITHOUT a model — everything else under them
/// plans or runs an agent and is skipped.
const OFFLINE_FLOW_SUBS: [&str; 5] = ["check", "graph", "show", "log", "resume"];

/// Split a claim into argv, or say why it is not run.
fn classify(line: &str) -> Verdict {
    // A page separates several commands with `·` and annotates them with `←`; neither
    // is one thing to run.
    if line.contains('\u{b7}') || line.contains('\u{2190}') {
        return Verdict::Skip("shows several commands, or an annotation, on one line");
    }
    // Trailing `# …` is explanation for the reader, not an argument.
    let line = match line.find(" #") {
        Some(i) => line[..i].trim_end(),
        None => line,
    };
    // Placeholders (`<name>`, `…`) are illustrations of a shape, not commands.
    if line.contains('<') || line.contains('\u{2026}') || line.contains("...") {
        return Verdict::Skip("shows a placeholder, not a runnable command");
    }
    if line.contains('|') || line.contains('>') || line.contains('$') {
        return Verdict::Skip("involves the shell (pipe/redirect/variable)");
    }
    // `-f` follows a log until interrupted. Running one here would hang the suite the
    // moment the id it names actually exists.
    if line.split_whitespace().any(|w| w == "-f" || w == "--follow") {
        return Verdict::Skip("follows a log, which never returns on its own");
    }
    // `@md` needs a document. The page's `release.md` is the reader's file, not one
    // this audit can conjure without writing into the repo it is running from — and
    // `@md edit` additionally needs a terminal. The engine itself has 28 markdown
    // scenarios; what is unproven here is only the argument plumbing.
    if line.starts_with("@md ") && line.split_whitespace().count() > 2 {
        return Verdict::Skip("needs a document on disk (see the markdown scenarios)");
    }
    let words = match shell_words(line) {
        Some(w) if !w.is_empty() => w,
        _ => return Verdict::Skip("could not be split into arguments"),
    };
    let head = words[0].as_str();
    if NEEDS_MODEL.contains(&head) {
        return Verdict::Skip("needs a configured model");
    }
    // `@<agent> "task"` — an agent run.
    let rest: Vec<String> = words[1..].to_vec();
    match head {
        "@profile" | "@theme" | "@config" | "@plugin" | "@gate" | "@md" => {
            Verdict::Run(std::iter::once(head.trim_start_matches('@').to_string()).chain(rest).collect())
        }
        "@flow" | "@job" => {
            let sub = rest.first().map(String::as_str).unwrap_or("");
            let offline = rest.is_empty() || OFFLINE_FLOW_SUBS.contains(&sub) || sub.starts_with('-');
            if !offline {
                return Verdict::Skip("runs agents");
            }
            Verdict::Run(
                ["ai".to_string(), head.trim_start_matches('@').to_string()].into_iter().chain(rest).collect(),
            )
        }
        _ => Verdict::Skip("an agent name, which runs a model"),
    }
}

/// Split on whitespace, honouring double quotes — enough for the shapes the site shows.
fn shell_words(line: &str) -> Option<Vec<String>> {
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if quoted {
        return None; // an unbalanced quote is a rendering artifact, not a command
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Some(out)
}

/// Run one claim through the CLI dispatch. `None` when the head is not a known command.
fn dispatch(argv: &[String]) -> Option<i32> {
    let rest = argv[1..].to_vec();
    Some(match argv[0].as_str() {
        "ai" => crate::cli::ai(&rest),
        "profile" => crate::cli::profile(&rest),
        "theme" => crate::cli::theme(&rest),
        "config" => crate::cli::config(&rest),
        "plugin" => crate::cli::plugin(&rest),
        "gate" => crate::cli::gate(&rest),
        "md" => crate::cli::md(&rest),
        _ => return None,
    })
}

/// Claims that legitimately do not exit 0, with the reason. Anything not listed here
/// must succeed — that asymmetry is the point: a new failure has to be justified in
/// writing rather than absorbed.
fn expected_failure(line: &str) -> Option<&'static str> {
    let l = line.trim();
    // A gate command that would START one declines while gates ship off; the read-only
    // ones (`@gate`, `@gate status`) still answer.
    if l.starts_with("@gate") && (l.contains("start") || l.contains("stop") || l.contains("only")) {
        return Some("gates ship off, so starting or stopping one declines until enabled");
    }
    // Inspecting a flow run needs a previous run, which needs a model.
    if l.starts_with("@flow") && ["show", "log", "resume"].iter().any(|s| l.contains(s)) {
        return Some("inspects a previous flow run, which cannot exist without a model");
    }
    if l.contains("revieew") {
        return Some("the site's own typo demo \u{2014} it exists to show the suggestion");
    }
    None
}

/// Give the temp `$HOME` the state the pages assume a reader already has: a finished
/// job to inspect, and a theme they were shown creating. Without it the audit would
/// report "no jobs yet" as a broken promise, which is the harness being wrong rather
/// than the product.
fn seed(home: &std::path::Path) {
    let _ = crate::cli::ai(&["job".into(), "--".into(), "echo".into(), "audit".into()]);
    if let Ok(t) = std::fs::read_to_string(home.join(".aiTerminal/themes/nebula.toml")) {
        let _ = std::fs::write(home.join(".aiTerminal/themes/mine.toml"), t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_the_website_shows_actually_works() {
        let (_h, _home) = crate::test_home::lock_home("site-audit");
        crate::config::Config::ensure_default();
        crate::i18n::install(crate::config::Config::load().i18n_catalog());
        seed(&_home);

        let claims: Vec<Claim> = PAGES.iter().flat_map(|p| claims_in(p)).collect();
        assert!(claims.len() > 30, "the pages should show plenty of commands, found {}", claims.len());

        let (mut ran, mut failures, mut skipped) = (0usize, Vec::new(), BTreeSet::new());
        for claim in &claims {
            match classify(&claim.line) {
                Verdict::Skip(why) => {
                    skipped.insert(format!("{why}: {}", claim.line));
                }
                Verdict::Run(argv) => {
                    let Some(code) = dispatch(&argv) else {
                        failures.push(format!("{} — {:?} is not a command the CLI dispatches", claim.source, claim.line));
                        continue;
                    };
                    ran += 1;
                    match (code, expected_failure(&claim.line)) {
                        (0, None) => {}
                        (0, Some(why)) => failures
                            .push(format!("{} — {:?} SUCCEEDED but is listed as expected-to-fail ({why})", claim.source, claim.line)),
                        (_, Some(_)) => {}
                        (c, None) => failures.push(format!("{} — {:?} exited {c}", claim.source, claim.line)),
                    }
                }
            }
        }

        // The skip set is printed, never hidden: a command that quietly stopped being
        // audited is the same problem in a different place.
        println!("site audit: ran {ran} of {} claims", claims.len());
        for s in &skipped {
            println!("  skipped — {s}");
        }
        assert!(ran >= 15, "too few claims were actually executed ({ran}) — the extractor has drifted");
        assert!(failures.is_empty(), "the website promises things that do not work:\n  {}", failures.join("\n  "));
    }

    #[test]
    fn every_documented_switch_can_be_switched_back() {
        // Running each claim on its own was NOT enough, and this test exists because of
        // that gap: the plugin bug lived in a SEQUENCE. The site shows `@plugin disable
        // redactor` concretely but `@plugin enable <name>` only as a shape, so a runner
        // that executes literal lines never performed the round trip and never saw that
        // there was no way back.
        //
        // So every documented switch is exercised as a PAIR. A capability you can only
        // use in one direction is not a capability.
        let (_h, home) = crate::test_home::lock_home("site-round-trips");
        crate::config::Config::ensure_default();
        crate::i18n::install(crate::config::Config::load().i18n_catalog());
        let arg = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // ── a plugin goes off and comes back ──────────────────────────────────
        assert_eq!(crate::cli::plugin(&arg(&["disable", "weather"])), 0, "the site documents disabling");
        assert_eq!(crate::cli::plugin(&arg(&["info", "weather"])), 0, "…and it is still describable");
        assert_eq!(crate::cli::plugin(&arg(&["enable", "weather"])), 0, "…and re-enabling must work");

        // ── a profile is created, renamed, switched to, and removed ───────────
        assert_eq!(crate::cli::profile(&arg(&["create", "Work", "\u{1f4bc}"])), 0);
        assert_eq!(crate::cli::profile(&arg(&["rename", "work", "Work Stuff"])), 0);
        assert_eq!(crate::cli::profile(&arg(&["work"])), 0, "switch to it");
        assert_eq!(crate::cli::profile(&arg(&["current"])), 0);
        assert_eq!(crate::cli::profile(&arg(&["default"])), 0, "switch away before deleting");
        assert_eq!(crate::cli::profile(&arg(&["delete", "work"])), 0);

        // ── a theme is switched and switched back ─────────────────────────────
        assert_eq!(crate::cli::theme(&arg(&["sunset"])), 0);
        assert_eq!(crate::cli::theme(&arg(&["midnight"])), 0);
        // …and one exported to a file is then selectable, which is the documented way
        // to start your own.
        let exported = home.join(".aiTerminal/themes/mine.toml");
        if let Ok(t) = std::fs::read_to_string(home.join(".aiTerminal/themes/nebula.toml")) {
            std::fs::write(&exported, t).expect("write a user theme");
        }
        assert_eq!(crate::cli::theme(&arg(&["mine"])), 0, "a theme dropped in the dir is selectable");

        // ── a job is made, listed, and cleared ────────────────────────────────
        assert_eq!(crate::cli::ai(&arg(&["job", "--", "echo", "roundtrip"])), 0);
        assert_eq!(crate::cli::ai(&arg(&["job"])), 0);
        assert_eq!(crate::cli::ai(&arg(&["job", "show", "last"])), 0);
        assert_eq!(crate::cli::ai(&arg(&["job", "log", "last"])), 0);
        assert_eq!(crate::cli::ai(&arg(&["job", "clear"])), 0);
    }

    #[test]
    fn the_extractor_reads_a_page_the_way_a_user_does() {
        // The audit is only as good as what it pulls out, so the parsing is pinned
        // separately: markup and prompt chrome are stripped, entities decoded.
        assert_eq!(plain("<span class=\"p\">❯</span> @theme <b>nebula</b>"), "❯ @theme nebula");
        assert_eq!(plain("@flow check &lt;name&gt;"), "@flow check <name>");
        assert_eq!(shell_words("@profile create \"Work Stuff\"").unwrap(), ["@profile", "create", "Work Stuff"]);
        assert_eq!(shell_words("@theme nebula").unwrap(), ["@theme", "nebula"]);
        assert!(shell_words("@profile create \"unbalanced").is_none());

        // Classification: real commands run, illustrations and model runs do not.
        assert_eq!(classify("@theme nebula"), Verdict::Run(vec!["theme".into(), "nebula".into()]));
        assert_eq!(classify("@flow check build"), Verdict::Run(vec!["ai".into(), "flow".into(), "check".into(), "build".into()]));
        assert!(matches!(classify("@flow check <name>"), Verdict::Skip(_)), "a placeholder is not a command");
        assert!(matches!(classify("@ai why is this failing"), Verdict::Skip(_)), "needs a model");
        assert!(matches!(classify("@coder \"fix the test\""), Verdict::Skip(_)), "an agent run needs a model");
        assert!(matches!(classify("@flow review the auth module"), Verdict::Skip(_)), "running a flow needs a model");
    }
}
