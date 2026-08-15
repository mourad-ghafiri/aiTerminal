use super::*;

fn status() -> Status {
    Status {
        root: "project".into(),
        mode: Mode::Build,
        persona: None,
        model: "claude-opus-4-8".into(),
        tokens: (12_345, 950),
        cost: 0.0123,
        overlay_on: true,
        tasks: Some((3, 9)),
    }
}

#[test]
fn the_clusters_carry_identity_left_and_facts_right() {
    let (left, right) = segments(&status(), Some(std::time::Duration::from_secs(42)));
    assert_eq!(left[0].text, "\u{2726} project");
    assert_eq!(left[0].kind, Kind::Brand);
    assert_eq!(left[1].text, "build");
    assert_eq!(left[1].kind, Kind::Pill);
    let texts: Vec<&str> = right.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["\u{25b0}\u{25b0}\u{25b1}\u{25b1}\u{25b1} 3/9", "claude-opus-4-8", "12.3k in / 950 out \u{b7} $0.012", "\u{25cf} project", "0:42"]);
}

#[test]
fn quiet_facts_stay_out_of_the_header() {
    let s = Status { tokens: (0, 0), tasks: None, persona: None, overlay_on: false, ..status() };
    let (left, right) = segments(&s, None);
    assert_eq!(left.len(), 2, "no persona pinned, none shown");
    let texts: Vec<&str> = right.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["claude-opus-4-8", "\u{25cb} global"], "no spend, no plan, no clock");
}

#[test]
fn when_space_runs_out_the_spend_goes_before_the_clock() {
    // The keep weights ARE the design: tokens < model < overlay < progress < clock.
    let (_, right) = segments(&status(), Some(std::time::Duration::from_secs(5)));
    let mut keeps: Vec<(u8, &str)> = right.iter().map(|s| (s.keep, s.text.as_str())).collect();
    keeps.sort();
    assert!(keeps[0].1.contains("in /"), "the spend sheds first: {keeps:?}");
    assert_eq!(keeps.last().unwrap().1, "0:05", "the running clock sheds last");
}

#[test]
fn the_small_vocabularies_read_right() {
    assert_eq!(kfmt(950), "950");
    assert_eq!(kfmt(12_345), "12.3k");
    assert_eq!(kfmt(4_100_000), "4.1m");
    assert_eq!(progress(0, 9), "\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1} 0/9");
    assert_eq!(progress(9, 9), "\u{25b0}\u{25b0}\u{25b0}\u{25b0}\u{25b0} 9/9");
    assert_eq!(progress(1, 2), "\u{25b0}\u{25b0}\u{25b0}\u{25b1}\u{25b1} 1/2", "half rounds up to the nearer cell");
    assert_eq!(progress(0, 0), "\u{25b1}\u{25b1}\u{25b1}\u{25b1}\u{25b1} 0/0", "an empty plan cannot divide by zero");
    assert_eq!(clock(std::time::Duration::from_secs(725)), "12:05");
}
