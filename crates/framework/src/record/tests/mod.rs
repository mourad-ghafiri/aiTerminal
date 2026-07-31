use super::*;

#[test]
fn an_id_can_never_escape_its_root() {
    let root = Path::new("/tmp/records");
    assert_eq!(folder(root, "1700000000-42"), Some(root.join("1700000000-42")));
    // Everything a traversal needs is outside the charset.
    for bad in ["..", "../etc", "a/b", ".hidden", "", "a b", "a.md"] {
        assert_eq!(folder(root, bad), None, "{bad:?} must be refused");
    }
}

#[test]
fn a_named_file_is_held_to_the_same_rule_as_a_folder() {
    let dir = Path::new("/tmp/records/1700000000-42");
    assert_eq!(child(dir, "nodes", "verify", "md"), Some(dir.join("nodes").join("verify.md")));
    assert_eq!(child(dir, "nodes", "build_web-2", "md"), Some(dir.join("nodes").join("build_web-2.md")));
    // A node id comes out of a file someone edited, so it gets the same refusal.
    for bad in ["..", "../etc", "a/b", ".hidden", "", "a b", "a.md"] {
        assert_eq!(child(dir, "nodes", bad, "md"), None, "{bad:?} must be refused");
    }
}

#[test]
fn logs_rotate_oldest_first() {
    let dir = std::env::temp_dir().join(format!("tt-record-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for i in 1..=5 {
        let (path, _f) = open_log(&dir, "runs", 3).unwrap();
        assert!(path.ends_with(format!("{i}.md")), "sequence keeps counting up");
    }
    let kept: Vec<String> =
        logs(&dir, "runs").iter().filter_map(|p| p.file_name()?.to_str().map(str::to_string)).collect();
    assert_eq!(kept, vec!["3.md", "4.md", "5.md"], "kept the newest three");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_reference_resolves_by_last_exact_or_any_unique_piece() {
    let ids: Vec<String> = ["600-2", "500-1"].iter().map(|s| s.to_string()).collect();
    assert_eq!(resolve(&ids, "last", "loop").unwrap(), "600-2", "newest first");
    assert_eq!(resolve(&ids, "500-1", "loop").unwrap(), "500-1");
    assert_eq!(resolve(&ids, "60", "loop").unwrap(), "600-2", "a prefix");
    assert_eq!(resolve(&ids, "2", "loop").unwrap(), "600-2", "the tail people retype");
    assert!(resolve(&ids, "nope", "loop").unwrap_err().contains("no such loop"));
    // Empty is a question, not a match-everything.
    assert!(resolve(&ids, "", "loop").unwrap_err().contains("which loop?"));
    assert!(resolve(&ids, "  ", "loop").unwrap_err().contains("which loop?"));
    assert!(resolve(&ids, "0", "loop").unwrap_err().contains("matches 2"));
    assert!(resolve(&[], "last", "loop").unwrap_err().contains("no loops yet"));
}

#[test]
fn ages_read_at_a_glance() {
    assert_eq!(human_age(45), "45s");
    assert_eq!(human_age(95), "1m");
    assert_eq!(human_age(4000), "1h");
    assert_eq!(human_age(200_000), "2d");
}
