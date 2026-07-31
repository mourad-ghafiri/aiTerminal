use super::*;

#[test]
fn same_root_same_id_different_root_differs() {
    let a = derive_id(Path::new("/work/my project"));
    let a2 = derive_id(Path::new("/work/my project"));
    let b = derive_id(Path::new("/work/other"));
    assert_eq!(a, a2, "same path → stable id");
    assert_ne!(a, b, "different path → different id");
    // Id is filesystem-safe (only alphanumerics + '-').
    assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'), "id is path-safe: {a}");
    // Two same-named folders in different places never collide.
    assert_ne!(derive_id(Path::new("/a/proj")), derive_id(Path::new("/b/proj")));
}

#[test]
fn resolve_root_walks_up_to_the_git_top_level() {
    let tmp = std::env::temp_dir().join(format!("aiterm-sess-{}", std::process::id()));
    let repo = tmp.join("repo");
    let sub = repo.join("src/deep");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    assert_eq!(resolve_root(&sub), repo, "a subdir resolves to the repo root");
    assert_eq!(resolve_root(&repo), repo);
    // Outside any repo → the folder itself.
    let bare = tmp.join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    assert_eq!(resolve_root(&bare), bare);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn record_run_appends_and_bumps_meta() {
    let base = std::env::temp_dir().join(format!("aiterm-sess-rec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let cwd = base.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let s = Session::at(&cwd, &base.join("sessions"));
    s.record_run("@ai", "list files", "ls -la");
    s.record_run("@coder", "fix the parser\nwith details", "done in 2 steps");
    let digest = s.digest();
    assert!(digest.contains("list files → ls -la"));
    assert!(digest.contains("@coder"));
    // The multi-line prompt was flattened to a single line (one entry per run).
    assert!(digest.contains("fix the parser with details"), "prompt flattened: {digest:?}");
    assert_eq!(digest.lines().count(), 2, "one line per run");
    // meta.toml tracks the run count and the real root.
    let meta = std::fs::read_to_string(base.join("sessions").join(&s.id).join("meta.toml")).unwrap();
    let doc = corelib::wire::Toml::parse(&meta).unwrap();
    assert_eq!(doc.get("runs").and_then(|v| v.as_int()), Some(2));
    assert_eq!(doc.get("root").and_then(|v| v.as_str()), Some(cwd.to_string_lossy().as_ref()));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn digest_stays_under_the_cap() {
    let base = std::env::temp_dir().join(format!("aiterm-sess-cap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let cwd = base.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let s = Session::at(&cwd, &base.join("sessions"));
    for i in 0..2000 {
        s.record_run("@ai", &format!("request number {i} with some words"), &format!("some command {i}"));
    }
    let digest = s.digest();
    assert!(digest.len() <= DIGEST_MAX, "digest bounded: {}", digest.len());
    assert!(digest.contains("request number 1999"), "the newest run survives");
    assert!(!digest.contains("request number 0 "), "the oldest run was trimmed");
    let _ = std::fs::remove_dir_all(&base);
}
