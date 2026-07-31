use super::*;

#[test]
fn cargo_short_output_parses_file_line_col_severity_message() {
    let out = "\
src/main.rs:4:9: error[E0433]: failed to resolve: use of undeclared crate `foo`
src/lib.rs:3:1: warning: unused import: `std::io`
error: could not compile `x` due to 1 previous error";
    let d = parse_cargo(out);
    assert_eq!(d.len(), 2, "the summary line is skipped");
    assert_eq!(d[0].file, "src/main.rs");
    assert_eq!((d[0].line, d[0].col), (4, 9));
    assert_eq!(d[0].severity, Severity::Error);
    assert_eq!(d[0].message, "failed to resolve: use of undeclared crate `foo`");
    assert_eq!(d[1].severity, Severity::Warning);
    assert_eq!(d[1].message, "unused import: `std::io`");
}

#[test]
fn tsc_output_parses_paren_location() {
    let out = "src/index.ts(10,5): error TS2304: Cannot find name 'foo'.\nFound 1 error.";
    let d = parse_tsc(out);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].file, "src/index.ts");
    assert_eq!((d[0].line, d[0].col), (10, 5));
    assert_eq!(d[0].severity, Severity::Error);
    assert_eq!(d[0].message, "Cannot find name 'foo'.");
}

#[test]
fn ruff_concise_output_is_warnings() {
    let out = "app.py:10:5: F821 Undefined name `foo`\nFound 1 error.";
    let d = parse_ruff(out);
    assert_eq!(d.len(), 1);
    assert_eq!((d[0].line, d[0].col), (10, 5));
    assert_eq!(d[0].severity, Severity::Warning);
    assert_eq!(d[0].message, "F821 Undefined name `foo`");
}

#[test]
fn govet_skips_package_headers() {
    let out = "# example.com/m\n./main.go:6:2: undefined: foo";
    let d = parse_govet(out);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].file, "./main.go");
    assert_eq!(d[0].severity, Severity::Error);
    assert_eq!(d[0].message, "undefined: foo");
}

#[test]
fn eslint_unix_output_reads_bracket_severity() {
    let out = "/w/a.js:2:7: 'x' is assigned a value but never used [Warning/no-unused-vars]\n\n1 problem";
    let d = parse_eslint(out);
    assert_eq!(d.len(), 1);
    assert_eq!((d[0].line, d[0].col), (2, 7));
    assert_eq!(d[0].severity, Severity::Warning);
    assert_eq!(d[0].message, "'x' is assigned a value but never used");
}

#[test]
fn detect_picks_toolchain_by_marker_file() {
    let dir = std::env::temp_dir().join(format!("diagdetect-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    // No marker → None.
    assert!(detect(&dir).is_none());
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname='x'").unwrap();
    assert_eq!(detect(&dir).unwrap().tool, "cargo");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_diagnostic_lines_are_ignored() {
    assert!(parse_cargo("   Compiling foo v0.1.0\n    Finished dev").is_empty());
    assert!(split_colon("just some prose").is_none());
    assert!(split_paren("no parens here").is_none());
}
