use super::*;
use super::backends::{glob_match, html_to_markdown, is_blocked_ip, is_secret_path, ssrf_resolve, url_host_port};

#[test]
fn glob_matches_star_doublestar_question() {
    let m = |p: &str, t: &str| glob_match(p.as_bytes(), t.as_bytes());
    assert!(m("*.rs", "main.rs"));
    assert!(!m("*.rs", "src/main.rs")); // * does not cross /
    assert!(m("src/*.rs", "src/main.rs"));
    assert!(m("**/*.rs", "src/a/b/main.rs")); // ** crosses /
    assert!(m("**/*.rs", "main.rs")); // ** matches zero segments
    assert!(m("src/**", "src/a/b.txt"));
    assert!(m("a?c.txt", "abc.txt"));
    assert!(!m("a?c.txt", "a/c.txt")); // ? does not match /
    assert!(!m("*.rs", "main.toml"));
}

fn ctx() -> CapCtx {
    CapCtx { policy: Arc::new(crate::security::Policy::new()), app_data: None, remote_enabled: true, origin: String::new(), sandbox: None }
}




fn ctx_ws(root: &std::path::Path) -> CapCtx {
    CapCtx { policy: Arc::new(crate::security::Policy::new()), app_data: None, remote_enabled: true, origin: String::new(), sandbox: Some(root.to_path_buf()) }
}

fn tmpdir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("tt-caps-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn fs_write_edit_delete_inside_workspace() {
    let ws = tmpdir("fswrite");
    let c = ctx_ws(&ws);
    let file = ws.join("sub").join("note.txt"); // parent created by fs.write
    let path = file.display().to_string();
    run("fs.write", &[("path".into(), path.clone()), ("content".into(), "hello world".into())], &c).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");
    // single literal edit
    run("fs.edit", &[("path".into(), path.clone()), ("find".into(), "world".into()), ("replace".into(), "there".into())], &c).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello there");
    // an ambiguous edit errors unless all=true
    std::fs::write(&file, "x x x").unwrap();
    assert!(run("fs.edit", &[("path".into(), path.clone()), ("find".into(), "x".into()), ("replace".into(), "y".into())], &c).is_err());
    run("fs.edit", &[("path".into(), path.clone()), ("find".into(), "x".into()), ("replace".into(), "y".into()), ("all".into(), "true".into())], &c).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "y y y");
    // mkdir + delete
    run("fs.mkdir", &[("path".into(), ws.join("d").display().to_string())], &c).unwrap();
    assert!(ws.join("d").is_dir());
    run("fs.delete", &[("path".into(), path)], &c).unwrap();
    assert!(!file.exists());
    let _ = std::fs::remove_dir_all(&ws);
}


#[test]
fn fs_edit_and_write_return_a_unified_diff() {
    let ws = tmpdir("fsdiff");
    let c = ctx_ws(&ws);
    let file = ws.join("main.rs");
    let path = file.display().to_string();
    // A fresh write diffs against empty (every line added).
    let r = run("fs.write", &[("path".into(), path.clone()), ("content".into(), "fn main() {\n    let x = compute();\n}\n".into())], &c).unwrap();
    let diff = r.get("diff").and_then(Json::as_str).unwrap();
    assert!(diff.contains("```diff") && diff.contains("+fn main()"), "write diff: {diff}");
    // An edit diffs old→new with +/- lines, labelled by the workspace-relative path.
    let r = run("fs.edit", &[("path".into(), path), ("find".into(), "compute()".into()), ("replace".into(), "compute().await?".into())], &c).unwrap();
    let diff = r.get("diff").and_then(Json::as_str).unwrap();
    assert!(diff.contains("- ") && diff.contains("+ "), "edit diff shows +/-: {diff}");
    assert!(diff.contains("compute().await?") && diff.contains("main.rs"));
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn fs_search_greps_literal_and_regex_under_the_workspace() {
    let ws = tmpdir("fssearch");
    std::fs::create_dir_all(ws.join("src")).unwrap();
    std::fs::write(ws.join("src/a.rs"), "fn one() {}\n// TODO: fix\nfn two() {}\n").unwrap();
    std::fs::write(ws.join("src/b.rs"), "let TODO = 1;\n").unwrap();
    let c = ctx_ws(&ws);
    // Literal: defaults to the workspace root (no path arg).
    let out = run("fs.search", &[("query".into(), "TODO".into())], &c).unwrap();
    let s = out.as_str().unwrap();
    assert!(s.contains("src/a.rs:2") && s.contains("src/b.rs:1"), "literal path:line hits: {s}");
    // Regex (alternation).
    let out = run("fs.search", &[("query".into(), "one|two".into()), ("regex".into(), "true".into())], &c).unwrap();
    let s = out.as_str().unwrap();
    assert!(s.contains("src/a.rs:1") && s.contains("src/a.rs:3"), "regex hits: {s}");
    // Empty query errors; no path AND no workspace errors.
    assert!(run("fs.search", &[("query".into(), "".into())], &c).is_err());
    assert!(run("fs.search", &[("query".into(), "x".into())], &ctx()).is_err());
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn fs_list_tags_each_entry_with_a_category() {
    let ws = tmpdir("fscat");
    std::fs::create_dir_all(ws.join("sub")).unwrap();
    std::fs::write(ws.join("pic.PNG"), b"x").unwrap();
    std::fs::write(ws.join("song.mp3"), b"x").unwrap();
    std::fs::write(ws.join("clip.mov"), b"x").unwrap();
    std::fs::write(ws.join("notes.txt"), b"x").unwrap();
    let out = run("fs.list", &[("path".into(), ws.display().to_string())], &ctx()).unwrap();
    let entries = out.get("entries").and_then(|e| e.as_array()).unwrap();
    let cat = |name: &str| {
        entries
            .iter()
            .find(|e| e.get("name").and_then(|n| n.as_str()) == Some(name))
            .and_then(|e| e.get("category"))
            .and_then(|c| c.as_str())
            .unwrap()
            .to_string()
    };
    assert_eq!(cat("sub"), "dir");
    assert_eq!(cat("pic.PNG"), "image", "case-insensitive ext → image");
    assert_eq!(cat("song.mp3"), "audio");
    assert_eq!(cat("clip.mov"), "video");
    assert_eq!(cat("notes.txt"), "file");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn fs_list_defaults_missing_path_to_the_workspace() {
    // An agent that calls `fs.list` with no path (or `.`) lists the workspace root, instead of
    // erroring "missing path".
    let ws = tmpdir("fslistdefault");
    std::fs::write(ws.join("a.txt"), b"x").unwrap();
    std::fs::create_dir_all(ws.join("sub")).unwrap();
    let c = ctx_ws(&ws);
    let names = |out: corelib::wire::Json| -> Vec<String> {
        out.get("entries")
            .and_then(|e| e.as_array())
            .unwrap()
            .iter()
            .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect()
    };
    // No path at all → the workspace. (An agent's `.` is resolved to an absolute path by the
    // agent layer's `resolve_under` BEFORE it reaches the cap, so the cap only sees absolutes.)
    let got = names(run("fs.list", &[], &c).unwrap());
    assert!(got.iter().any(|n| n == "a.txt") && got.iter().any(|n| n == "sub"), "no-path lists the workspace: {got:?}");
    // With NO workspace it still resolves (home) rather than erroring.
    assert!(run("fs.list", &[], &ctx()).is_ok(), "no workspace → home, not an error");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn fs_list_returns_dotfile_names_when_hidden_is_on() {
    // Guards the "hidden files show their name" fix at the data layer: a dotfile is included
    // only with hidden=true, and its `name` (leading dot intact) is always present.
    let ws = tmpdir("fshidden");
    std::fs::write(ws.join(".secret_config"), b"x").unwrap();
    std::fs::write(ws.join("visible.txt"), b"x").unwrap();
    let names = |hidden: &str| -> Vec<String> {
        run("fs.list", &[("path".into(), ws.display().to_string()), ("hidden".into(), hidden.into())], &ctx())
            .unwrap()
            .get("entries")
            .and_then(|e| e.as_array())
            .unwrap()
            .iter()
            .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect()
    };
    assert!(!names("false").iter().any(|n| n == ".secret_config"), "dotfile hidden by default");
    let shown = names("true");
    assert!(shown.iter().any(|n| n == ".secret_config"), "dotfile name is present with hidden=true");
    assert!(shown.iter().any(|n| n == "visible.txt"));
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn fs_measure_sums_size_and_counts_recursively() {
    let ws = tmpdir("fsmeasure");
    std::fs::create_dir_all(ws.join("a/b")).unwrap();
    std::fs::write(ws.join("top.bin"), vec![0u8; 100]).unwrap();
    std::fs::write(ws.join("a/mid.bin"), vec![0u8; 20]).unwrap();
    std::fs::write(ws.join("a/b/deep.bin"), vec![0u8; 3]).unwrap();
    let out = run("fs.measure", &[("path".into(), ws.display().to_string())], &ctx()).unwrap();
    assert_eq!(out.get("bytes").and_then(|v| v.as_f64()), Some(123.0), "100 + 20 + 3 across the tree");
    assert_eq!(out.get("files").and_then(|v| v.as_f64()), Some(3.0));
    assert_eq!(out.get("dirs").and_then(|v| v.as_f64()), Some(2.0), "a + a/b");
    assert_eq!(out.get("partial").and_then(|v| v.as_bool()), Some(false));
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn fs_write_outside_or_unset_workspace_is_denied() {
    let ws = tmpdir("fsdeny");
    // no workspace → writes disabled
    assert!(run("fs.write", &[("path".into(), ws.join("a.txt").display().to_string()), ("content".into(), "x".into())], &ctx()).is_err());
    // outside the workspace root → denied
    let outside = std::env::temp_dir().join(format!("tt-caps-out-{}.txt", std::process::id()));
    assert!(run("fs.write", &[("path".into(), outside.display().to_string()), ("content".into(), "x".into())], &ctx_ws(&ws)).is_err());
    assert!(!outside.exists());
    // a `..` escape is rejected even though it would resolve inside
    assert!(run("fs.write", &[("path".into(), ws.join("..").join("escape.txt").display().to_string()), ("content".into(), "x".into())], &ctx_ws(&ws)).is_err());
    // reads are NEVER confined — fs.read of an outside file still works
    std::fs::write(&outside, "readable").unwrap();
    assert_eq!(run("fs.read", &[("path".into(), outside.display().to_string())], &ctx_ws(&ws)).unwrap().get("text").and_then(Json::as_str), Some("readable"));
    let _ = std::fs::remove_file(&outside);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn fs_read_blocks_secret_paths() {
    let dir = tmpdir("fssecret");
    // A `.pem` (secret-shaped) read is blocked even inside the workspace + as a free read.
    let pem = dir.join("server.pem");
    std::fs::write(&pem, "-----BEGIN PRIVATE KEY-----\nx\n-----END PRIVATE KEY-----").unwrap();
    assert!(run("fs.read", &[("path".into(), pem.display().to_string())], &ctx_ws(&dir)).is_err());
    assert!(run("fs.stat", &[("path".into(), pem.display().to_string())], &ctx()).is_err());
    // A normal file is still readable.
    let ok = dir.join("notes.txt");
    std::fs::write(&ok, "hello").unwrap();
    assert_eq!(run("fs.read", &[("path".into(), ok.display().to_string())], &ctx()).unwrap().get("text").and_then(Json::as_str), Some("hello"));
    // Secret directories are flagged regardless of extension.
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() {
        assert!(is_secret_path(std::path::Path::new(&format!("{home}/.ssh/id_rsa"))));
        assert!(is_secret_path(std::path::Path::new(&format!("{home}/.aws/credentials"))));
        assert!(is_secret_path(std::path::Path::new(&format!("{home}/.aiTerminal/config.toml"))));
        assert!(!is_secret_path(std::path::Path::new(&format!("{home}/Documents/notes.md"))));
    }
    let _ = std::fs::remove_dir_all(&dir);
}


#[test]
fn html_to_markdown_reduces_tags_and_drops_scripts() {
    let html = "<html><head><style>x{color:red}</style></head><body><h1>Title</h1><p>Hello <b>world</b> &amp; more</p><script>evil()</script></body></html>";
    let md = html_to_markdown(html);
    assert!(md.contains("# Title"), "got: {md}");
    assert!(md.contains("**world**"));
    assert!(md.contains("& more"));
    assert!(!md.contains("evil"));
    assert!(!md.to_lowercase().contains("<script"));
    // UTF-8 text survives intact
    assert!(html_to_markdown("<p>café — déjà</p>").contains("café — déjà"));
}

#[test]
fn web_read_md_fixture_and_remote_guards() {
    let f = std::env::temp_dir().join(format!("tt-caps-webread-{}.md", std::process::id()));
    std::fs::write(&f, "# Doc\n\nbody text").unwrap();
    let r = run("web.read", &[("url".into(), format!("md://{}", f.display()))], &ctx()).unwrap();
    assert!(r.get("markdown").and_then(Json::as_str).unwrap().contains("# Doc"));
    // https with remote disabled → error (no egress)
    let no_remote = CapCtx { policy: Arc::new(crate::security::Policy::new()), app_data: None, remote_enabled: false, origin: String::new(), sandbox: None };
    assert!(run("web.read", &[("url".into(), "https://example.com".into())], &no_remote).is_err());
    // an SSRF host is blocked even with remote enabled
    assert!(run("web.read", &[("url".into(), "https://localhost/x".into())], &ctx()).is_err());
    // non-http(s)/md scheme rejected
    assert!(run("web.read", &[("url".into(), "ftp://x".into())], &ctx()).is_err());
    let _ = std::fs::remove_file(&f);
}

#[test]
fn sys_run_guard_sees_canonical_argv_not_quoted_bypass() {
    let mut policy = crate::security::Policy::new();
    policy.add_deny("^rm\\b").unwrap(); // an anchored deny rule
    let ctx = CapCtx { policy: Arc::new(policy), app_data: None, remote_enabled: true, origin: String::new(), sandbox: None };
    // Quoting used to slip past `^rm` (raw string starts with `"`); now the guard sees
    // the canonical de-quoted command, so it is still blocked.
    assert!(run("sys.run", &[("cmd".into(), "\"rm\" -rf /tmp/whatever".into())], &ctx).unwrap_err().contains("blocked"));
    assert!(run("sys.run", &[("cmd".into(), "rm -rf /tmp/whatever".into())], &ctx).unwrap_err().contains("blocked"));
    // An unterminated quote is rejected (no mangled-token execution).
    assert!(run("sys.run", &[("cmd".into(), "echo \"oops".into())], &ctx).unwrap_err().contains("unterminated"));
    // A benign command still runs.
    assert_eq!(run("sys.run", &[("cmd".into(), "echo hi".into())], &ctx).unwrap().as_str(), Some("hi\n"));
}


#[test]
fn ssrf_blocks_private_and_encoded_hosts() {
    use std::net::IpAddr;
    // is_blocked_ip — the pure core (literal IPs, incl. IPv6 + mapped forms).
    for ip in ["127.0.0.1", "10.1.2.3", "192.168.0.1", "169.254.169.254", "172.16.5.5", "0.0.0.0", "100.64.1.1", "::1", "fc00::1", "fe80::1", "::ffff:127.0.0.1", "::ffff:169.254.169.254"] {
        assert!(is_blocked_ip(&ip.parse::<IpAddr>().unwrap()), "{ip} should be blocked");
    }
    for ip in ["93.184.216.34", "8.8.8.8", "2606:2800:220:1:248:1893:25c8:1946"] {
        assert!(!is_blocked_ip(&ip.parse::<IpAddr>().unwrap()), "{ip} should be allowed");
    }
    // ssrf_resolve — numeric encodings normalize the way getaddrinfo/curl do (offline:
    // these resolve locally, no DNS). localhost + literal private + decimal/IPv6 forms.
    assert!(ssrf_resolve("localhost", 443).is_err());
    assert!(ssrf_resolve("127.0.0.1", 443).is_err());
    assert!(ssrf_resolve("2130706433", 443).is_err(), "decimal 127.0.0.1 must be blocked");
    assert!(ssrf_resolve("[::1]", 443).is_err());
    assert!(ssrf_resolve("[::ffff:127.0.0.1]", 443).is_err());
    // url_host_port parsing (scheme default port, [IPv6]:port, userinfo, path suffix).
    assert_eq!(url_host_port("https://example.com/a/b?x"), Some(("example.com".into(), 443)));
    assert_eq!(url_host_port("https://[::1]:8443/x"), Some(("::1".into(), 8443)));
    assert_eq!(url_host_port("https://user@host:9000/"), Some(("host".into(), 9000)));
}

#[test]
fn sec_is_query_only() {
    let mut policy = crate::security::Policy::new();
    policy.add_deny("rm").unwrap();
    let ctx = CapCtx { policy: Arc::new(policy), app_data: None, remote_enabled: true, origin: String::new(), sandbox: None };
    let r = run("sec.check_command", &[("cmd".into(), "rm -rf x".into())], &ctx).unwrap();
    assert_eq!(r.get("verdict").and_then(Json::as_str), Some("deny"));
}

#[test]
fn sys_run_blocked_by_guard() {
    let mut policy = crate::security::Policy::new();
    policy.add_deny("^danger").unwrap();
    let ctx = CapCtx { policy: Arc::new(policy), app_data: None, remote_enabled: true, origin: String::new(), sandbox: None };
    // `danger --now` is an INERT literal — the guard rejects it, it never runs.
    assert!(run("sys.run", &[("cmd".into(), "danger --now".into())], &ctx).is_err());
}

#[test]
fn sys_run_confirm_is_blocked_in_noninteractive_path() {
    // A `Confirm` verdict has no UI to prompt here (background agent / app
    // capability path) → deny-wins, it must NOT execute. (`git status` is an
    // inert literal; the confirm rule rejects it before any exec.)
    let mut policy = crate::security::Policy::new();
    policy.add_confirm("^git ").unwrap();
    let ctx = CapCtx { policy: Arc::new(policy), app_data: None, remote_enabled: true, origin: String::new(), sandbox: None };
    let r = run("sys.run", &[("cmd".into(), "git status".into())], &ctx);
    assert!(r.is_err(), "Confirm-class command must not run unprompted");
    assert!(format!("{}", r.unwrap_err()).contains("confirmation"));
}

#[test]
fn store_requires_app_sandbox() {
    assert!(run("store.get", &[("key".into(), "x".into())], &ctx()).is_err());
    let dir = std::env::temp_dir().join(format!("tt-caps-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let c = CapCtx { policy: Arc::new(crate::security::Policy::new()), app_data: Some(dir.clone()), remote_enabled: true, origin: String::new(), sandbox: None };
    run("store.set", &[("key".into(), "pref".into()), ("value".into(), "{\"x\":1}".into())], &c).unwrap();
    let got = run("store.get", &[("key".into(), "pref".into())], &c).unwrap();
    assert_eq!(got.get("x").and_then(Json::as_f64), Some(1.0));
    // traversal rejected
    assert!(run("store.get", &[("key".into(), "../escape".into())], &c).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}


#[test]
fn fs_read_only_browsing() {
    let dir = std::env::temp_dir().join(format!("tt-fs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), "hello").unwrap();
    std::fs::write(dir.join(".hidden"), "x").unwrap();
    let dpath = dir.to_string_lossy().into_owned();

    // list: hidden filtered by default; dirs sort first.
    let r = run("fs.list", &[("path".into(), dpath.clone())], &ctx()).unwrap();
    let entries = r.get("entries").and_then(Json::as_array).unwrap();
    let names: Vec<&str> = entries.iter().filter_map(|e| e.get("name").and_then(Json::as_str)).collect();
    assert_eq!(names, vec!["sub", "a.txt"], "dirs first, hidden hidden");
    assert_eq!(entries[0].get("kind").and_then(Json::as_str), Some("dir"));
    // hidden=true reveals the dotfile.
    let r = run("fs.list", &[("path".into(), dpath.clone()), ("hidden".into(), "true".into())], &ctx()).unwrap();
    assert_eq!(r.get("entries").and_then(Json::as_array).unwrap().len(), 3);

    // stat + read.
    let fpath = dir.join("a.txt").to_string_lossy().into_owned();
    let s = run("fs.stat", &[("path".into(), fpath.clone())], &ctx()).unwrap();
    assert_eq!(s.get("kind").and_then(Json::as_str), Some("file"));
    assert_eq!(s.get("ext").and_then(Json::as_str), Some("txt"));
    let rd = run("fs.read", &[("path".into(), fpath.clone()), ("max".into(), "3".into())], &ctx()).unwrap();
    assert_eq!(rd.get("text").and_then(Json::as_str), Some("hel"));
    assert_eq!(rd.get("truncated").and_then(Json::as_bool), Some(true));

    // home + roots.
    assert!(run("fs.home", &[], &ctx()).unwrap().get("path").is_some());
    assert!(!run("fs.roots", &[], &ctx()).unwrap().as_array().unwrap().is_empty());

    // relative path rejected.
    assert!(run("fs.list", &[("path".into(), "relative/dir".into())], &ctx()).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}




// A custom object dispatches through the registry without touching any central
// match — the Open/Closed property the registry exists to provide.
struct Echo;
impl NativeObject for Echo {
    fn family(&self) -> &'static str { "echo" }
    fn methods(&self) -> &'static [MethodSpec] {
        &[MethodSpec { method: "echo.say", describe: "Echo a value" }]
    }
    fn invoke(&self, _m: &str, args: &[(String, String)], _c: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        Ok(Json::Str(arg(args, 0, "text").unwrap_or("").to_string()))
    }
}

#[test]
fn registry_is_open_for_extension() {
    // a third-party object dispatches with no edit to a central match
    let objs = ObjectRegistry::new(vec![Box::new(Echo)]);
    assert_eq!(objs.run("echo.say", &[("text".into(), "hi".into())], &ctx(), &mut NullHost).unwrap().as_str(), Some("hi"));
    assert_eq!(objs.describe("echo.say"), "Echo a value");
    assert!(objs.run("nope.x", &[], &ctx(), &mut NullHost).is_err(), "unknown family errors");
}

#[test]
fn pure_families_work_with_no_host() {
    // Every family is pure over CapCtx — the NullHost path is the only path.
    assert!(run("clock.now", &[], &ctx()).is_ok());
    assert!(run("time.now", &[], &ctx()).is_ok());
    // an unknown family errors
    assert!(run("nope.x", &[], &ctx()).is_err());
}

// ── network gating (no egress ever happens in these tests: every call fails
// BEFORE any socket — on the [ai] network switch or the https-only rule) ──────

fn ctx_offline() -> CapCtx {
    CapCtx { policy: Arc::new(crate::security::Policy::new()), app_data: None, remote_enabled: false, origin: String::new(), sandbox: None }
}

#[test]
fn network_families_respect_the_ai_network_switch() {
    // [ai] network = false → http/net/web egress families refuse with a clear error.
    for (m, args) in [
        ("http.get", vec![("url".to_string(), "https://example.com".to_string())]),
        ("http.post", vec![("url".to_string(), "https://example.com".to_string())]),
        ("net.get", vec![("url".to_string(), "https://example.com".to_string())]),
        ("web.read", vec![("url".to_string(), "https://example.com".to_string())]),
    ] {
        let err = run(m, &args, &ctx_offline()).unwrap_err();
        assert!(err.contains("network is disabled"), "{m}: {err}");
    }
}

#[test]
fn http_requires_https_and_never_touches_plain_http() {
    // Even with the network on, a non-https URL is rejected before any request.
    let err = run("http.get", &[("url".into(), "http://example.com".into())], &ctx()).unwrap_err();
    assert!(err.contains("only https"), "{err}");
    let err = run("http.request", &[("url".into(), "ftp://x".into()), ("method".into(), "GET".into())], &ctx()).unwrap_err();
    assert!(err.contains("only https"), "{err}");
}

// ── memory family through the pure caps path (hermetic: temp workspace + HOME) ─

#[test]
fn memory_family_add_search_forget_through_caps() {
    let (_h, _home) = crate::test_home::lock_home("caps-memory");
    let root = tmpdir("caps-memory-ws");
    let ctx = ctx_ws(&root);
    // add → returns the stored id
    let added = run("memory.add", &[("text".into(), "the deploy script lives in ops/deploy.sh".into())], &ctx).unwrap();
    let id = added.get("id").and_then(Json::as_str).unwrap_or_default().to_string();
    assert!(!id.is_empty(), "add returns an id");
    // search finds it by content
    let hits = run("memory.search", &[("query".into(), "deploy script".into())], &ctx).unwrap();
    let arr = hits.as_array().expect("search returns an array");
    assert!(!arr.is_empty(), "the stored memory is retrievable");
    // get reads it back
    let got = run("memory.get", &[("id".into(), id.clone())], &ctx).unwrap();
    assert!(got.to_string().contains("deploy"));
    // forget removes it
    run("memory.forget", &[("id".into(), id.clone())], &ctx).unwrap();
    let after = run("memory.search", &[("query".into(), "deploy script".into())], &ctx).unwrap();
    assert!(after.as_array().map(|a| a.iter().all(|h| h.get("id").and_then(Json::as_str) != Some(id.as_str()))).unwrap_or(true));
    let _ = std::fs::remove_dir_all(&root);
}

// ── families with side effects outside the sandbox are error-checked only ─────

#[test]
fn task_run_errors_outside_an_agent_loop() {
    // task.run is registered (for the tool catalog) but only the agent runner may
    // execute it — the plain capability path must refuse, not spawn agents.
    let err = run("task.run", &[("agent".into(), "explorer".into()), ("prompt".into(), "x".into())], &ctx()).unwrap_err();
    assert!(err.contains("agent loop"), "{err}");
}

#[test]
fn clip_rejects_unknown_methods() {
    // Only the error path — a real clip.read/write would touch the USER's clipboard.
    assert!(run("clip.nope", &[], &ctx()).is_err());
}

#[test]
fn fs_read_clamps_the_model_supplied_max() {
    // `max: 999999999` must NOT defeat the read cap — at most FS_READ_MAX (1 MiB)
    // ever comes back (or into memory), with the truncation flagged.
    let dir = std::env::temp_dir().join(format!("tt-fsread-clamp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let big = dir.join("big.txt");
    std::fs::write(&big, "z".repeat(2 * 1024 * 1024)).unwrap();
    let out = run(
        "fs.read",
        &[("path".into(), big.display().to_string()), ("max".into(), "999999999".into())],
        &ctx(),
    )
    .unwrap();
    let text = out.get("text").and_then(Json::as_str).unwrap();
    assert_eq!(text.len(), 1024 * 1024, "clamped to FS_READ_MAX");
    assert_eq!(out.get("truncated").and_then(Json::as_bool), Some(true));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sys_run_output_is_capped_not_buffered_whole() {
    // A command printing ~10 MB: the result carries at most the cap + marker —
    // the transcript (and memory) never sees the full stream.
    let out = run("sys.run", &[("cmd".into(), "head -c 10000000 /dev/zero".into())], &ctx()).unwrap();
    let s = out.as_str().unwrap();
    assert!(s.len() <= 256 * 1024 + 64, "capped: {}", s.len());
    assert!(s.contains("[output truncated"), "the truncation is visible to the model");
}
