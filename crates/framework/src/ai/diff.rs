//! A minimal, from-scratch unified diff (line LCS → hunks) — used to show an agent's
//! `fs.edit`/`fs.write` as a clear `+`/`-` change in the transcript and back to the model.
//! Not a full Myers implementation; good enough for code review, and bounded so a huge
//! rewrite degrades to a one-line summary instead of a giant block.

/// Lines of context kept around each change; longer unchanged runs collapse to `…`.
const CONTEXT: usize = 3;
/// Above this many lines on either side, skip the O(n·m) LCS and summarize.
const MAX_LCS_LINES: usize = 4000;
/// Above this many changed lines, summarize instead of rendering the whole hunk.
const MAX_CHANGED: usize = 400;

/// A ```diff fenced block of `old`→`new`, labelled with `path`. Empty when identical.
pub fn unified_diff(old: &str, new: &str, path: &str) -> String {
    if old == new {
        return String::new();
    }
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let (adds, dels) = (count_only_in(&b, &a), count_only_in(&a, &b));
    if a.len() > MAX_LCS_LINES || b.len() > MAX_LCS_LINES {
        return summary(path, b.len().saturating_sub(a.len()).max(adds), a.len().saturating_sub(b.len()).max(dels));
    }
    let ops = lcs_diff(&a, &b);
    let changed = ops.iter().filter(|o| !matches!(o, Op::Keep(_))).count();
    if changed > MAX_CHANGED {
        let add = ops.iter().filter(|o| matches!(o, Op::Add(_))).count();
        let del = ops.iter().filter(|o| matches!(o, Op::Del(_))).count();
        return summary(path, add, del);
    }
    format!("```diff\n--- {path}\n+++ {path}\n{}```", render_hunks(&ops))
}

fn summary(path: &str, adds: usize, dels: usize) -> String {
    format!("```diff\n# {path}: +{adds} -{dels} lines (diff too large to show)\n```")
}

/// Rough count of lines present in `xs` but not (as a set) in `ys` — only for the
/// too-large fallback, not the rendered diff.
fn count_only_in(xs: &[&str], ys: &[&str]) -> usize {
    use std::collections::HashSet;
    let set: HashSet<&str> = ys.iter().copied().collect();
    xs.iter().filter(|l| !set.contains(*l)).count()
}

enum Op<'a> {
    Keep(&'a str),
    Add(&'a str),
    Del(&'a str),
}

/// Line LCS → keep/del/add ops (deletes before adds at each divergence).
fn lcs_diff<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<Op<'a>> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::Keep(a[i]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(Op::Del(a[i]));
            i += 1;
        } else {
            ops.push(Op::Add(b[j]));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Del(a[i]));
        i += 1;
    }
    while j < m {
        ops.push(Op::Add(b[j]));
        j += 1;
    }
    ops
}

/// Render ops as a unified body: `+`/`-`/space-prefixed lines, with long unchanged runs
/// (further than [`CONTEXT`] from any change) collapsed to a single `…` line.
fn render_hunks(ops: &[Op]) -> String {
    let mut show = vec![false; ops.len()];
    for (i, op) in ops.iter().enumerate() {
        if !matches!(op, Op::Keep(_)) {
            let lo = i.saturating_sub(CONTEXT);
            let hi = (i + CONTEXT + 1).min(ops.len());
            for s in show.iter_mut().take(hi).skip(lo) {
                *s = true;
            }
        }
    }
    let mut out = String::new();
    let mut collapsed = false;
    for (i, op) in ops.iter().enumerate() {
        if !show[i] {
            if !collapsed {
                out.push_str("…\n");
                collapsed = true;
            }
            continue;
        }
        collapsed = false;
        let (sigil, line) = match op {
            Op::Keep(l) => (' ', *l),
            Op::Add(l) => ('+', *l),
            Op::Del(l) => ('-', *l),
        };
        out.push(sigil);
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
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
}
