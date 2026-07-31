use super::*;

fn m(p: &str, t: &str) -> bool {
    Regex::new(p).unwrap().is_match(t)
}

#[test]
fn literals_and_dot() {
    assert!(m("abc", "xx abc yy"));
    assert!(!m("abc", "ab c"));
    assert!(m("a.c", "axc"));
    assert!(!m("a.c", "a\nc"));
}

#[test]
fn quantifiers() {
    assert!(m("ab*c", "ac"));
    assert!(m("ab*c", "abbbc"));
    assert!(m("ab+c", "abc"));
    assert!(!m("ab+c", "ac"));
    assert!(m("colou?r", "color"));
    assert!(m("colou?r", "colour"));
}

#[test]
fn classes_ranges_negation_escapes() {
    assert!(m("[0-9]+", "abc123"));
    assert!(m("[a-fA-F]", "D"));
    assert!(m("[^0-9]", "x"));
    assert!(!m("^[^0-9]+$", "12 3"));
    assert!(m(r"\d{0}\w+", "hello_1"));
    assert!(m(r"\s", "a b"));
    assert!(!m(r"^\S+$", "a b"));
}

#[test]
fn anchors_alternation_groups() {
    assert!(m("^foo$", "foo"));
    assert!(!m("^foo$", "foobar"));
    assert!(m("cat|dog", "a dog"));
    assert!(m("(ab)+", "abab"));
    assert!(m("gr(a|e)y", "grey"));
    assert!(!m("^(ab)+$", "aba"));
}

#[test]
fn word_boundary_and_brace_quantifiers() {
    // The command-guard motivator: ^rm\b matches the command rm but not rmdir.
    assert!(m(r"^rm\b", "rm -rf /"));
    assert!(m(r"^rm\b", "rm"));
    assert!(!m(r"^rm\b", "rmdir tmp"));
    assert!(m(r"\bcat\b", "the cat sat"));
    assert!(!m(r"\bcat\b", "category"));
    // bounded {m,n}
    assert!(m(r"^\d{3}$", "123"));
    assert!(!m(r"^\d{3}$", "12"));
    assert!(m(r"a{2,4}", "aaaa"));
    assert!(!m(r"^a{2,4}$", "aaaaa"));
    assert!(m(r"x{2,}", "xxxxx")); // unbounded {m,}
}

#[test]
fn case_insensitive_flag() {
    assert!(m("(?i)rm -rf", "RM -RF /"));
    assert!(!m("rm -rf", "RM -RF /"));
    assert!(m("(?i)[a-z]+", "ABC"));
}

#[test]
fn find_and_replace_all() {
    let re = Regex::new(r"\d+").unwrap();
    assert_eq!(re.find("ab12cd34"), Some((2, 4)));
    assert_eq!(re.replace_all("ab12cd34", "#"), "ab#cd#");
    let re2 = Regex::new("a").unwrap();
    assert_eq!(re2.replace_all("banana", "X"), "bXnXnX");
}

#[test]
fn class_negated_shorthands_in_class() {
    // `[\s\S]` is the standard "any char incl. newline" idiom — needs \S inside a class.
    let any = Regex::new(r"a[\s\S]*?b").unwrap();
    assert!(any.is_match("a\nx\ny\nb"), "[\\s\\S] must match across newlines");
    // \D / \W inside a class also parse as negated shorthands.
    assert!(Regex::new(r"[\D]").unwrap().is_match("z"));
    assert!(!Regex::new(r"^[\D]$").unwrap().is_match("5"));
    assert!(Regex::new(r"[\W]").unwrap().is_match("-"));
}

#[test]
fn redos_pattern_fails_fast_not_hang() {
    // The classic catastrophic-backtracking pattern: bounded by STEP_CAP.
    let re = Regex::new("(a+)+$").unwrap();
    let evil = "a".repeat(40) + "!";
    // Should return (no match) quickly rather than hang.
    assert!(!re.is_match(&evil));
}

#[test]
fn redos_budget_is_shared_across_start_positions() {
    // The regression this pins: the step cap used to reset PER START POSITION,
    // so a long input multiplied it by its length (n × 1e6 steps — minutes).
    // One shared budget means even a 10 KB pathological input returns fast.
    let re = Regex::new("(a+)+$").unwrap();
    let evil = "a".repeat(10_000) + "b";
    let t = std::time::Instant::now();
    assert!(!re.is_match(&evil));
    assert!(t.elapsed() < std::time::Duration::from_millis(100), "took {:?}", t.elapsed());
    // replace_all with the same pathological pattern: input comes back
    // UNCHANGED (never a silent half-redaction), also fast.
    let t = std::time::Instant::now();
    assert_eq!(re.replace_all(&evil, "#"), evil);
    assert!(t.elapsed() < std::time::Duration::from_millis(100), "took {:?}", t.elapsed());
}

#[test]
fn literal_prefix_prefilter_skips_clean_text_fast() {
    // Secret patterns are literal-headed (`sk-`, `AKIA…`): on text without the
    // head, matching must cost a substring scan, not a per-char backtrack.
    let re = Regex::new("sk-[a-z0-9]{8}").unwrap();
    let clean = "x".repeat(1_000_000);
    let t = std::time::Instant::now();
    assert!(!re.is_match(&clean));
    assert_eq!(re.replace_all_opt(&clean, "«key»"), None, "untouched → no allocation");
    assert!(t.elapsed() < std::time::Duration::from_millis(50), "took {:?}", t.elapsed());
    // Correctness is unchanged when the head IS present.
    assert!(re.is_match("token sk-abcd1234 end"));
    assert_eq!(re.replace_all("token sk-abcd1234 end", "«key»"), "token «key» end");
    // A `^`-anchored literal head still prefilters.
    let re = Regex::new("^AKIA[0-9A-Z]+").unwrap();
    assert!(!re.is_match(&clean));
    assert!(re.is_match("AKIA123XYZ"));
    // Case-insensitive patterns skip the prefilter but stay correct.
    let re = Regex::new("(?i)bearer [a-z]+").unwrap();
    assert!(re.is_match("Authorization: BEARER abc"));
}

#[test]
fn invalid_patterns_error() {
    assert!(Regex::new("[abc").is_err());
    assert!(Regex::new("(ab").is_err());
    assert!(Regex::new(r"\").is_err());
}

#[test]
fn pathological_patterns_are_rejected_at_compile() {
    // Unbounded `{n,m}` desugaring would clone billions of nodes → OOM. Rejected.
    assert!(Regex::new("a{2000000000}").is_err());
    assert!(Regex::new("a{0,999999999}").is_err());
    // Deep group nesting would overflow the parser stack. Rejected.
    let deep = format!("{}a{}", "(".repeat(1000), ")".repeat(1000));
    assert!(Regex::new(&deep).is_err());
    // Reasonable bounded repetition still compiles and matches.
    let r = Regex::new("a{2,4}").unwrap();
    assert!(r.is_match("aaa"));
    assert!(!r.is_match("a"));
}
