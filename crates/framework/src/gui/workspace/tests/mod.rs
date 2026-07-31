use super::*;

#[test]
fn terminal_pane_snapshot_round_trips_through_toml() {
    // A terminal-pane snapshot table round-trips kind/zoom/cwd/dims through TOML text —
    // the exact path workspace.toml takes (the full Pane is rebuilt by the live factory).
    let t = Toml::Table(vec![
        ("kind".into(), Toml::Str("terminal".into())),
        ("zoom".into(), Toml::Float(1.25)),
        ("cols".into(), Toml::Int(132)),
        ("rows".into(), Toml::Int(43)),
        ("cwd".into(), Toml::Str("/work/My Project".into())),
    ]);
    let back = Toml::parse(&t.to_string()).unwrap();
    assert_eq!(back.get("kind").and_then(|v| v.as_str()), Some("terminal"));
    assert_eq!(back.get("zoom").and_then(|v| v.as_num()), Some(1.25));
    assert_eq!(back.get("cols").and_then(|v| v.as_num()), Some(132.0));
    assert_eq!(back.get("rows").and_then(|v| v.as_num()), Some(43.0));
    assert_eq!(back.get("cwd").and_then(|v| v.as_str()), Some("/work/My Project"));
}

#[test]
fn restore_reads_saved_grid_dims_with_a_default_fallback() {
    // The dims parser mirrors restore_pane: both present and ≥1 → Some; missing or
    // garbage → None, so the pane falls back to the classic 80×24.
    let dims = |t: &Toml| {
        t.get("cols")
            .and_then(|v| v.as_num())
            .zip(t.get("rows").and_then(|v| v.as_num()))
            .filter(|(c, r)| *c >= 1.0 && *r >= 1.0)
            .map(|(c, r)| (c as u16, r as u16))
    };
    let full = Toml::Table(vec![("cols".into(), Toml::Int(120)), ("rows".into(), Toml::Int(30))]);
    assert_eq!(dims(&full), Some((120, 30)));
    assert_eq!(dims(&Toml::Table(vec![("cols".into(), Toml::Int(120))])), None, "rows missing → fallback");
    assert_eq!(dims(&Toml::Table(vec![])), None, "both missing → fallback");
    let zero = Toml::Table(vec![("cols".into(), Toml::Int(0)), ("rows".into(), Toml::Int(30))]);
    assert_eq!(dims(&zero), None, "a zero dimension is rejected");
}

#[test]
fn multi_tab_split_layout_round_trips() {
    // Two tabs — one a single pane, one a split of two terminals in different folders —
    // keep their layout, cwds, and the active-tab/focus through the TOML text form.
    let mk = |cwd: &str| {
        Toml::Table(vec![
            ("kind".into(), Toml::Str("terminal".into())),
            ("cwd".into(), Toml::Str(cwd.into())),
        ])
    };
    let doc = Toml::Table(vec![
        ("active".into(), Toml::Int(1)),
        ("tab".into(), Toml::Array(vec![
            Toml::Table(vec![("focus".into(), Toml::Int(0)), ("root".into(), Toml::Table(vec![("leaf".into(), mk("/home"))]))]),
            Toml::Table(vec![
                ("focus".into(), Toml::Int(1)),
                ("root".into(), Toml::Table(vec![("split".into(), Toml::Table(vec![
                    ("dir".into(), Toml::Str("row".into())),
                    ("kids".into(), Toml::Array(vec![
                        Toml::Table(vec![("leaf".into(), mk("/tmp"))]),
                        Toml::Table(vec![("leaf".into(), mk("/work/a,b"))]),
                    ])),
                ]))])),
            ]),
        ])),
    ]);
    let back = Toml::parse(&doc.to_string()).unwrap();
    assert_eq!(back.get("active").and_then(|v| v.as_num()), Some(1.0));
    let tabs = back.get("tab").and_then(|v| if let Toml::Array(a) = v { Some(a) } else { None }).unwrap();
    assert_eq!(tabs.len(), 2, "both tabs survive");
    let split_kids = tabs[1].get("root").and_then(|r| r.get("split")).and_then(|s| s.get("kids"))
        .and_then(|v| if let Toml::Array(a) = v { Some(a) } else { None }).unwrap();
    let cwd_of = |t: &Toml| t.get("leaf").and_then(|l| l.get("cwd")).and_then(|a| a.as_str()).map(str::to_string);
    assert_eq!(cwd_of(&split_kids[0]).as_deref(), Some("/tmp"));
    assert_eq!(cwd_of(&split_kids[1]).as_deref(), Some("/work/a,b"));
}

#[test]
fn window_and_tab_bar_round_trip_through_workspace_toml() {
    // The exact-state promise: the saved doc carries the window's logical size
    // and the tab-bar orientation, and the readers resolve them back.
    let (_h, _home) = crate::test_home::lock_home("ws-window");
    crate::profile::ensure_default();
    let path = crate::profile::workspace_path("default").unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "active = 0\ntab_bar = \"left\"\n[window]\nw = 1280.0\nh = 800.0\n[[tab]]\nfocus = 0\n[tab.root.leaf]\nkind = \"terminal\"\n",
    )
    .unwrap();
    assert_eq!(saved_window("default"), Some((1280.0, 800.0)));
    assert_eq!(saved_tab_bar("default").as_deref(), Some("left"));
    // Garbage sizes are rejected (never a 1×1 window).
    std::fs::write(&path, "[window]\nw = 10.0\nh = 5.0\n").unwrap();
    assert_eq!(saved_window("default"), None);
    // No file → no overrides.
    std::fs::remove_file(&path).unwrap();
    assert_eq!(saved_window("default"), None);
    assert_eq!(saved_tab_bar("default"), None);
}

#[test]
fn pane_content_round_trips_through_workspace_toml() {
    // The restore promise: a pane's STYLED buffer (ANSI escapes, multi-line,
    // quotes, non-ASCII) survives the TOML text form byte-for-byte.
    let content = "\u{276F} \x1b[32mcargo test\x1b[0m\nrunning \x1b[1m5\x1b[0m tests\ntest a::b {ok} \"quoted\" … مرحبا\n\x1b[38;5;42mdone\x1b[0m";
    let t = Toml::Table(vec![
        ("kind".into(), Toml::Str("terminal".into())),
        ("cwd".into(), Toml::Str("/w".into())),
        ("content".into(), Toml::Str(content.into())),
    ]);
    let back = Toml::parse(&t.to_string()).unwrap();
    assert_eq!(back.get("content").and_then(|v| v.as_str()), Some(content), "content is lossless through workspace.toml");
}

#[test]
fn expand_tilde_resolves_home() {
    let home = platform::os::home_dir().unwrap();
    assert_eq!(expand_tilde("~/proj"), home.join("proj").to_string_lossy());
    assert_eq!(expand_tilde("/abs/path"), "/abs/path");
}
