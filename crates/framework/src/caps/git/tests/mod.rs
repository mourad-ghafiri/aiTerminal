use super::*;

#[test]
fn parses_every_address_form() {
    let g = parse("https://github.com/anthropics/claude-code").unwrap();
    assert_eq!(g.clone_url, "https://github.com/anthropics/claude-code.git");
    assert_eq!(g.name, "anthropics/claude-code");
    assert_eq!(g.reff, None);
    assert_eq!(g.path, "");

    let t = parse("https://github.com/o/r/tree/develop/src/lib").unwrap();
    assert_eq!(t.clone_url, "https://github.com/o/r.git");
    assert_eq!(t.reff.as_deref(), Some("develop"));
    assert_eq!(t.path, "src/lib");

    let b = parse("https://github.com/o/r/blob/main/docs/GUIDE.md").unwrap();
    assert_eq!(b.reff.as_deref(), Some("main"));
    assert_eq!(b.path, "docs/GUIDE.md");

    let gl = parse("https://gitlab.com/group/sub/proj/-/tree/v2/pkg").unwrap();
    assert_eq!(gl.clone_url, "https://gitlab.com/group/sub/proj.git");
    assert_eq!(gl.reff.as_deref(), Some("v2"));
    assert_eq!(gl.path, "pkg");

    let scp = parse("git@github.com:o/r.git").unwrap();
    assert_eq!(scp.clone_url, "git@github.com:o/r.git");
    assert_eq!(scp.host, "github.com");
    assert_eq!(scp.name, "o/r");

    let dotgit = parse("https://example.com/team/repo.git").unwrap();
    assert_eq!(dotgit.clone_url, "https://example.com/team/repo.git");

    // The canonical fragment round-trips branch + path.
    let canon = parse("https://github.com/o/r.git#feature/x:a/b").unwrap();
    assert_eq!(canon.reff.as_deref(), Some("feature/x"));
    assert_eq!(canon.path, "a/b");
    assert_eq!(canon.canonical(), "https://github.com/o/r.git#feature/x:a/b");

    // Plain non-git http is NOT a repo (stays external).
    assert!(parse("https://example.com/blog/post").is_none());
    assert!(parse("md://~/notes").is_none());

    // A bare owner (profile page) or the host root is NOT a repo — needs `owner/repo`.
    assert!(parse("https://github.com/mourad-ghafiri").is_none());
    assert!(parse("https://github.com/mourad-ghafiri/").is_none());
    assert!(parse("https://github.com").is_none());
    assert!(parse("https://gitlab.com/just-a-user").is_none());
    // …but an explicit `.git` at the root is still a repo.
    assert!(parse("https://git.example.com/repo.git").is_some());
}

#[test]
fn relative_links_move_within_the_repo() {
    let base = "https://github.com/o/r.git#main:docs";
    let a = resolve("guide.md", base).unwrap();
    assert_eq!(a.path, "docs/guide.md");
    assert_eq!(a.reff.as_deref(), Some("main"));
    let up = resolve("../src", base).unwrap();
    assert_eq!(up.path, "src");
}

#[test]
fn find_readme_is_case_insensitive_and_prefers_md() {
    let dir = std::env::temp_dir().join(format!("tt-git-readme-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("README.txt"), "txt").unwrap();
    std::fs::write(dir.join("Readme.md"), "# md").unwrap();
    let found = find_readme(&dir).unwrap();
    assert_eq!(found.file_name().unwrap().to_string_lossy().to_lowercase(), "readme.md");
    let _ = std::fs::remove_dir_all(&dir);
}

/// End-to-end over a LOCAL repo (no network). Skips cleanly if `git` isn't installed.
#[test]
fn browses_a_local_repo_readme_branches_and_folders() {
    if Command::new("git").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("skipping: git not installed");
        return;
    }
    let root = std::env::temp_dir().join(format!("tt-git-src-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("README.md"), "# Root Readme\n").unwrap();
    std::fs::write(root.join("docs").join("readme.MD"), "# Docs Readme\n").unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "init"]);
    git(&["checkout", "-q", "-b", "dev"]);
    std::fs::write(root.join("README.md"), "# Dev Readme\n").unwrap();
    git(&["commit", "-qam", "dev"]);
    git(&["checkout", "-q", "main"]);

    let addr = parse(&root.to_string_lossy()).expect("local repo parses");
    assert!(addr.local);
    let page = git_fetch(&addr, false).expect("fetch local repo");
    assert_eq!(page.get("doc").and_then(Json::as_str), Some("# Root Readme\n"));
    let repo = page.get("repo").unwrap();
    let branches: Vec<&str> = repo.get("branches").unwrap().as_array().unwrap().iter().filter_map(Json::as_str).collect();
    assert!(branches.contains(&"main") && branches.contains(&"dev"), "branches: {branches:?}");

    // Switch folder → the docs README (case-insensitive `.MD`).
    let docs = resolve("docs", &addr.canonical()).unwrap();
    let dpage = git_fetch(&docs, false).unwrap();
    assert_eq!(dpage.get("doc").and_then(Json::as_str), Some("# Docs Readme\n"));

    // Switch branch → the dev README.
    let dev = GitAddress { reff: Some("dev".into()), ..addr.clone() };
    let devpage = git_fetch(&dev, false).unwrap();
    assert_eq!(devpage.get("doc").and_then(Json::as_str), Some("# Dev Readme\n"));

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(repo_dir(&addr.clone_url));
}
