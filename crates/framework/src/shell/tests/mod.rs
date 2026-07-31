use super::*;

fn ctx(aliases: Vec<(String, String)>, snippets: Vec<(String, String)>) -> Integration {
    Integration { aliases, abbrs: Vec::new(), completions: Vec::new(), snippets }
}

/// A scratch dir that behaves like `<home>/.aiTerminal/shell`.
fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("tt-shell-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn our_own_directory_is_never_mistaken_for_the_users() {
    // The bug: we set ZDOTDIR to our directory for every shell we spawn, so launching
    // the app from inside one of its own shells made `ZDOTDIR` — and therefore
    // `TT_REAL_ZDOTDIR` — point at us. Each generated file then sourced itself.
    let zdir = scratch("real").join("zsh");
    std::fs::create_dir_all(&zdir).unwrap();

    // Inherited ZDOTDIR is ours → fall back to $HOME, never to ourselves.
    std::env::set_var("ZDOTDIR", zdir.to_string_lossy().to_string());
    std::env::remove_var("TT_REAL_ZDOTDIR");
    assert_eq!(real_zdotdir(&zdir), home(), "ours must be refused");
    // …and so is anything nested inside it.
    std::env::set_var("ZDOTDIR", zdir.join("deeper").to_string_lossy().to_string());
    assert_eq!(real_zdotdir(&zdir), home());

    // A genuine user ZDOTDIR is respected.
    std::env::set_var("ZDOTDIR", "/Users/somebody/dotfiles");
    assert_eq!(real_zdotdir(&zdir), "/Users/somebody/dotfiles");

    // An inherited TT_REAL_ZDOTDIR wins — a parent already worked it out.
    std::env::set_var("TT_REAL_ZDOTDIR", "/Users/somebody/elsewhere");
    assert_eq!(real_zdotdir(&zdir), "/Users/somebody/elsewhere");
    // Unless it too was poisoned, in which case it is refused like any other.
    std::env::set_var("TT_REAL_ZDOTDIR", zdir.to_string_lossy().to_string());
    assert_eq!(real_zdotdir(&zdir), "/Users/somebody/dotfiles", "falls through to ZDOTDIR");

    // Empty is not an answer.
    std::env::set_var("TT_REAL_ZDOTDIR", "");
    std::env::set_var("ZDOTDIR", "  ");
    assert_eq!(real_zdotdir(&zdir), home());
    std::env::remove_var("ZDOTDIR");
    std::env::remove_var("TT_REAL_ZDOTDIR");
}

#[test]
fn a_generated_file_refuses_to_source_itself() {
    // Belt and braces: the guard lives in the file too, so a copy written with the old
    // value stops recursing when it is read rather than when it is regenerated.
    let dir = scratch("guard");
    Zsh.prepare(&dir, &ctx(Vec::new(), Vec::new()));
    let zdir = dir.join("zsh");
    for hook in [".zshenv", ".zprofile", ".zlogin", ".zshrc"] {
        let text = std::fs::read_to_string(zdir.join(hook)).unwrap();
        assert!(text.contains("TT_OWN_ZDOTDIR="), "{hook} knows its own directory");
        assert!(
            text.contains("if [ \"$TT_REAL_ZDOTDIR\" != \"$TT_OWN_ZDOTDIR\" ]"),
            "{hook} guards the chain: {text}"
        );
    }
}

/// The reproducer, in a real shell: point `TT_REAL_ZDOTDIR` at the generated directory
/// itself and source `.zshenv`. Before the guard this printed "job table full or
/// recursion limit exceeded" until zsh gave up.
#[test]
fn sourcing_a_generated_file_terminates_even_when_pointed_at_itself() {
    if !std::path::Path::new("/bin/zsh").exists() {
        return; // nothing to prove without zsh
    }
    let dir = scratch("recurse");
    Zsh.prepare(&dir, &ctx(Vec::new(), Vec::new()));
    let zdir = dir.join("zsh");
    let out = std::process::Command::new("/bin/zsh")
        .arg("-c")
        .arg(format!("source {}/.zshenv && echo OK", zdir.display()))
        .env("TT_REAL_ZDOTDIR", &zdir)
        .env("ZDOTDIR", &zdir)
        .env("HOME", &dir)
        .output()
        .expect("zsh runs");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!text.contains("recursion limit"), "it recursed: {text}");
    assert!(!text.contains("job table full"), "it recursed: {text}");
    assert!(text.contains("OK"), "it did not finish: {text}");
}

#[test]
fn detects_shell_kind() {
    assert_eq!(ShellKind::detect("/bin/zsh"), ShellKind::Zsh);
    assert_eq!(ShellKind::detect("/usr/local/bin/bash"), ShellKind::Bash);
    assert_eq!(ShellKind::detect("/usr/bin/fish"), ShellKind::Other);
}

#[test]
fn alias_block_quotes_and_filters() {
    let al = vec![
        ("g".into(), "git".into()),
        ("gcm".into(), "git commit -m".into()),
        ("weird name".into(), "nope".into()), // skipped (space in name)
        ("q".into(), "echo 'hi'".into()),     // single-quote escaped
    ];
    let out = alias_lines(&al);
    assert!(out.contains("alias g='git'\n"));
    assert!(out.contains("alias gcm='git commit -m'\n"));
    assert!(!out.contains("weird name"));
    assert!(out.contains(r#"alias q='echo '\''hi'\'''"#));
}

#[test]
fn git_plugin_aliases_reach_the_zsh_init() {
    // The reported bug: `g` → command not found. The git plugin's aliases must land
    // in the generated init (which `.zshrc` sources).
    let git = crate::plugin::Manifest::parse(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../builtin/plugins/git/plugin.toml"
    )))
    .unwrap();
    let mut reg = PluginRegistry::new();
    reg.add_trusted(git);
    let init = zsh_integration(&Integration {
        aliases: reg.aliases(),
        abbrs: reg.abbreviations(),
        completions: reg.completions(),
        snippets: reg.shell_snippets(false),
    });
    assert!(init.contains("alias g='git'"), "{init}");
    assert!(init.contains("alias gst='git status'"));
    assert!(init.contains("__tt_abbr[gcam]="), "the gcam abbreviation should be present");
}

#[test]
fn enriched_plugin_aliases_and_helpers_render_in_both_dialects() {
    // Smoke test over the WHOLE builtin set: representative new aliases must reach the init
    // (none silently dropped) and the helper functions that branch-aware aliases call must be
    // injected — in BOTH zsh and bash, so `$(git_main_branch)` resolves at run time everywhere.
    let dir = format!("{}/../../builtin/plugins", env!("CARGO_MANIFEST_DIR"));
    let mut reg = PluginRegistry::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path().join("plugin.toml");
        if p.exists() {
            reg.add_trusted(crate::plugin::Manifest::load_from(&p).unwrap());
        }
    }
    for bash in [false, true] {
        let snippets = reg.shell_snippets(bash);
        let ctx = Integration { aliases: reg.aliases(), abbrs: reg.abbreviations(), completions: reg.completions(), snippets };
        let init = if bash { bash_integration(&ctx) } else { zsh_integration(&ctx) };
        let dia = if bash { "bash" } else { "zsh" };
        for a in ["alias gcom=", "alias kgpa=", "alias dxcit=", "alias cnext=", "alias naud="] {
            assert!(init.contains(a), "{a} missing from {dia} init");
        }
        for f in ["git_main_branch()", "dsh()", "pyclean()", "ghcd()"] {
            assert!(init.contains(f), "{f} helper missing from {dia} init");
        }
    }
}

#[test]
fn full_builtin_integration_parses_in_the_real_shell() {
    // The reported startup crash was `defining function based on alias 'yt'`: an alias clashing
    // with a snippet's function of the same name. Generate the COMPLETE builtin integration and
    // parse it with the real `zsh -n` / `bash -n` — the definitive guard against any clash or
    // syntax slip reaching a user's shell. Hermetic (a temp file); skips a missing shell.
    let dir = format!("{}/../../builtin/plugins", env!("CARGO_MANIFEST_DIR"));
    let mut reg = PluginRegistry::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path().join("plugin.toml");
        if p.exists() {
            reg.add_trusted(crate::plugin::Manifest::load_from(&p).unwrap());
        }
    }
    for (sh, bash) in [("zsh", false), ("bash", true)] {
        if !matches!(std::process::Command::new(sh).arg("-c").arg("exit 0").status(), Ok(s) if s.success()) {
            eprintln!("skipping: {sh} not available");
            continue;
        }
        let ctx = Integration { aliases: reg.aliases(), abbrs: reg.abbreviations(), completions: reg.completions(), snippets: reg.shell_snippets(bash) };
        let init = if bash { bash_integration(&ctx) } else { zsh_integration(&ctx) };
        let tmp = std::env::temp_dir().join(format!("tt-integ-{}-{sh}", std::process::id()));
        std::fs::write(&tmp, &init).unwrap();
        let out = std::process::Command::new(sh).arg("-n").arg(&tmp).output().unwrap();
        assert!(out.status.success(), "`{sh} -n` rejected the generated integration:\n{}", String::from_utf8_lossy(&out.stderr));
        // `-n` parses but never EXECUTES, so aliases defined earlier in the
        // file are not live while later lines parse — it is blind to the
        // global-alias class of bug (common's `H` = `| head` once rewrote a
        // later snippet into `… F | head` and aborted the whole file). Source
        // the integration for real in an interactive shell, hermetic HOME.
        if !bash {
            let home = std::env::temp_dir().join(format!("tt-integ-home-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&home);
            let out = std::process::Command::new(sh)
                .args(["-f", "-i", "-c"])
                .arg(format!("source '{}'", tmp.display()))
                .env("HOME", &home)
                .env("ZDOTDIR", &home)
                .output()
                .unwrap();
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success() && !err.contains("parse error"),
                "sourcing the zsh integration in an interactive shell failed:\n{err}"
            );
            let _ = std::fs::remove_dir_all(&home);
        }
        let _ = std::fs::remove_file(&tmp);
    }
}

/// End-to-end guard for the alias-hints index: a generated `tt_alias_index` must
/// actually be *retrievable* when sourced by a real zsh. A text assertion alone missed
/// the regression where `a['git']=…` stored the key WITH quotes, so `${a[git]}` was
/// empty and no hint ever fired. Skips cleanly where zsh isn't installed.
#[test]
fn alias_index_resolves_in_real_zsh() {
    let zsh = std::process::Command::new("zsh").arg("-c").arg("exit 0").status();
    if !matches!(zsh, Ok(s) if s.success()) {
        eprintln!("skipping: zsh not available");
        return;
    }
    let idx = tt_alias_index(&[
        ("gst".into(), "git status".into()),
        ("gcm".into(), "git commit -m".into()),
        ("g".into(), "git".into()),
    ]);
    // Source the index, then print the bucket the snippet would look up for `git …`.
    let script = format!("{idx}\nprint -r -- \"${{TT_ALIAS_BY_HEAD[git]}}\"\n");
    let out = std::process::Command::new("zsh").arg("-c").arg(&script).output().expect("run zsh");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("git commit -m"), "bucket must resolve under the bare `git` key, got: {stdout:?}\n{idx}");
    assert!(stdout.contains("gcm") && stdout.contains("gst"), "rows present: {stdout:?}");
}

#[test]
fn engine_exports_theme_and_sources_plugin_snippets() {
    // The integration SOURCES the live colors file (so a theme switch recolors
    // running shells) and installs the per-prompt refresher in both dialects.
    let zsh_init = zsh_integration(&ctx(Vec::new(), Vec::new()));
    assert!(zsh_init.contains("colors.sh") && zsh_init.contains("_tt_refresh_colors"), "{zsh_init}");
    assert!(zsh_init.contains("precmd_functions+=(_tt_refresh_colors)"));
    let bash_init = bash_integration(&ctx(Vec::new(), Vec::new()));
    assert!(bash_init.contains("colors.sh") && bash_init.contains("_tt_refresh_colors"), "{bash_init}");
    assert!(bash_init.contains("PROMPT_COMMAND"));
    // ... the alias forward index (head-bucketed, longest-first) ...
    let idx = tt_alias_index(&[
        ("gst".into(), "git status".into()),
        ("gs".into(), "git status".into()),
        ("g".into(), "git".into()),
        ("xx".into(), "x".into()), // alias no shorter than expansion → dropped
    ]);
    assert!(idx.contains("typeset -gA TT_ALIAS_BY_HEAD"));
    // One subscript assignment per head; rows are longest-expansion-first (so the
    // 2-token `git status` rows precede the 1-token `git`), shorter alias name first
    // (`gs` before `gst`), ANSI-C quoted so bash + zsh both parse it. The key is
    // UNQUOTED: `a['git']=…` would store under the literal key `'git'` in zsh, so
    // `${a[git]}` would miss — the exact regression that silently broke alias-hints.
    assert!(
        idx.contains(r"TT_ALIAS_BY_HEAD[git]=$'gs\tgit status\ngst\tgit status\ng\tgit'"),
        "head-bucketed longest-first index, unquoted key: {idx}"
    );
    assert!(!idx.contains("['git']="), "the assoc key must be unquoted (zsh stores quotes literally): {idx}");
    assert!(!idx.contains("[x]="), "alias no shorter than its expansion is dropped: {idx}");
    // ... and sources each plugin snippet under a header (engine stays generic).
    let init = zsh_integration(&ctx(Vec::new(), vec![("history".into(), "HISTSIZE=50000\n".into())]));
    assert!(init.contains("# --- plugin: history ---"));
    assert!(init.contains("HISTSIZE=50000"));
}

#[test]
fn completion_map_emits_zsh_data() {
    let specs = vec![
        CompletionSpec {
            command: "aiTerminal".into(),
            subcommands: vec!["plugin".into(), "theme".into()],
            flags: vec!["--command".into()],
        },
        CompletionSpec { command: "nope".into(), subcommands: vec![], flags: vec![] }, // no candidates → dropped
    ];
    let out = tt_completion_map(&specs);
    assert!(out.contains("typeset -gA TT_COMPL_SUB TT_COMPL_FLAGS"));
    assert!(out.contains("'aiTerminal' 'plugin theme'"), "subcommands joined: {out}");
    assert!(out.contains("'aiTerminal' '--command'"), "flags joined: {out}");
    assert!(!out.contains("'nope'"), "empty specs are dropped: {out}");
    // Generated data must parse as zsh.
    assert!(out.contains("TT_COMPL_SUB=("));
    // Nothing usable → empty (no stray declarations).
    assert!(tt_completion_map(&[]).is_empty());
}

#[test]
fn color_env_emits_themed_ls_colors() {
    let env = color_env(&corelib::theme::midnight());
    let get = |k: &str| env.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.clone()).unwrap();
    assert_eq!(get("CLICOLOR"), "1");
    let gnu = get("LS_COLORS");
    assert!(gnu.contains("di=38;2;"), "directory truecolor: {gnu}");
    assert!(gnu.contains(":*.png=38;2;"), "image extension mapped");
    assert!(gnu.contains(":*.rs=38;2;"), "code extension mapped");
    assert_eq!(get("LSCOLORS").len(), 22, "BSD LSCOLORS is 11 fg/bg pairs");
}

#[test]
fn integration_off_is_bare() {
    let mut cfg = Config::default();
    cfg.shell_integration = false;
    let reg = PluginRegistry::new();
    let theme = corelib::theme::midnight();
    assert_eq!(prepare(&cfg, &reg, &theme, "/bin/zsh"), ShellSpawn::bare());
}

#[test]
fn colors_file_writes_exports_both_shells_can_source() {
    let (_h, _home) = crate::test_home::lock_home("shell-colors");
    let theme = corelib::theme::midnight();
    write_colors_file(&theme).unwrap();
    let path = colors_path();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(&format!("export TT_ACCENT='{}'", hex(theme.accent))));
    assert!(text.contains("export LS_COLORS=") && text.contains("export LSCOLORS="));
    // Both real shells must SOURCE it cleanly (skip where not installed).
    for sh in ["zsh", "bash"] {
        if !matches!(std::process::Command::new(sh).arg("-c").arg("exit 0").status(), Ok(st) if st.success()) {
            continue;
        }
        let script = format!(". {} && printf '%s' \"$TT_ACCENT\"", sh_squote(&path.to_string_lossy()));
        let out = std::process::Command::new(sh).arg("-c").arg(&script).output().unwrap();
        assert!(out.status.success(), "{sh} sources colors.sh");
        assert_eq!(String::from_utf8_lossy(&out.stdout), hex(theme.accent), "{sh} sees the accent");
    }
    // A different theme rewrites the SAME file with different values (the live switch).
    let day = crate::config::Config::resolve_theme("graphite");
    write_colors_file(&day).unwrap();
    let text2 = std::fs::read_to_string(&path).unwrap();
    assert_ne!(text, text2, "a theme switch changes the file running shells re-source");
}
