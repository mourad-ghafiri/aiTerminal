use super::*;

#[test]
fn identical_is_empty() {
    assert_eq!(unified_diff("a\nb\n", "a\nb\n", "f"), "");
}

#[test]
fn one_line_change_shows_plus_minus_with_context() {
    let old = "fn main() {\n    let x = compute();\n    print(x);\n}\n";
    let new = "fn main() {\n    let x = compute().await?;\n    print(x);\n}\n";
    let d = unified_diff(old, new, "src/main.rs");
    assert!(d.starts_with("```diff"));
    assert!(d.contains("- "), "shows the removed line");
    assert!(d.contains("+ "), "shows the added line");
    assert!(d.contains("let x = compute();"));
    assert!(d.contains("let x = compute().await?;"));
    assert!(d.contains(" fn main() {"), "keeps a line of context");
}

#[test]
fn distant_unchanged_lines_collapse() {
    let old = (0..30).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    let mut newv: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
    newv[15] = "line 15 CHANGED".into();
    let d = unified_diff(&old, &newv.join("\n"), "f");
    assert!(d.contains("…"), "far-from-change lines collapse to an ellipsis");
    assert!(d.contains("+line 15 CHANGED"));
    assert!(!d.contains("line 2\n"), "lines far from the change are not shown");
}
