use super::*;

fn pool(n: usize) -> Pool {
    Pool {
        lines: (0..n).filter_map(|i| Line::new(Kind::Fact, &format!("line {i}"))).collect(),
        written: 1_700_000_000,
    }
}

fn settings() -> Settings {
    Settings {
        enabled: true,
        kinds: Kind::all(),
        after: Duration::from_secs(6),
        every: Duration::from_secs(15),
    }
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn a_short_wait_is_never_interrupted() {
    // The rule that decides whether this feature is pleasant or maddening. A run that
    // answers quickly must show nothing at all — a line that appears for half a second
    // and vanishes is worse than the plain spinner it replaced.
    let mut muse = Muse::new(&pool(4), &settings(), 0);
    for at in [0, 1, 3, 5] {
        assert_eq!(muse.line(secs(at)), None, "{at}s in, nothing has been waited for yet");
    }
    assert!(muse.line(secs(6)).is_some(), "and at `after` it says something");
}

#[test]
fn a_line_stays_for_its_turn_and_then_gives_way() {
    let mut muse = Muse::new(&pool(4), &settings(), 0);
    let first = muse.line(secs(6)).expect("a line").to_string();
    // It does not flicker: every frame in between is the SAME line.
    for at in [7, 10, 14, 20] {
        assert_eq!(muse.line(secs(at)), Some(first.as_str()), "still {at}s in");
    }
    // `every` after it went up, the next one takes over.
    let second = muse.line(secs(21)).expect("a line").to_string();
    assert_ne!(second, first, "and it is a different one");
}

#[test]
fn a_new_wait_starts_over_rather_than_resuming() {
    // Each turn of a run is its own wait, and the spinner's clock restarts with it. A
    // line whose moment passed while the model was answering must not reappear mid-turn:
    // the next wait earns its own silence first.
    let mut muse = Muse::new(&pool(4), &settings(), 0);
    assert!(muse.line(secs(9)).is_some());
    assert_eq!(muse.line(secs(0)), None, "a fresh wait is silent again");
    assert_eq!(muse.line(secs(5)), None);
    assert!(muse.line(secs(6)).is_some(), "and then speaks on its own schedule");
}

#[test]
fn the_rotation_comes_round_without_repeating_itself() {
    let mut muse = Muse::new(&pool(3), &settings(), 0);
    let mut seen = Vec::new();
    for turn in 0..3 {
        let at = 6 + turn * 15;
        seen.push(muse.line(secs(at)).expect("a line").to_string());
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 3, "three turns, three different lines");
}

#[test]
fn nothing_to_say_is_a_muse_that_says_nothing() {
    // Four ways this ends up mute, and all of them have to behave identically — no
    // caller should have to carry an `Option` for a decoration.
    let off = Settings { enabled: false, ..settings() };
    let none = Settings { kinds: Vec::new(), ..settings() };
    for (why, muse) in [
        ("switched off", Muse::new(&pool(4), &off, 0)),
        ("no kinds wanted", Muse::new(&pool(4), &none, 0)),
        ("an empty pool", Muse::new(&Pool::default(), &settings(), 0)),
        ("silent by construction", Muse::silent()),
    ] {
        let mut muse = muse;
        assert!(muse.mute(), "{why}");
        for at in [0, 6, 60, 6000] {
            assert_eq!(muse.line(secs(at)), None, "{why}, {at}s in");
        }
    }
}

#[test]
fn only_the_kinds_asked_for_are_ever_shown() {
    let mixed = Pool {
        lines: vec![
            Line::new(Kind::Tip, "a tip").unwrap(),
            Line::new(Kind::Quote, "a quote").unwrap(),
            Line::new(Kind::Cheer, "a cheer").unwrap(),
        ],
        written: 1,
    };
    let only_tips = Settings { kinds: vec![Kind::Tip], ..settings() };
    let mut muse = Muse::new(&mixed, &only_tips, 0);
    for turn in 0..5 {
        assert_eq!(muse.line(secs(6 + turn * 15)), Some("a tip"), "nothing else was asked for");
    }
}

#[test]
fn a_line_that_would_wrap_the_terminal_is_dropped_not_truncated() {
    // The whole line is erased with a bare `\r`, so one that wraps becomes two rows and
    // only the second is ever cleared — a trail of half-lines down the terminal. Half a
    // fact is not a fact either, so it goes rather than being cut.
    assert!(Line::new(Kind::Fact, &"x".repeat(MAX_LEN)).is_some());
    assert!(Line::new(Kind::Fact, &"x".repeat(MAX_LEN + 1)).is_none());
    assert!(Line::new(Kind::Fact, "two\nlines").is_none());
    assert!(Line::new(Kind::Fact, "   ").is_none());
    // Trimmed, so a model's stray whitespace does not spend the budget.
    assert_eq!(Line::new(Kind::Fact, "  spaced  ").unwrap().text, "spaced");
}

#[test]
fn a_pool_survives_a_round_trip_and_a_broken_file_is_simply_no_pool() {
    let written = Pool {
        lines: vec![
            Line::new(Kind::Tip, "@flow retry <node> re-runs it and what needed it").unwrap(),
            Line::new(Kind::Quote, "Simplicity is prerequisite for reliability \u{2014} Dijkstra").unwrap(),
        ],
        written: 1_700_000_000,
    };
    let back = Pool::parse(&written.to_toml());
    assert_eq!(back.lines, written.lines);
    assert_eq!(back.written, written.written);
    // Anything unreadable is an empty pool, which is the same thing as not having one.
    for junk in ["", "[[[[", "written = \"soon\"", "[[line]]\nkind = \"nonsense\"\ntext = \"x\""] {
        assert!(Pool::parse(junk).lines.is_empty(), "{junk:?}");
    }
}

#[test]
fn a_thin_or_stale_pool_asks_to_be_written_again() {
    let now = 1_800_000_000u64;
    let fresh = Pool { lines: pool(THIN).lines, written: now };
    assert!(!fresh.needs_refill(now), "enough lines, just written");
    assert!(Pool { lines: pool(THIN - 1).lines, written: now }.needs_refill(now), "too few");
    assert!(fresh.needs_refill(now + STALE.as_secs() + 1), "old enough to be worth writing again");
    assert!(Pool::default().needs_refill(now), "and no pool at all certainly does");
}

#[test]
fn the_kinds_a_person_writes_in_config_are_the_kinds_the_code_means() {
    // The words in `[motivation] kinds` are the vocabulary of the setting, so both
    // spellings of each are accepted and anything else is dropped rather than guessed at.
    for (word, kind) in [
        ("tips", Kind::Tip),
        ("tip", Kind::Tip),
        ("facts", Kind::Fact),
        ("quotes", Kind::Quote),
        ("encouragement", Kind::Cheer),
        ("cheer", Kind::Cheer),
        ("  QUOTES  ", Kind::Quote),
    ] {
        assert_eq!(Kind::read(word), Some(kind), "{word:?}");
    }
    assert_eq!(Kind::read("jokes"), None);
    // And every kind round-trips through the word it is written as.
    for k in Kind::all() {
        assert_eq!(Kind::read(k.word()), Some(k));
    }
}

#[test]
fn what_the_model_writes_is_read_strictly() {
    let reply = r#"Here you go:
[
  {"kind":"tips","text":"@flow retry <node> re-runs it and what needed it"},
  {"kind":"facts","text":"A cached prompt prefix costs about a tenth of a fresh one"},
  {"kind":"jokes","text":"a kind nobody asked for"},
  {"kind":"tips","text":"@FLOW RETRY <NODE> RE-RUNS IT AND WHAT NEEDED IT"},
  {"kind":"quotes","text":"x"},
  {"text":"no kind at all"},
  {"kind":"facts"}
]"#;
    let lines = refill::decode(reply, &[Kind::Tip, Kind::Fact]);
    // The two good ones. A kind nobody asked for goes, a duplicate goes however it is
    // cased, and anything missing a half goes.
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().all(|l| matches!(l.kind, Kind::Tip | Kind::Fact)));
    assert!(lines.iter().any(|l| l.text.starts_with("@flow retry")));
    // Nothing usable at all is no lines, never a half-written pool.
    for junk in ["", "sorry, I can't", "{}", "[]"] {
        assert!(refill::decode(junk, &Kind::all()).is_empty(), "{junk:?}");
    }
}
