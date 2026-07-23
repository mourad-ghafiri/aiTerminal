//! The `diag.*` native family — **project diagnostics**: run the workspace's own
//! checker (compiler / linter) and return structured `[{file,line,col,severity,message}]`
//! so an agent can SELF-VERIFY after editing and fix its own errors before finishing.
//!
//! This is the decisive coding lever OpenCode lacks (it ships only an *experimental* LSP
//! tool, no automatic diagnostics): a single cheap tool that turns real compiler output
//! into a problem list. The toolchain is auto-detected from a marker file at the workspace
//! root (`Cargo.toml` → `cargo check`, `tsconfig.json`/`package.json` → `tsc`/`eslint`,
//! `pyproject.toml`/`ruff.toml` → `ruff`, `go.mod` → `go vet`).
//!
//! Safety: `diag.check` is read-only inspection (the same class as `cargo check`, which is
//! already on the auto-pilot safe-list), so it runs without a prompt — but every checker
//! command is STILL re-checked through the command guard before it spawns, so a user's
//! `deny`/`confirm` rule blocks it exactly like `sys.run` (deny-wins). Bounded: at most
//! [`MAX_DIAGNOSTICS`] rows, and the checker runs with its cwd pinned to the workspace root.

use std::path::Path;
use std::process::Command;

use corelib::wire::Json;

use super::object::{MethodSpec, NativeObject};
use super::{arg, obj, CapCtx};

pub struct DiagObj;

const SPECS: &[MethodSpec] = &[MethodSpec {
    method: "diag.check",
    describe: "Run the project's compiler/linter and return structured errors (self-verify after edits)",
}];

/// Cap the number of parsed diagnostics so a pathological build (thousands of errors) can't
/// balloon the tool result the model has to read.
const MAX_DIAGNOSTICS: usize = 200;

impl NativeObject for DiagObj {
    fn family(&self) -> &'static str {
        "diag"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, _host: &mut dyn super::Host) -> Result<Json, String> {
        match method {
            "diag.check" => check(args, ctx),
            _ => Err(format!("unknown diag method '{method}'")),
        }
    }
}

/// One structured diagnostic.
struct Diagnostic {
    file: String,
    line: u32,
    col: u32,
    severity: Severity,
    message: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Severity {
    Error,
    Warning,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// The checker for a detected toolchain: the argv to run and the parser for its output.
struct Checker {
    tool: &'static str,
    /// The command as `[program, arg, …]`.
    argv: Vec<&'static str>,
    /// Parse the combined stdout+stderr into diagnostics.
    parse: fn(&str) -> Vec<Diagnostic>,
}

/// Auto-detect the toolchain from a marker file at `root`, preferring the type checker.
fn detect(root: &Path) -> Option<Checker> {
    let has = |name: &str| root.join(name).exists();
    if has("Cargo.toml") {
        return Some(Checker { tool: "cargo", argv: vec!["cargo", "check", "--message-format=short", "--quiet"], parse: parse_cargo });
    }
    if has("tsconfig.json") {
        return Some(Checker { tool: "tsc", argv: vec!["tsc", "--noEmit", "--pretty", "false"], parse: parse_tsc });
    }
    if has("go.mod") {
        return Some(Checker { tool: "go vet", argv: vec!["go", "vet", "./..."], parse: parse_govet });
    }
    if has("pyproject.toml") || has("ruff.toml") || has(".ruff.toml") {
        return Some(Checker { tool: "ruff", argv: vec!["ruff", "check", "--output-format=concise", "."], parse: parse_ruff });
    }
    if has("package.json") {
        // A JS project without tsconfig: fall back to eslint over the tree.
        return Some(Checker { tool: "eslint", argv: vec!["eslint", "--format", "unix", "."], parse: parse_eslint });
    }
    None
}

/// `diag.check(path?)` — detect + run the checker, workspace-confined + guard-checked.
fn check(args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    let root = ctx.sandbox.as_ref().ok_or("diag.check: no workspace set — diagnostics are disabled")?;
    // An explicit `path` narrows the check to a sub-directory, but it must stay inside the
    // workspace (no `..`, and it must resolve under the root) — same containment as writes.
    let cwd = match arg(args, 0, "path") {
        Some(p) if !p.trim().is_empty() => {
            let target = root.join(p.trim());
            if target.components().any(|c| matches!(c, std::path::Component::ParentDir)) || !target.starts_with(root) {
                return Err("diag.check: path is outside the workspace".into());
            }
            target
        }
        _ => root.clone(),
    };
    let Some(checker) = detect(&cwd).or_else(|| detect(root)) else {
        return Ok(obj(&[
            ("tool", Json::Str("none".into())),
            ("errors", Json::Num(0.0)),
            ("warnings", Json::Num(0.0)),
            ("diagnostics", Json::Arr(Vec::new())),
            ("note", Json::Str("no recognized toolchain (Cargo.toml / tsconfig.json / go.mod / pyproject.toml / package.json)".into())),
        ]));
    };
    // Re-check the checker command through the guard (deny-wins), exactly like `sys.run`.
    let cmd_str = checker.argv.join(" ");
    match ctx.policy.check_command(&cmd_str) {
        crate::security::Verdict::Deny { reason } => return Err(format!("blocked by guard: {reason}")),
        crate::security::Verdict::Confirm { reason } => return Err(format!("requires confirmation (guard): {reason}")),
        crate::security::Verdict::Allow => {}
    }
    let (prog, rest) = checker.argv.split_first().expect("checker argv is non-empty");
    let out = Command::new(prog)
        .args(rest)
        .current_dir(&cwd)
        .output()
        .map_err(|e| format!("diag.check: cannot run `{prog}` ({e}) — is it installed and on PATH?"))?;
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    let mut diags = (checker.parse)(&combined);
    // The checkers report paths relative to their cwd; absolutize them so the Problems panel
    // can open each file directly (and the agent gets a workspace-rooted path).
    for d in &mut diags {
        let rel = d.file.trim_start_matches("./");
        let p = std::path::Path::new(rel);
        if p.is_relative() {
            d.file = cwd.join(p).to_string_lossy().into_owned();
        }
    }
    let truncated = diags.len() > MAX_DIAGNOSTICS;
    diags.truncate(MAX_DIAGNOSTICS);
    let errors = diags.iter().filter(|d| d.severity == Severity::Error).count();
    let warnings = diags.iter().filter(|d| d.severity == Severity::Warning).count();
    Ok(obj(&[
        ("tool", Json::Str(checker.tool.into())),
        ("errors", Json::Num(errors as f64)),
        ("warnings", Json::Num(warnings as f64)),
        ("truncated", Json::Bool(truncated)),
        ("diagnostics", Json::Arr(diags.iter().map(diag_json).collect())),
    ]))
}

fn diag_json(d: &Diagnostic) -> Json {
    obj(&[
        ("file", Json::Str(d.file.clone())),
        ("line", Json::Num(d.line as f64)),
        ("col", Json::Num(d.col as f64)),
        ("severity", Json::Str(d.severity.as_str().into())),
        ("message", Json::Str(d.message.clone())),
    ])
}

// ===== per-toolchain output parsers (pure — unit-tested on captured samples) =====

/// Split a `file:line:col: rest` location prefix (unix paths — no drive colons). Returns
/// `(file, line, col, rest)`; `None` when the 2nd/3rd fields aren't line/col numbers, so a
/// summary line like `error: could not compile …` is skipped.
fn split_colon(line: &str) -> Option<(&str, u32, u32, &str)> {
    let mut parts = line.splitn(4, ':');
    let file = parts.next()?.trim();
    let ln = parts.next()?.trim().parse().ok()?;
    let col = parts.next()?.trim().parse().ok()?;
    let rest = parts.next()?.trim();
    if file.is_empty() {
        return None;
    }
    Some((file, ln, col, rest))
}

/// Split a `file(line,col): rest` location prefix (the TypeScript compiler form).
fn split_paren(line: &str) -> Option<(&str, u32, u32, &str)> {
    let open = line.find('(')?;
    let close = line[open..].find(')')? + open;
    let (l, c) = line[open + 1..close].split_once(',')?;
    let rest = line[close + 1..].strip_prefix(':')?.trim();
    let file = line[..open].trim();
    if file.is_empty() {
        return None;
    }
    Some((file, l.trim().parse().ok()?, c.trim().parse().ok()?, rest))
}

/// Severity from a message tail: a leading `error`/`warning` keyword wins, else `default`.
fn severity_of(rest: &str, default: Severity) -> Severity {
    let low = rest.trim_start().to_ascii_lowercase();
    if low.starts_with("error") {
        Severity::Error
    } else if low.starts_with("warning") || low.starts_with("warn") {
        Severity::Warning
    } else {
        default
    }
}

/// Strip a leading `error[E0425]: ` / `warning: ` / `error TS2304: ` severity prefix so the
/// message reads cleanly (the severity is already captured separately).
fn strip_severity_prefix(rest: &str) -> &str {
    let low = rest.to_ascii_lowercase();
    if (low.starts_with("error") || low.starts_with("warning")) && rest.contains(": ") {
        if let Some(idx) = rest.find(": ") {
            return rest[idx + 2..].trim();
        }
    }
    rest
}

/// `cargo check --message-format=short`: `src/x.rs:4:9: error[E0433]: message`.
fn parse_cargo(out: &str) -> Vec<Diagnostic> {
    out.lines()
        .filter_map(|l| split_colon(l).map(|(file, line, col, rest)| Diagnostic { file: file.into(), line, col, severity: severity_of(rest, Severity::Error), message: strip_severity_prefix(rest).into() }))
        .collect()
}

/// `tsc --noEmit`: `src/index.ts(10,5): error TS2304: Cannot find name 'foo'.`.
fn parse_tsc(out: &str) -> Vec<Diagnostic> {
    out.lines()
        .filter_map(|l| split_paren(l).map(|(file, line, col, rest)| Diagnostic { file: file.into(), line, col, severity: severity_of(rest, Severity::Error), message: strip_severity_prefix(rest).into() }))
        .collect()
}

/// `ruff check --output-format=concise`: `app.py:10:5: F821 Undefined name \`foo\``. Ruff
/// prints no severity word — treat lint findings as warnings.
fn parse_ruff(out: &str) -> Vec<Diagnostic> {
    out.lines()
        .filter_map(|l| split_colon(l).map(|(file, line, col, rest)| Diagnostic { file: file.into(), line, col, severity: Severity::Warning, message: rest.into() }))
        .collect()
}

/// `go vet ./...`: `./main.go:6:2: undefined: foo` (package `# …` header lines don't match).
fn parse_govet(out: &str) -> Vec<Diagnostic> {
    out.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| split_colon(l).map(|(file, line, col, rest)| Diagnostic { file: file.into(), line, col, severity: Severity::Error, message: rest.into() }))
        .collect()
}

/// `eslint --format unix`: `/abs/file.js:10:5: 'x' is never used [Error/no-unused-vars]`.
fn parse_eslint(out: &str) -> Vec<Diagnostic> {
    out.lines()
        .filter_map(|l| {
            split_colon(l).map(|(file, line, col, rest)| {
                // Severity rides in a trailing `[Error/rule]` / `[Warning/rule]` bracket.
                let severity = if rest.to_ascii_lowercase().contains("[error") { Severity::Error } else { Severity::Warning };
                let message = rest.rsplit_once(" [").map(|(m, _)| m).unwrap_or(rest).trim().to_string();
                Diagnostic { file: file.into(), line, col, severity, message }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
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
}
