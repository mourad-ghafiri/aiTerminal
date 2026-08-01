use crate::cli::config::config;
use crate::cli::flow::args::{flow_usage, view_word};
use crate::cli::plugin::unknown_plugin;
use crate::cli::profile::{profile, profile_switch};
use crate::cli::theme::{theme, theme_set};

#[test]
fn profile_switch_resolves_id_and_name_in_both_forms() {
    let (_h, _home) = crate::test_home::lock_home("cli-profile-switch");
    crate::config::Config::ensure_default();
    let p = crate::profile::create("Hamid", "🚀").unwrap();
    // By display name, case-insensitive — the exact confusion users hit.
    assert_eq!(profile_switch("Hamid"), 0);
    assert_eq!(crate::profile::active_id(), p.id);
    assert_eq!(profile_switch("default"), 0);
    // The `switch` verb goes through the SAME resolver (name works there too).
    assert_eq!(profile(&["switch".to_string(), "HAMID".to_string()]), 0);
    assert_eq!(crate::profile::active_id(), p.id);
    // Unknown → clear error pointing at @profile.
    assert_eq!(profile_switch("nope"), 2);
}

#[test]
fn theme_set_updates_the_active_profile_and_validates() {
    let (_h, _home) = crate::test_home::lock_home("cli-theme-set");
    crate::config::Config::ensure_default();
    // A known theme (case-insensitive) lands in the ACTIVE profile's overlay and
    // becomes the effective config.
    assert_eq!(theme_set("Graphite"), 0);
    assert_eq!(crate::config::Config::load().theme, "graphite", "overlay applies via Config::load");
    // Another profile keeps its own look after switching.
    let p = crate::profile::create("Rose", "🌹").unwrap();
    crate::profile::set_active(&p.id).unwrap();
    assert_eq!(theme_set("pink"), 0);
    assert_eq!(crate::config::Config::load().theme, "pink");
    crate::profile::set_active(crate::profile::DEFAULT_ID).unwrap();
    assert_eq!(crate::config::Config::load().theme, "graphite", "per-profile themes are independent");
    // An unknown name is rejected with the available list, and changes nothing.
    assert_eq!(theme_set("no-such-theme"), 2);
    assert_eq!(crate::config::Config::load().theme, "graphite");
}

#[test]
fn a_theme_that_will_not_parse_is_refused_not_applied() {
    // `theme_set` checked that the FILENAME existed and wrote the name straight into the
    // profile. `resolve` then fell back to midnight at render time, so the name you picked
    // was in your config, the window was a different theme, and nothing anywhere said so.
    let (_h, _home) = crate::test_home::lock_home("cli-theme-broken");
    crate::config::Config::ensure_default();
    assert_eq!(theme_set("graphite"), 0);

    let dir = crate::config::Config::themes_dir();
    std::fs::write(dir.join("broken.toml"), "[[[[\nname =\n").unwrap();
    std::fs::write(dir.join("unterm.toml"), "name = \"unterminated\n").unwrap();

    for name in ["broken", "unterm"] {
        assert_eq!(theme_set(name), 2, "{name} is not a theme this can apply");
        assert_eq!(
            crate::config::Config::load().theme,
            "graphite",
            "and the profile still names the theme that works after trying {name}",
        );
        assert!(crate::config::Config::theme_problem(name).is_some(), "the listing marks {name}");
    }

    // What it must NOT refuse: the parser resolves every token on its own, so a file that
    // sets some of them is a real theme. Refusing these would break the same rule the
    // config keeps — missing keys fall back, a partial file is fine — and would turn a
    // fix for silent breakage into a new way to be told no.
    std::fs::write(dir.join("partial.toml"), "name = \"partial\"\naccent = \"#ff8800\"\n").unwrap();
    std::fs::write(dir.join("odd.toml"), "fg = \"notacolour\"\n").unwrap();
    for name in ["partial", "odd"] {
        assert_eq!(crate::config::Config::theme_problem(name), None, "{name} parses");
        assert_eq!(theme_set(name), 0, "{name} applies");
        assert_eq!(crate::config::Config::load().theme, name);
    }
}

#[test]
fn config_refuses_a_word_it_does_not_know() {
    // `@config paht` printed the whole config and exited 0, so the typo looked like
    // a real subcommand.
    let (_h, _home) = crate::test_home::lock_home("cli-config-subcommand");
    let a = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(config(&a(&[])), 0, "bare shows the config");
    assert_eq!(config(&a(&["path"])), 0);
    assert_eq!(config(&a(&["paht"])), 2, "a typo is refused, not ignored");
}

#[test]
fn exporting_a_theme_that_does_not_exist_is_refused_not_substituted() {
    // `@theme export typo` printed a complete, valid MIDNIGHT and exited 0, because
    // `resolve_theme` falls back — which is right for a config naming a deleted theme
    // (the window must still open) and wrong here. People saved that file, edited it,
    // and could not work out why their theme was not their theme.
    let (_h, _home) = crate::test_home::lock_home("cli-theme-export");
    crate::config::Config::ensure_default();

    let t = |args: &[&str]| theme(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(t(&["export", "no-such-theme"]), 2, "an unknown name is an error");
    assert_eq!(t(&["export", "nebula"]), 0, "a real one still exports");
    // Case-insensitive, like every other theme lookup.
    assert_eq!(t(&["export", "Nebula"]), 0);
    assert_eq!(t(&["export"]), 2, "and the usage still guards a missing name");
}

#[test]
fn a_plugin_subcommand_refuses_a_name_that_is_not_a_plugin() {
    // `@plugin enable nosuchplugin` reported success and wrote the typo into the
    // config list, where it sat doing nothing. And `@plugin remove git` said "not
    // found" about a plugin that was loaded and working — it is bundled, which is a
    // different thing entirely and points somewhere else to look.
    let (_h, _home) = crate::test_home::lock_home("cli-plugin-names");
    crate::config::Config::ensure_default();
    let store = crate::plugin::store::PluginStore::open_default().expect("store");

    assert!(unknown_plugin(&store, "definitely-not-a-plugin").is_some());
    assert!(
        unknown_plugin(&store, "definitely-not-a-plugin").unwrap().contains("no plugin"),
        "it says what is wrong"
    );
    // A bundled plugin is known, so enable/disable/remove all treat it as real.
    assert_eq!(unknown_plugin(&store, "git"), None, "bundled plugins exist");
}

#[test]
fn every_usage_line_is_a_command_somebody_could_type() {
    // Usage text is the one documentation a person reads at the moment they are
    // stuck, and it drifts silently: a verb gets added, renamed or removed and the
    // help still lists the old one. So every verb the parser accepts must appear in
    // the usage, and every `@flow` line in the usage must start with `@flow`.
    let usage = flow_usage();
    for verb in ["check", "graph", "runs", "show", "nodes", "node", "watch", "log", "resume", "retry", "clear"] {
        assert!(usage.contains(&format!("@flow {verb}")), "`{verb}` is not in the usage:\n{usage}");
    }
    for line in usage.lines() {
        let body = line.trim_start_matches("usage:").trim();
        assert!(body.starts_with("@flow"), "a usage line that is not a command: {line:?}");
    }
    // And the flags, which are the part people guess at.
    for flag in ["--bg", "--dry-run", "--timeout", "--budget", "--concurrency", "--view"] {
        assert!(usage.contains(flag), "`{flag}` is not in the usage:\n{usage}");
    }
}

#[test]
fn a_refused_flag_says_what_it_would_have_accepted() {
    // A model never reads these; a person does, at the moment they typed something
    // slightly wrong. "invalid value" costs them a second guess.
    let err = view_word("tree").unwrap_err();
    assert!(err.contains("graph") && err.contains("list"), "{err}");
    assert!(err.contains("tree"), "it repeats what was typed: {err}");
    // Case and surrounding space are a person's, not a mistake.
    assert_eq!(view_word(" GRAPH ").unwrap(), "graph");
    assert_eq!(view_word("List").unwrap(), "list");
}
