//! The guard, proved subject by subject.
//!
//! Nothing here runs a command, reads a credential or touches a real home directory: a
//! refused command is a string, a refused path is a string, and a "secret" is a value with
//! the right shape and no entropy. Policies are written in the product's own vocabulary,
//! so every test proves the parser as well as the rule.

use super::rules::{CommandRule, PathRule};
use super::*;

/// A guard with rules, and no machine underneath it.
fn guard(doc: &str) -> Guard {
    Guard::from_toml(doc)
}

/// A guard rooted at a made-up home, for the rules that are built from one.
fn at_home(doc: &str) -> (Guard, std::path::PathBuf) {
    let home = std::path::PathBuf::from("/nowhere/person");
    (Guard::rooted(doc, Base { home: Some(home.clone()), cwd: None }), home)
}

fn allowed(g: &Guard, act: Act) -> bool {
    matches!(g.judge(act), Decision::Allow)
}

fn refused(g: &Guard, act: Act) -> bool {
    matches!(g.judge(act), Decision::Deny { .. })
}

fn section(doc: &str) -> RuleSet {
    let parsed = corelib::wire::Toml::parse(doc).expect("a guard fixture parses");
    RuleSet::parse(parsed.get("guard").expect("a [guard] section"))
}

// ── commands ────────────────────────────────────────────────────────────────

#[test]
fn an_empty_guard_refuses_nothing() {
    let g = Guard::default();
    assert!(allowed(&g, Act::Run("wipe-the-lot --now")));
    assert!(allowed(&g, Act::Read(std::path::Path::new("/anything"))));
    assert!(allowed(&g, Act::Write(std::path::Path::new("/anything"))));
    assert!(g.briefing().is_empty(), "nothing to tell a model about");
}

#[test]
fn deny_beats_confirm_beats_the_allow_list() {
    let g = guard(
        r#"
[[guard.command]]
pattern = "^wipe"
rule = "deny"
[[guard.command]]
pattern = "^wipe|^tidy"
rule = "confirm"
[[guard.command]]
pattern = "^tidy|^wipe|^ls"
rule = "allow"
"#,
    );
    assert!(refused(&g, Act::Run("wipe everything")), "deny wins over both");
    assert!(matches!(g.judge(Act::Run("tidy up")), Decision::Confirm { .. }), "confirm wins over the allow-list");
    assert!(allowed(&g, Act::Run("ls -la")));
}

#[test]
fn an_allow_list_excludes_everything_else() {
    let g = guard("[[guard.command]]\npattern = \"^git\\\\b\"\nrule = \"allow\"\n");
    assert!(allowed(&g, Act::Run("git status")));
    let Decision::Deny { reason } = g.judge(Act::Run("ls")) else { panic!("an allow-list denies the rest") };
    assert!(reason.contains("not in the allow-list"), "{reason}");
}

#[test]
fn a_harmless_first_stage_cannot_shield_what_follows_it() {
    let g = guard("[[guard.command]]\npattern = \"^wipe\"\nrule = \"deny\"\n");
    // Every segment of every line is judged, so none of these hide behind `echo`.
    for line in ["echo hi && wipe everything", "echo hi | wipe", "echo hi; wipe", "echo hi\nwipe everything"] {
        assert!(refused(&g, Act::Run(line)), "not refused: {line:?}");
    }
    // …and an allow-list is per segment for the same reason.
    let only_ls = guard("[[guard.command]]\npattern = \"^ls\"\nrule = \"allow\"\n");
    assert!(refused(&only_ls, Act::Run("ls | wipe")));
}

#[test]
fn a_program_is_recognised_by_its_name_not_its_path() {
    let g = guard("[[guard.command]]\npattern = \"^sudo\\\\b\"\nrule = \"confirm\"\n");
    // The rule is anchored on the program, and the program is `sudo` however it was spelled.
    assert!(matches!(g.judge(Act::Run("/usr/bin/sudo apt install x")), Decision::Confirm { .. }));
    assert!(matches!(g.judge(Act::Run("\"sudo\" apt install x")), Decision::Confirm { .. }));
}

#[test]
fn a_refusal_names_the_rule_that_refused() {
    let g = guard("[[guard.command]]\npattern = \"^wipe\"\nrule = \"deny\"\n");
    let Decision::Deny { reason } = g.judge(Act::Run("wipe everything")) else { panic!("denied") };
    assert!(reason.contains("^wipe"), "the rule is named: {reason}");
    assert!(reason.contains("wipe everything"), "and so is what matched it: {reason}");
}

#[test]
fn the_auto_list_is_separate_from_the_tiers() {
    let g = guard(
        r#"
[[guard.command]]
pattern = "^(ls|cat)\\b"
rule = "auto"
[[guard.command]]
pattern = "^cat\\b"
rule = "confirm"
"#,
    );
    assert!(g.auto_runs("ls -la"));
    // On the auto list AND on the confirm list: auto mode would not prompt, the guard
    // still does. The two questions are asked separately and both answers stand.
    assert!(g.auto_runs("cat x"));
    assert!(matches!(g.judge(Act::Run("cat x")), Decision::Confirm { .. }));
    assert!(!g.auto_runs("wipe everything"), "not on the list");
    assert!(!g.auto_runs("ls | wipe"), "only as safe as its least-safe stage");
    assert!(!g.auto_runs(""), "an empty command qualifies for nothing");
}

#[test]
fn a_rule_word_nobody_recognises_is_read_the_strictest_way() {
    // A typo must never widen what may run.
    let g = guard("[[guard.command]]\npattern = \"^tidy\"\nrule = \"confrim\"\n");
    assert!(refused(&g, Act::Run("tidy up")));
}

#[test]
fn a_pattern_that_does_not_compile_is_skipped_and_the_rest_still_loads() {
    let set = section("[[guard.command]]\npattern = \"[unclosed\"\nrule = \"deny\"\n[[guard.command]]\npattern = \"^wipe\"\nrule = \"deny\"\n");
    let (g, skipped) = Guard::compile(&[&set], Base::default());
    assert_eq!(skipped.len(), 1, "one rule reported: {skipped:?}");
    assert!(skipped[0].contains("[unclosed"), "and it says which: {}", skipped[0]);
    assert!(refused(&g, Act::Run("wipe everything")), "the good rule still guards");
}

#[test]
fn an_empty_pattern_is_not_a_rule_at_all() {
    // An empty regex matches everywhere, so a templating slip must not become a rule that
    // refuses every command in the product.
    let g = guard("[[guard.command]]\npattern = \"\"\nrule = \"deny\"\n");
    assert!(allowed(&g, Act::Run("ls")));
}

// ── paths ───────────────────────────────────────────────────────────────────

#[test]
fn an_off_limits_path_is_neither_read_nor_written() {
    let g = guard("[[guard.path]]\npattern = \"/clients/\"\nrule = \"deny\"\n");
    let p = std::path::Path::new("/work/clients/acme/notes.md");
    assert!(refused(&g, Act::Read(p)));
    assert!(refused(&g, Act::Write(p)));
    assert!(allowed(&g, Act::Read(std::path::Path::new("/work/src/main.rs"))));
}

#[test]
fn a_read_only_path_is_read_and_never_changed() {
    let g = guard("[[guard.path]]\npattern = \"^/etc/\"\nrule = \"read-only\"\n");
    let p = std::path::Path::new("/etc/hosts");
    assert!(allowed(&g, Act::Read(p)));
    let Decision::Deny { reason } = g.judge(Act::Write(p)) else { panic!("a write is refused") };
    assert!(reason.contains("read-only"), "{reason}");
}

#[test]
fn a_path_allow_list_excludes_everywhere_else() {
    let g = guard("[[guard.path]]\npattern = \"^/work/\"\nrule = \"allow\"\n");
    assert!(allowed(&g, Act::Read(std::path::Path::new("/work/notes.md"))));
    assert!(refused(&g, Act::Read(std::path::Path::new("/elsewhere/notes.md"))));
}

#[test]
fn the_built_in_floor_holds_whatever_the_config_says() {
    // Nothing configured at all — these come from the guard itself, so disabling every
    // plugin cannot open them.
    let (g, home) = at_home("");
    for rel in [
        ".ssh/id_rsa",
        ".aws/credentials",
        ".gnupg/secring.gpg",
        ".config/gh/hosts.yml",
        ".aiTerminal/config.toml",
        ".aiTerminal/gates/telegram.toml",
    ] {
        assert!(refused(&g, Act::Read(&home.join(rel))), "readable: {rel}");
    }
    assert!(refused(&g, Act::Read(std::path::Path::new("/work/server.PEM"))), "an extension rule ignores case");
    assert!(refused(&g, Act::Read(std::path::Path::new("/work/deploy/id_ed25519"))));
    assert!(allowed(&g, Act::Read(&home.join("Documents/notes.md"))), "an ordinary file is ordinary");
    // `.env` is deliberately NOT here: reading it is the everyday case, and the vault is
    // what makes it safe.
    assert!(allowed(&g, Act::Read(&home.join("project/.env"))));
}

#[test]
fn a_home_with_regex_characters_in_it_is_still_matched_literally() {
    let home = std::path::PathBuf::from("/Users/a.b+c(1)");
    let g = Guard::rooted("", Base { home: Some(home.clone()), cwd: None });
    assert!(refused(&g, Act::Read(&home.join(".aws/credentials"))));
    // The dots and brackets in the home directory are literal, so a path that would match
    // them as wildcards is somebody else's home and not covered.
    assert!(allowed(&g, Act::Read(std::path::Path::new("/Users/axbxc_1_/.aws/credentials"))), "not a wildcard");
}

// ── paths named on a command line ───────────────────────────────────────────

#[test]
fn a_command_that_names_an_off_limits_path_is_refused() {
    let (g, _) = at_home("");
    let Decision::Deny { reason } = g.judge(Act::Run("cat ~/.ssh/id_rsa")) else {
        panic!("a path rule that only reached fs.* would be decoration")
    };
    assert!(reason.contains("~/.ssh/id_rsa"), "the refusal names the token: {reason}");
    assert!(refused(&g, Act::Run("grep -r secret /nowhere/person/.aws/credentials")));
}

#[test]
fn an_ordinary_command_is_not_mistaken_for_a_path() {
    let g = guard("[[guard.path]]\npattern = \"/clients/\"\nrule = \"deny\"\n");
    for line in ["grep -r TODO src/", "cargo test --all", "echo hello"] {
        assert!(allowed(&g, Act::Run(line)), "refused: {line:?}");
    }
    // A URL is not a path — the command rules are what govern where a command may reach.
    assert!(allowed(&g, Act::Run("git clone https://host/clients/repo.git")));
}

#[test]
fn a_relative_path_on_a_command_line_resolves_against_the_run_directory() {
    let g = Guard::rooted(
        "[[guard.path]]\npattern = \"^/work/vault/\"\nrule = \"deny\"\n",
        Base { home: None, cwd: Some("/work".into()) },
    );
    assert!(refused(&g, Act::Run("cat vault/keys.txt")));
    assert!(allowed(&g, Act::Run("cat src/main.rs")));
}

// ── secrets: out as a placeholder, back as itself ───────────────────────────

const SECRET_RULES: &str = r#"
[[guard.secret]]
pattern = "AKIA[0-9A-Z]+"
name = "aws-key"
[[guard.secret]]
pattern = "pw-[a-z0-9]+"
name = "db-password"
"#;

#[test]
fn a_secret_leaves_as_a_placeholder_and_comes_back_as_itself() {
    let g = guard(SECRET_RULES);
    let real = "AWS_ACCESS_KEY_ID=AKIAEXAMPLE\nDB=pw-example0";
    let hidden = g.hide(real);
    assert!(!hidden.contains("AKIAEXAMPLE"), "the value left the machine: {hidden}");
    assert!(!hidden.contains("pw-example0"));
    assert!(hidden.contains("\u{ab}aws-key-1\u{bb}"), "named by its rule: {hidden}");
    assert!(hidden.contains("\u{ab}db-password-1\u{bb}"));
    assert_eq!(g.vault().restore(&hidden).unwrap(), real, "and it round-trips exactly");
}

#[test]
fn the_same_secret_is_always_the_same_placeholder() {
    let g = guard(SECRET_RULES);
    let hidden = g.hide("AKIAEXAMPLE here and AKIAEXAMPLE again, plus AKIAOTHER");
    assert_eq!(hidden.matches("\u{ab}aws-key-1\u{bb}").count(), 2, "one value, one name: {hidden}");
    assert!(hidden.contains("\u{ab}aws-key-2\u{bb}"), "a different value gets a different name: {hidden}");
    assert_eq!(g.vault().len(), 2);
}

#[test]
fn a_placeholder_survives_being_written_into_a_command() {
    let g = guard(SECRET_RULES);
    let hidden = g.hide("DB_PASSWORD=pw-example0");
    // What the model writes back, with the placeholder used as a value.
    let token = hidden.trim_start_matches("DB_PASSWORD=");
    let ready = g.vault().restore(&format!("psql \"postgres://app:{token}@db.internal/prod\"")).unwrap();
    assert!(ready.contains("app:pw-example0@db.internal"), "the command can actually connect: {ready}");
}

#[test]
fn a_placeholder_from_another_run_is_refused_rather_than_run() {
    let g = guard(SECRET_RULES);
    let err = g.vault().restore("psql \u{ab}db-password-7\u{bb}").unwrap_err();
    assert!(err.contains("\u{ab}db-password-7\u{bb}"), "it says which: {err}");
    assert!(err.contains("another run"), "and why: {err}");
}

#[test]
fn text_with_nothing_of_ours_in_it_comes_back_untouched() {
    let g = guard(SECRET_RULES);
    // Guillemets are ordinary punctuation in plenty of languages, and «redacted» is ours
    // but carries no value — neither is mistaken for an unresolved placeholder.
    for text in ["il a dit \u{ab}bonjour\u{bb}", "KEY=\u{ab}redacted\u{bb}", "cargo test --all"] {
        assert_eq!(g.vault().restore(text).unwrap(), text);
    }
}

#[test]
fn rules_compose_so_a_key_takes_its_name_with_it() {
    let g = guard(
        r#"
[[guard.secret]]
pattern = "pw-[a-z0-9]+"
name = "db-password"
[[guard.secret]]
pattern = "(?i)password[\"']?\\s*[:=]\\s*\\S+"
name = "assignment"
"#,
    );
    let hidden = g.hide("DB_PASSWORD=pw-example0");
    assert!(!hidden.contains("pw-example0"));
    assert!(hidden.contains("\u{ab}assignment-1\u{bb}"), "the second rule took the name too: {hidden}");
}

#[test]
fn masking_is_for_the_screen_and_has_no_way_back() {
    let g = guard("[[guard.secret]]\npattern = \"pw-[a-z0-9]+\"\nscope = \"terminal\"\n");
    assert_eq!(g.mask("DB=pw-example0"), format!("DB={MASK}"));
    assert!(g.masks_display());
    // A display rule does not hide, and a hiding rule does not mask — the scope is the
    // whole of the difference.
    assert_eq!(g.hide("DB=pw-example0"), "DB=pw-example0");
    let ai = guard("[[guard.secret]]\npattern = \"pw-[a-z0-9]+\"\n");
    assert!(!ai.masks_display(), "the default scope is egress, so `cat .env` still shows you your own values");
}

#[test]
fn what_leaves_this_process_keeps_neither_the_secret_nor_the_placeholder() {
    // The window writes a session file the CLI reads back, and the CLI has a different
    // vault: a placeholder there could never be turned back into anything.
    let g = guard(SECRET_RULES);
    assert_eq!(g.scrub("AWS_ACCESS_KEY_ID=AKIAEXAMPLE"), format!("AWS_ACCESS_KEY_ID={MASK}"));
    assert_eq!(g.vault().len(), 0, "nothing was remembered, because nothing can come back");
}

#[test]
fn a_literal_rule_needs_no_escaping() {
    let g = guard("[[guard.secret]]\npattern = \"10.0.42.17\"\nliteral = true\nname = \"host\"\n");
    assert!(g.hide("ping 10.0.42.17").contains("\u{ab}host-1\u{bb}"));
    assert_eq!(g.hide("ping 10x0y42z17"), "ping 10x0y42z17", "a literal is not a pattern");
}

#[test]
fn clean_text_costs_nothing_and_comes_back_identical() {
    let g = guard(SECRET_RULES);
    let text = "cargo build --release";
    assert_eq!(g.hide(text), text);
    assert_eq!(g.mask(text), text);
    assert_eq!(g.vault().len(), 0);
}

#[test]
fn a_value_too_large_to_be_a_secret_is_masked_rather_than_vaulted() {
    let g = guard("[[guard.secret]]\npattern = \"BEGIN[\\\\s\\\\S]*END\"\nname = \"block\"\n");
    let huge = format!("BEGIN{}END", "x".repeat(9000));
    assert_eq!(g.hide(&huge), MASK, "a document that matched is not carried");
    assert_eq!(g.vault().len(), 0);
}

// ── what the model is told ──────────────────────────────────────────────────

#[test]
fn the_briefing_names_the_rules_and_explains_a_placeholder() {
    let g = guard(
        r#"
[[guard.command]]
pattern = "^wipe"
rule = "deny"
[[guard.path]]
pattern = "/clients/"
rule = "deny"
[[guard.secret]]
pattern = "pw-[a-z0-9]+"
name = "db-password"
"#,
    );
    let brief = g.briefing();
    assert!(brief.contains("^wipe"), "the command rule: {brief}");
    assert!(brief.contains("/clients/"), "the path rule: {brief}");
    assert!(brief.contains("db-password"), "what a placeholder can stand for: {brief}");
    assert!(brief.contains("PLACEHOLDER"), "and that it IS one: {brief}");
    assert!(brief.contains("stop and say"), "what to do when refused: {brief}");
}

#[test]
fn the_briefing_counts_what_it_does_not_name() {
    let doc: String = (0..12).map(|i| format!("[[guard.command]]\npattern = \"^cmd{i}\"\nrule = \"deny\"\n")).collect();
    let brief = guard(&doc).briefing();
    assert!(brief.contains("^cmd0"), "the first few are named");
    assert!(brief.contains("and 4 more"), "and the rest are counted: {brief}");
    assert!(!brief.contains("^cmd11"), "a policy does not spend the window describing itself");
}

#[test]
fn a_guard_with_only_secret_rules_still_explains_placeholders() {
    let brief = guard(SECRET_RULES).briefing();
    assert!(!brief.contains("Commands"), "there are no command rules to describe: {brief}");
    assert!(brief.contains("aws-key"), "but the placeholders must be explained: {brief}");
    assert!(brief.contains("db-password"));
}

// ── the shape of a refusal ──────────────────────────────────────────────────

#[test]
fn every_refusal_is_recognisable_as_one() {
    let g = guard("[[guard.command]]\npattern = \"^wipe\"\nrule = \"confirm\"\n");
    let err = g.permit(Act::Run("wipe everything")).unwrap_err();
    assert!(is_refusal(&err), "the loop has to be able to tell: {err}");
    assert!(err.contains("running \"wipe everything\""), "it says what was refused: {err}");
    assert!(!is_refusal("error: fs.read: no such file"), "an ordinary failure is not a refusal");
    // Nobody to ask means confirm is a refusal — and `judge` still reports it as confirm,
    // for the one caller (a person at a terminal) who can act on the difference.
    assert!(matches!(g.judge(Act::Run("wipe everything")), Decision::Confirm { .. }));
    assert!(g.permit(Act::Run("ls")).is_ok());
}

// ── the vocabulary ──────────────────────────────────────────────────────────

#[test]
fn one_parser_reads_the_rules_wherever_they_are_written() {
    let set = section(
        r#"
[[guard.command]]
pattern = "^wipe"
rule = "deny"
[[guard.path]]
pattern = "/clients/"
rule = "read-only"
[[guard.secret]]
pattern = "pw-x"
name = "db"
scope = "all"
literal = true
"#,
    );
    assert_eq!(set.commands.len(), 1);
    assert_eq!(set.commands[0].rule, CommandRule::Deny);
    assert_eq!(set.paths[0].rule, PathRule::ReadOnly);
    assert_eq!(set.secrets[0].scope, Scope::All);
    assert!(set.secrets[0].literal);
    assert!(!set.is_empty());
    // A document with no `[guard]` section says nothing about the guard.
    assert!(RuleSet::parse(&corelib::wire::Toml::Table(Vec::new())).is_empty());
}

#[test]
fn rule_sets_fold_in_order_so_a_users_own_rule_answers_first() {
    let mine = section("[[guard.command]]\npattern = \"^wipe .*\"\nrule = \"deny\"\n");
    let plugins = section("[[guard.command]]\npattern = \"^wipe\"\nrule = \"deny\"\n");
    let (g, skipped) = Guard::compile(&[&mine, &plugins], Base::default());
    assert!(skipped.is_empty());
    let Decision::Deny { reason } = g.judge(Act::Run("wipe everything")) else { panic!("denied") };
    assert!(reason.contains("^wipe .*"), "the user's rule is the one named: {reason}");
}
