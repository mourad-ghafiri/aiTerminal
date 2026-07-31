use super::*;
use crate::test_home::lock_home;

#[test]
fn slug_is_filesystem_safe() {
    assert_eq!(slug("Work Stuff"), "work-stuff");
    assert_eq!(slug("  Déjà!! Vu  "), "d-j-vu");
    assert_eq!(slug("../etc"), "etc");
    assert_eq!(slug("!!!"), "profile");
    assert!(!is_valid_id("../etc"));
    assert!(!is_valid_id(""));
    assert!(is_valid_id("work-stuff"));
}

#[test]
fn ensure_default_then_crud_round_trips() {
    let (_h, _home) = lock_home("profiles-crud");
    ensure_default();
    // The default exists and is active by fallback.
    assert!(read_profile(DEFAULT_ID).is_some());
    assert_eq!(active_id(), DEFAULT_ID);
    assert_eq!(list().len(), 1);

    // Create a second profile with an emoji.
    let p = create("Work Stuff", "🛠").unwrap();
    assert_eq!(p.id, "work-stuff");
    assert_eq!(p.emoji, "🛠");
    assert_eq!(list().len(), 2);

    // Rename + re-emoji.
    update(&p.id, "Work", "💼").unwrap();
    let p2 = read_profile(&p.id).unwrap();
    assert_eq!(p2.name, "Work");
    assert_eq!(p2.emoji, "💼");

    // Switch makes it active + bumps last_opened.
    set_active(&p.id).unwrap();
    assert_eq!(active_id(), "work-stuff");
    assert!(active().get("active").and_then(|v| v.as_bool()).unwrap_or(false));

    // Can't delete the active one; can after switching back.
    assert!(delete(&p.id).is_err());
    set_active(DEFAULT_ID).unwrap();
    delete(&p.id).unwrap();
    assert_eq!(list().len(), 1);
    // Never the last one.
    assert!(delete(DEFAULT_ID).is_err());
}

#[test]
fn an_active_pointer_at_a_profile_that_is_gone_heals() {
    // A profile directory can vanish without going through `delete` — a `git clean`,
    // a sync tool, somebody tidying `~`. The pointer then names nothing, and every
    // read after it would resolve to a directory that is not there. It has to fall
    // back to a real profile rather than hand out a broken id forever.
    let (_h, _home) = lock_home("profiles-heal");
    ensure_default();
    let p = create("Work", "\u{1f4bc}").unwrap();
    set_active(&p.id).unwrap();
    assert_eq!(active_id(), "work");

    std::fs::remove_dir_all(profile_dir("work").unwrap()).unwrap();
    assert_eq!(active_id(), DEFAULT_ID, "it fell back to a profile that exists");
    // And the pointer file itself is still the stale one — healing is a READ-time
    // rule, so nothing has to be repaired before the terminal can start.
    assert!(read_profile("work").is_none());
}

#[test]
fn an_id_can_never_reach_outside_the_profiles_directory() {
    // `profiles/<id>` is joined from a value that reaches this from a config file, a
    // pointer file and a command line. A traversal here would delete or overwrite an
    // arbitrary directory, so the charset is the whole defence and it is checked at
    // the ONE place a path is built.
    for bad in ["..", ".", "../etc", "a/b", "a\\b", "Work", "work stuff", "", "wörk", "a:b"] {
        assert!(profile_dir(bad).is_none(), "{bad:?} produced a path");
        assert!(config_path(bad).is_none(), "{bad:?} produced a config path");
        assert!(workspace_path(bad).is_none(), "{bad:?} produced a workspace path");
    }
    for good in ["work", "work-stuff", "side_project", "p2"] {
        assert!(profile_dir(good).is_some(), "{good:?} was refused");
    }
}

#[test]
fn switching_to_something_that_is_not_a_profile_changes_nothing() {
    let (_h, _home) = lock_home("profiles-switch-bogus");
    ensure_default();
    for bad in ["nope", "..", "work"] {
        assert!(set_active(bad).is_err(), "{bad:?} was accepted");
        assert_eq!(active_id(), DEFAULT_ID, "after {bad:?}");
    }
    // And renaming or deleting one that is not there is an error, not a silent no-op
    // that leaves somebody believing it worked.
    assert!(update("nope", "Whatever", "").is_err());
    assert!(delete("nope").is_err());
}

#[test]
fn a_name_with_nothing_usable_in_it_still_gets_a_usable_id() {
    // The id is a slug of a name a person typed, and people type emoji, punctuation
    // and non-Latin scripts. A directory called "" is not a thing.
    let (_h, _home) = lock_home("profiles-odd-names");
    ensure_default();
    assert_eq!(create("---", "").unwrap().id, "profile");
    assert_eq!(create("!!!", "").unwrap().id, "profile-2");
    assert_eq!(create("  Work  ", "").unwrap().id, "work", "surrounding space is not part of a name");
    // An empty name is refused outright: there is nothing to call it.
    assert!(create("   ", "").is_err());
    assert!(create("", "").is_err());
}

#[test]
fn duplicate_names_get_unique_ids() {
    let (_h, _home) = lock_home("profiles-dup");
    ensure_default();
    let a = create("Side Project", "🚀").unwrap();
    let b = create("Side Project", "🚀").unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(a.id, "side-project");
    assert_eq!(b.id, "side-project-2");
}

#[test]
fn config_overlay_round_trips() {
    let (_h, _home) = lock_home("profiles-cfg");
    ensure_default();
    let p = create("Dark", "🌙").unwrap();
    config_set(&p.id, "appearance", "theme", "\"daylight\"").unwrap();
    let text = std::fs::read_to_string(config_path(&p.id).unwrap()).unwrap();
    assert!(text.contains("theme = \"daylight\""), "{text}");
}

#[test]
fn active_id_falls_back_to_latest_when_pointer_missing() {
    let (_h, _home) = lock_home("profiles-latest");
    ensure_default();
    let p = create("Newer", "✨").unwrap();
    // No pointer written yet; the just-created profile is the most recent.
    let _ = std::fs::remove_file(active_path());
    assert_eq!(active_id(), p.id);
}
