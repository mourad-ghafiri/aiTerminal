use super::*;

fn vars(pairs: &[(&str, &str)]) -> Vars {
    let mut v = Vars::default();
    for (k, val) in pairs {
        v.set(k, *val);
    }
    v
}

#[test]
fn template_interpolates_and_escapes() {
    let v = vars(&[("user", "ada")]);
    assert_eq!(render_template("hi {user}!", &v), "hi ada!");
    assert_eq!(render_template("{missing}", &v), "");
    assert_eq!(render_template("{{x}}", &v), "{x}");
}

#[test]
fn transforms_pipeline() {
    let tr = Transforms { trim: true, strip_prefix: Some("ref: refs/heads/".into()), ..Default::default() };
    assert_eq!(apply_transforms("ref: refs/heads/main\n".into(), &tr), "main");

    let dirty = Transforms { map_nonempty: Some("\u{25CF}".into()), ..Default::default() };
    assert_eq!(apply_transforms(" M file\n".into(), &dirty), "\u{25CF}");
    assert_eq!(apply_transforms("".into(), &dirty), "");

    let color = Transforms { map_nonempty: Some("#f2c05c".into()), default: Some("#57d97b".into()), ..Default::default() };
    assert_eq!(apply_transforms("1".into(), &color), "#f2c05c");
    assert_eq!(apply_transforms("".into(), &color), "#57d97b");

    let field = Transforms { field: Some(1), ..Default::default() };
    assert_eq!(apply_transforms("2 5".into(), &field), "5");
}

/// Parse a plugin manifest from the repo's `builtin/` registry data (the same
/// files a user installs into ~/.aiTerminal/plugins/). Nothing is embedded.
fn registry_plugin(name: &str) -> Manifest {
    let p = format!("{}/../../builtin/plugins/{name}/plugin.toml", env!("CARGO_MANIFEST_DIR"));
    Manifest::parse(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p}: {e}"))).unwrap()
}

#[test]
fn registry_plugins_parse_and_compose() {
    let mut r = PluginRegistry::new();
    for n in ["git", "common", "dir", "prompt", "ai-guard"] {
        r.add_trusted(registry_plugin(n));
    }
    // git contributes a healthy alias set
    assert!(r.aliases().len() >= 20, "git+essentials should expose many aliases");
    assert!(r.aliases().iter().any(|(k, v)| k == "gst" && v == "git status"));
}

#[test]
fn every_builtin_plugin_parses_and_loads() {
    // Guard rail for the whole shipped plugin set: every `builtin/plugins/<name>/plugin.toml`
    // must parse and load (with its shell.zsh/shell.bash siblings) into a registry. Catches a
    // malformed manifest before it ever reaches a user's shell.
    let dir = format!("{}/../../builtin/plugins", env!("CARGO_MANIFEST_DIR"));
    let mut r = PluginRegistry::new();
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {dir}: {e}")) {
        let path = entry.unwrap().path().join("plugin.toml");
        if !path.exists() {
            continue;
        }
        let m = Manifest::load_from(&path).unwrap_or_else(|e| panic!("load {path:?}: {e}"));
        r.add_trusted(m);
        count += 1;
    }
    assert_eq!(count, 30, "the terminal ships exactly 30 builtin plugins, got {count}");
    // The whole set composes: aliases/abbreviations/snippets all resolve without panicking.
    let _ = r.aliases();
    let _ = r.abbreviations();
    let _ = r.completions();
    let _ = r.shell_snippets(false);
    let _ = r.shell_snippets(true);
}

#[test]
fn every_builtin_alias_and_abbr_name_is_a_valid_shell_identifier() {
    // The shell renderer SILENTLY DROPS any alias/abbr whose NAME contains a character outside
    // `[A-Za-z0-9._-]` (see `shell::alias_lines`). So a typo'd name (e.g. `gca!`) would vanish
    // with no error. This guard makes that a hard failure at the source instead.
    let is_ident = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    let dir = format!("{}/../../builtin/plugins", env!("CARGO_MANIFEST_DIR"));
    let mut r = PluginRegistry::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path().join("plugin.toml");
        if path.exists() {
            r.add_trusted(Manifest::load_from(&path).unwrap());
        }
    }
    for (name, _) in r.aliases() {
        assert!(is_ident(&name), "alias name `{name}` would be silently dropped by the shell renderer");
    }
    for (trigger, _) in r.abbreviations() {
        assert!(is_ident(&trigger), "abbreviation trigger `{trigger}` would be silently dropped");
    }
    // Sanity: the enrichment landed — a representative branch-aware alias and a big set are present.
    let aliases = r.aliases();
    assert!(aliases.iter().any(|(k, v)| k == "gcom" && v.contains("git_main_branch")), "git main-branch alias present");
    assert!(aliases.iter().filter(|(k, _)| k.starts_with('g')).count() >= 90, "git ships a comprehensive alias set");
    assert!(aliases.iter().filter(|(k, _)| k.starts_with('k')).count() >= 80, "kubernetes ships a comprehensive alias set");
}

#[test]
fn no_builtin_alias_collides_with_a_shell_function() {
    // zsh raises a hard "defining function based on alias" PARSE ERROR when an alias is in scope
    // and a later-sourced snippet declares a same-named function. The integration renders all
    // aliases BEFORE sourcing snippets, so any such overlap breaks the whole shell init. Forbid it
    // at the source (this is exactly the `yt` alias-vs-YouTube-function regression).
    let dir = format!("{}/../../builtin/plugins", env!("CARGO_MANIFEST_DIR"));
    let mut r = PluginRegistry::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path().join("plugin.toml");
        if path.exists() {
            r.add_trusted(Manifest::load_from(&path).unwrap());
        }
    }
    // A leading `name()` / `name ()` function definition on a snippet line.
    let fn_name = |t: &str| -> Option<String> {
        let t = t.trim_start();
        let id: String = t.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if id.is_empty() {
            return None;
        }
        t[id.len()..].trim_start().starts_with("()").then_some(id)
    };
    let aliases: std::collections::HashSet<String> = r.aliases().into_iter().map(|(k, _)| k).collect();
    for (plugin, body) in r.shell_snippets(false).into_iter().chain(r.shell_snippets(true)) {
        for line in body.lines() {
            if let Some(name) = fn_name(line) {
                assert!(!aliases.contains(&name), "alias `{name}` collides with the `{name}()` function in plugin `{plugin}` — zsh would refuse to source the integration");
            }
        }
    }
}


#[test]
fn shell_snippets_are_trust_gated_and_dialect_aware() {
    let mut zsh_only = Manifest::parse("name = \"feat\"\nversion = \"1\"\ndescription = \"\"\n").unwrap();
    zsh_only.shell_zsh = Some("echo zsh-snippet".into());
    zsh_only.shell_bash = Some("echo bash-snippet".into());
    let mut untrusted = Manifest::parse("name = \"evil\"\nversion = \"1\"\ndescription = \"\"\n").unwrap();
    untrusted.shell_zsh = Some("rm -rf /".into());

    let mut r = PluginRegistry::new();
    r.add_trusted(zsh_only);
    r.add_untrusted(untrusted); // shell code from an untrusted plugin must NOT run

    // Trusted snippet is returned; the dialect flag picks zsh vs bash.
    assert_eq!(r.shell_snippets(false), vec![("feat".into(), "echo zsh-snippet".into())]);
    assert_eq!(r.shell_snippets(true), vec![("feat".into(), "echo bash-snippet".into())]);
    // Untrusted plugin's snippet is never present (like `exec`).
    assert!(r.shell_snippets(false).iter().all(|(n, _)| n != "evil"));

    // Disabling the trusted plugin drops its snippet too.
    r.set_enabled("feat", false);
    assert!(r.shell_snippets(false).is_empty());
}

#[test]
fn common_plugin_contributes_cli_completion() {
    let mut r = PluginRegistry::new();
    r.add_trusted(registry_plugin("common"));
    let spec = r.completions().into_iter().find(|c| c.command == "aiTerminal").expect("CLI completion spec");
    assert!(spec.subcommands.contains(&"plugin".to_string()));
    assert!(spec.subcommands.contains(&"theme".to_string()));
}


#[test]
fn declarative_var_providers_evaluate() {
    // A plugin that derives a value purely declaratively (literal + from + transform).
    let m = Manifest::parse(
        "name = \"t\"\n\
         [[var]]\nid = \"x\"\nliteral = \"hello world\"\nfield = 1\n\
         [[var]]\nid = \"y\"\nfrom = \"x\"\nmap_nonempty = \"#57d97b\"\n\
         [[segment]]\nalign=\"left\"\nwhen=\"x\"\ntemplate=\"{x}\"\nfg=\"{y}\"\n",
    )
    .unwrap();
    let mut r = PluginRegistry::new();
    r.add(m, true);
    let ctx = Context { cwd: PathBuf::from("/tmp"), home: PathBuf::from("/tmp"), columns: 80, host: None };
    let v = r.evaluate(&ctx);
    assert_eq!(v.get("x"), "world");
    assert_eq!(v.get("y"), "#57d97b");
    let line = r.status_line(&v);
    assert_eq!(line.left[0].text, "world");
    assert_eq!(line.left[0].fg, Some("#57d97b".to_string()));
}

#[test]
fn parses_keybinding_contributions() {
    let m = Manifest::parse(
        "name = \"custom\"\n\
         [[keybinding]]\nkey = \"cmd+shift+x\"\naction = \"cycle_tab_bar\"\n",
    )
    .unwrap();
    assert_eq!(m.keybindings, vec![Keybinding { key: "cmd+shift+x".into(), action: "cycle_tab_bar".into() }]);

    // Aggregated only from ENABLED plugins.
    let mut r = PluginRegistry::new();
    r.add(m, false);
    assert_eq!(r.keybindings().len(), 1);
    r.set_enabled("custom", false);
    assert!(r.keybindings().is_empty());
}

#[test]
fn a_plugin_writes_guard_rules_in_the_same_vocabulary_a_config_does() {
    use crate::guard::rules::{CommandRule, PathRule};
    let m = Manifest::parse(
        "name = \"sec\"\n\
         [[guard.command]]\npattern = \"^ls\"\nrule = \"allow\"\n\
         [[guard.command]]\npattern = \"^tidy\"\nrule = \"deny\"\n\
         [[guard.command]]\npattern = \"^git\\\\s+status\"\nrule = \"auto\"\n\
         [[guard.path]]\npattern = \"/clients/\"\nrule = \"read-only\"\n\
         [[guard.secret]]\npattern = \"TOKEN\"\nname = \"token\"\n",
    )
    .unwrap();
    assert_eq!(m.guard.commands[0].rule, CommandRule::Allow);
    assert_eq!(m.guard.commands[1].pattern, "^tidy");
    assert_eq!(m.guard.commands[2].rule, CommandRule::Auto);
    assert_eq!(m.guard.paths[0].rule, PathRule::ReadOnly);
    assert_eq!(m.guard.secrets[0].name, "token");
    // Aggregated only from ENABLED plugins — which is why the rules that must never be
    // switched off are the guard's own floor rather than data a plugin supplies.
    let mut r = PluginRegistry::new();
    r.add(m, false);
    assert_eq!(r.guard_rules().commands.len(), 3);
    assert_eq!(r.guard_rules().secrets.len(), 1);
    r.set_enabled("sec", false);
    assert!(r.guard_rules().is_empty());
}

#[test]
fn load_dir_orders_plugins_alphabetically_by_name() {
    // `read_dir` is filesystem-ordered; `load_dir` must impose a deterministic (alphabetical)
    // order so shell snippets source predictably — `autosuggest` before `syntax-highlight`, which
    // relies on registering its ZLE hook LAST to own `region_highlight`.
    let dir = std::env::temp_dir().join(format!("tt-plugorder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Write files in a deliberately NON-alphabetical filename order.
    for name in ["syntax-highlight", "autosuggest", "git"] {
        std::fs::write(dir.join(format!("{name}.toml")), format!("name = \"{name}\"\n")).unwrap();
    }
    let mut r = PluginRegistry::new();
    r.load_dir(&dir);
    let names = r.names();
    assert_eq!(names, vec!["autosuggest".to_string(), "git".to_string(), "syntax-highlight".to_string()]);
    // The coupling the shell plugins depend on: autosuggest sources before syntax-highlight.
    let ai = names.iter().position(|n| n == "autosuggest").unwrap();
    let sh = names.iter().position(|n| n == "syntax-highlight").unwrap();
    assert!(ai < sh, "autosuggest must load before syntax-highlight");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exec_is_gated_to_trusted_plugins() {
    let m = Manifest::parse(
        "name = \"u\"\n[[var]]\nid = \"out\"\nexec = \"echo SHOULD_NOT_RUN\"\n",
    )
    .unwrap();
    let mut r = PluginRegistry::new();
    r.add(m, false); // untrusted
    let ctx = Context { cwd: PathBuf::from("/tmp"), home: PathBuf::from("/tmp"), columns: 80, host: None };
    let v = r.evaluate(&ctx);
    assert_eq!(v.get("out"), "", "untrusted exec must be skipped");
}

#[test]
fn example_plugin_manifest_parses() {
    // The shipped example must always match the live schema — it is the template
    // users copy. Parse it and spot-check each declared surface.
    let path = format!("{}/../../examples/plugin/plugin.toml", env!("CARGO_MANIFEST_DIR"));
    let m = Manifest::load_from(std::path::Path::new(&path)).expect("examples/plugin parses");
    assert_eq!(m.name, "hello");
    assert!(m.aliases.iter().any(|(k, _)| k == "hi"), "alias declared");
    assert!(!m.segments.is_empty(), "status segment declared");
    assert!(!m.completions.is_empty(), "completion declared");
    assert!(!m.guard.commands.is_empty(), "a guard command rule declared");
    assert!(!m.guard.secrets.is_empty(), "a guard secret rule declared");
    assert!(!m.keybindings.is_empty(), "keybinding declared");
}

#[test]
fn load_registry_is_ui_free_and_honours_config() {
    // The UI-independent loader: bundled builtins load, [plugins] disabled turns
    // one off, and the master switch empties the registry. Hermetic ($HOME lock).
    let (_h, _home) = crate::test_home::lock_home("plugin-load-registry");
    let mut cfg = crate::config::Config::default();
    let reg = crate::plugin::load_registry(&cfg);
    let names = reg.names();
    assert!(names.iter().any(|n| n == "git"), "bundled git plugin loads: {names:?}");
    assert!(names.iter().any(|n| n == "ai-terminal"), "the @-command plugin loads");
    // Disabling by config actually disables (its aliases stop contributing).
    cfg.plugins_disabled = vec!["git".into()];
    let reg = crate::plugin::load_registry(&cfg);
    assert!(!reg.aliases().iter().any(|(k, _)| k == "gst"), "disabled plugin contributes nothing");
    // Master switch off → empty registry.
    cfg.plugins_enabled = false;
    assert!(crate::plugin::load_registry(&cfg).names().is_empty());
}

#[test]
fn turning_a_plugin_off_stops_it_without_losing_it() {
    // `@plugin disable git` REMOVED git from the product: the registry skipped disabled
    // plugins at load, so it left `names()`, the listing dropped from 31 to 30,
    // `@plugin info` said it did not exist, and `@plugin enable` had nothing to enable.
    // The only way back was hand-editing a file nobody had been told about.
    //
    // `git` is the subject because it contributes BOTH aliases and a shell snippet — a
    // plugin that contributes neither would let every "it really stopped" assertion
    // below pass without proving anything.
    let (_h, _home) = crate::test_home::lock_home("plugin-disable-round-trip");
    crate::config::Config::ensure_default();
    let cfg = crate::config::Config::default();
    let store = crate::plugin::store::PluginStore::at(crate::config::Config::plugins_dir());

    let reg = crate::plugin::load_registry(&cfg);
    let before = reg.loaded().len();
    assert!(reg.aliases().iter().any(|(k, _)| k == "gst"), "git contributes `gst` while on");
    assert!(reg.shell_snippets(false).iter().any(|(n, _)| n == "git"), "and a zsh snippet");

    store.set_enabled("git", false).expect("disable");
    let reg = crate::plugin::load_registry(&cfg);

    // Still there, still countable, and honestly marked.
    assert_eq!(reg.loaded().len(), before, "the count does not move when one is turned off");
    let row = reg.loaded().into_iter().find(|(n, ..)| n == "git").expect("git is still known");
    assert!(!row.4, "…and reported as off");

    // The half that MUST hold: off means off. Losing this would trade a visible bug for
    // a silent one — a plugin the user turned off still reaching their shell.
    assert!(!reg.aliases().iter().any(|(k, _)| k == "gst"), "a disabled plugin contributes no alias");
    assert!(!reg.shell_snippets(false).iter().any(|(n, _)| n == "git"), "and no shell snippet");
    assert!(!reg.completions().iter().any(|c| c.command == "git"), "and no completion");

    // And it comes back.
    store.set_enabled("git", true).expect("enable");
    let reg = crate::plugin::load_registry(&cfg);
    assert!(reg.loaded().into_iter().any(|(n, _, _, _, on)| n == "git" && on), "re-enabled");
    assert!(reg.aliases().iter().any(|(k, _)| k == "gst"), "and contributing again");
}

#[test]
fn both_ways_of_turning_a_plugin_off_agree() {
    // Two routes existed with opposite defects: `@plugin disable` hid the plugin
    // entirely, while `[plugins] disabled` left it listed as though it were running.
    // Whichever a user reaches for, the answer has to be the same one.
    let (_h, _home) = crate::test_home::lock_home("plugin-disable-routes");
    crate::config::Config::ensure_default();
    let store = crate::plugin::store::PluginStore::at(crate::config::Config::plugins_dir());
    let look = |reg: &crate::plugin::PluginRegistry| {
        (
            reg.loaded().len(),
            reg.loaded().into_iter().find(|(n, ..)| n == "git").map(|r| r.4),
            reg.aliases().iter().any(|(k, _)| k == "gst"),
        )
    };

    let mut cfg = crate::config::Config::default();
    cfg.plugins_disabled = vec!["git".into()];
    let by_config = look(&crate::plugin::load_registry(&cfg));

    store.set_enabled("git", false).expect("disable");
    let by_store = look(&crate::plugin::load_registry(&crate::config::Config::default()));

    assert_eq!(by_config, by_store, "the two routes must be indistinguishable");
    assert_eq!(by_config.1, Some(false), "both report it off");
    assert!(!by_config.2, "and both actually stop it");
}

#[test]
fn builtin_plugins_are_complete_and_consistent() {
    // The completeness contract for every BUNDLED plugin: full metadata, shell-safe
    // alias/abbr/completion identifiers (the generator silently skips invalid ones —
    // this catches the typo instead), resolvable keybindings, segment templates that
    // only reference declared or engine-supplied vars, snippet dialect parity, and
    // no stale app-era surfaces. Read-only over the repo's builtin/ — hermetic.
    let dir = format!("{}/../../builtin/plugins", env!("CARGO_MANIFEST_DIR"));
    // Vars the engine supplies to every template (see plugin/eval.rs probe_context).
    let engine_vars = [
        "cwd.full", "cwd.short", "dir.name", "home", "user", "os", "host", "host.short",
        "time.hm", "time.hms", "date.ymd",
    ];
    // zsh-only snippets are allowed ONLY for zsh-specific features (ZLE widgets,
    // global aliases); everything else must ship both dialects.
    let zsh_only = ["autosuggest", "syntax-highlight", "common"];
    let ident = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c));

    let mut seen = 0;
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    for pdir in entries {
        let toml = pdir.join("plugin.toml");
        if !toml.exists() {
            continue;
        }
        seen += 1;
        let name = pdir.file_name().unwrap().to_string_lossy().to_string();
        let raw = std::fs::read_to_string(&toml).unwrap();
        for stale in ["[[command]]", "[[bookmark]]", "[[view]]"] {
            assert!(!raw.contains(stale), "{name}: stale app-era `{stale}` block");
        }
        let m = Manifest::load_from(&toml).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(m.name, name, "{name}: manifest name matches its folder");
        assert!(!m.version.trim().is_empty(), "{name}: missing version");
        assert!(!m.description.trim().is_empty(), "{name}: missing description");
        for (k, v) in &m.aliases {
            assert!(ident(k), "{name}: alias name {k:?} is not shell-safe (would be silently skipped)");
            assert!(!v.trim().is_empty(), "{name}: alias {k} has an empty expansion");
        }
        for (t, e) in &m.abbreviations {
            assert!(ident(t), "{name}: abbr trigger {t:?} is not shell-safe");
            assert!(!e.is_empty(), "{name}: abbr {t} has an empty expansion");
        }
        for c in &m.completions {
            assert!(ident(&c.command), "{name}: completion command {:?}", c.command);
            assert!(!c.subcommands.is_empty() || !c.flags.is_empty(), "{name}: completion for {} has no candidates", c.command);
        }
        for kb in &m.keybindings {
            assert!(corelib::types::Chord::parse(&kb.key).is_some(), "{name}: unparseable chord {:?}", kb.key);
            assert!(crate::gui::Action::from_name(&kb.action).is_some(), "{name}: unknown action {:?}", kb.action);
        }
        // Segment templates + gates only reference vars that will actually resolve.
        let known = |id: &str| m.vars.iter().any(|v| v.id == id) || engine_vars.contains(&id);
        for seg in &m.segments {
            let mut rest = seg.template.as_str();
            while let Some(i) = rest.find('{') {
                rest = &rest[i + 1..];
                if let Some(j) = rest.find('}') {
                    let var = &rest[..j];
                    assert!(known(var), "{name}: segment references undeclared var {{{var}}}");
                    rest = &rest[j + 1..];
                } else {
                    break;
                }
            }
            if let Some(w) = &seg.when {
                assert!(known(w), "{name}: segment `when` references undeclared var {w:?}");
            }
        }
        // Dialect parity: a feature snippet must serve bash users too.
        let (z, b) = (m.shell_zsh.is_some(), m.shell_bash.is_some());
        if z && !zsh_only.contains(&name.as_str()) {
            assert!(b, "{name}: has shell.zsh but no shell.bash (dialect parity)");
        }
        assert!(!(b && !z), "{name}: shell.bash without shell.zsh");
    }
    assert!(seen >= 28, "the full builtin plugin set is present, got {seen}");
}
