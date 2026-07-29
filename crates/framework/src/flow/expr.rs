//! The `when` language — the one place a flow makes a decision.
//!
//! Graph engineering's sharpest rule is that routing belongs on the **edge**, not
//! inside a prompt: an agent asked to decide what happens next decides differently
//! each time, and nothing about the run can be audited afterwards. So a node's
//! `when` is data, parsed here into a tiny tree, and evaluated against facts the
//! scheduler already knows.
//!
//! The grammar is deliberately not a programming language — no arithmetic, no
//! calls, no assignment, no way to reach the filesystem. It can ask five things:
//!
//! ```text
//! verify.passed · verify.failed · verify.skipped · verify.ran · gate.approved
//! verify.exit == 1        (also != < >)
//! verify.output contains "0 failed"
//! verify.output matches /(\d+) failed/
//! verify.output == "clean"
//! not X   ·   X and Y   ·   X or Y   ·   ( … )
//! ```
//!
//! That ceiling is the point. Everything expressible here can be checked before
//! the flow runs — [`Expr::nodes`] hands the verifier every node an expression
//! names, so a condition that mentions a node that does not exist, or one that is
//! not upstream, is a parse-time error rather than a silent `false` at midnight.

use crate::security::regex::Regex;

/// A parsed `when` condition.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr {
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    /// `verify.passed` and friends.
    State { node: String, want: State },
    /// `verify.exit == 1`.
    Exit { node: String, op: Cmp, value: i64 },
    /// `verify.output contains "…"`.
    Text { node: String, op: TextOp, value: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Passed,
    Failed,
    Skipped,
    /// It executed at all — true for both a pass and a failure.
    Ran,
    /// An `approve` node the person said yes to.
    Approved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cmp {
    Eq,
    Ne,
    Lt,
    Gt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextOp {
    Contains,
    Matches,
    Equals,
}

/// What is known about one node when a condition is evaluated.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Facts {
    pub ran: bool,
    pub passed: bool,
    pub skipped: bool,
    pub approved: bool,
    /// A command node's exit status.
    pub exit: Option<i64>,
    pub output: String,
}

impl Expr {
    /// Evaluate against a lookup of node facts.
    ///
    /// A node with no facts — never reached, or named in a condition on a branch
    /// that was retired — is **false** for every test rather than an error. By the
    /// time a flow runs, the verifier has already proved every name exists and is
    /// upstream, so the only way to get here is a branch that legitimately did not
    /// happen, and "it did not pass" is the truthful answer.
    pub fn eval(&self, facts: &dyn Fn(&str) -> Option<Facts>) -> bool {
        match self {
            Expr::Not(inner) => !inner.eval(facts),
            Expr::And(a, b) => a.eval(facts) && b.eval(facts),
            Expr::Or(a, b) => a.eval(facts) || b.eval(facts),
            Expr::State { node, want } => facts(node).is_some_and(|f| match want {
                State::Passed => f.passed,
                State::Failed => f.ran && !f.passed,
                State::Skipped => f.skipped,
                State::Ran => f.ran,
                State::Approved => f.approved,
            }),
            Expr::Exit { node, op, value } => facts(node)
                .and_then(|f| f.exit)
                .is_some_and(|got| match op {
                    Cmp::Eq => got == *value,
                    Cmp::Ne => got != *value,
                    Cmp::Lt => got < *value,
                    Cmp::Gt => got > *value,
                }),
            Expr::Text { node, op, value } => facts(node).is_some_and(|f| match op {
                TextOp::Contains => f.output.contains(value.as_str()),
                TextOp::Equals => f.output.trim() == value,
                // Compiled at parse time too, so an unparseable pattern is reported
                // then; here a bad one can only be false, never a panic.
                TextOp::Matches => Regex::new(value).map(|re| re.is_match(&f.output)).unwrap_or(false),
            }),
        }
    }

    /// Every node id this condition names, in order of first appearance — what the
    /// verifier walks to prove a condition only looks upstream.
    pub fn nodes(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<String>) {
        match self {
            Expr::Not(inner) => inner.collect(out),
            Expr::And(a, b) | Expr::Or(a, b) => {
                a.collect(out);
                b.collect(out);
            }
            Expr::State { node, .. } | Expr::Exit { node, .. } | Expr::Text { node, .. } => {
                if !out.iter().any(|n| n == node) {
                    out.push(node.clone());
                }
            }
        }
    }
}

// ────────────────────────────── the parser ──────────────────────────────

/// Read a `when` expression, or say precisely what is wrong with it.
pub(crate) fn parse(src: &str) -> Result<Expr, String> {
    let tokens = lex(src)?;
    let mut p = Parser { tokens, at: 0 };
    let expr = p.or()?;
    if p.at < p.tokens.len() {
        return Err(format!("unexpected {:?} after the condition", p.tokens[p.at].text()));
    }
    Ok(expr)
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Re(String),
    Num(i64),
    Op(String),
    Open,
    Close,
}

impl Tok {
    fn text(&self) -> String {
        match self {
            Tok::Word(w) => w.clone(),
            Tok::Str(s) => format!("{s:?}"),
            Tok::Re(r) => format!("/{r}/"),
            Tok::Num(n) => n.to_string(),
            Tok::Op(o) => o.clone(),
            Tok::Open => "(".into(),
            Tok::Close => ")".into(),
        }
    }
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                out.push(Tok::Open);
                i += 1;
            }
            ')' => {
                out.push(Tok::Close);
                i += 1;
            }
            '"' | '\'' => {
                let quote = c;
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j] != quote {
                    j += 1;
                }
                if j >= chars.len() {
                    return Err(format!("unclosed {quote} in the condition"));
                }
                out.push(Tok::Str(chars[start..j].iter().collect()));
                i = j + 1;
            }
            '/' => {
                let start = i + 1;
                let mut j = start;
                // A regex may contain an escaped slash, which must not end it.
                while j < chars.len() && !(chars[j] == '/' && chars[j - 1] != '\\') {
                    j += 1;
                }
                if j >= chars.len() {
                    return Err("unclosed / in the condition — a pattern is /like this/".into());
                }
                out.push(Tok::Re(chars[start..j].iter().collect()));
                i = j + 1;
            }
            '=' | '!' | '<' | '>' => {
                let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
                if two == "==" || two == "!=" {
                    out.push(Tok::Op(two));
                    i += 2;
                } else if c == '<' || c == '>' {
                    out.push(Tok::Op(c.to_string()));
                    i += 1;
                } else {
                    return Err(format!("'{c}' alone is not an operator — did you mean '=='?"));
                }
            }
            c if c.is_ascii_digit() || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) => {
                let start = i;
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                out.push(Tok::Num(text.parse().map_err(|_| format!("{text:?} is not a number"))?));
            }
            c if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '-' || chars[i] == '.')
                {
                    i += 1;
                }
                out.push(Tok::Word(chars[start..i].iter().collect()));
            }
            other => return Err(format!("'{other}' has no meaning in a condition")),
        }
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<Tok>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at)
    }

    fn eat_word(&mut self, want: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case(want)) {
            self.at += 1;
            return true;
        }
        false
    }

    fn or(&mut self) -> Result<Expr, String> {
        let mut left = self.and()?;
        while self.eat_word("or") {
            left = Expr::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        while self.eat_word("and") {
            left = Expr::And(Box::new(left), Box::new(self.unary()?));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat_word("not") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.peek().cloned() {
            Some(Tok::Open) => {
                self.at += 1;
                let inner = self.or()?;
                if self.peek() != Some(&Tok::Close) {
                    return Err("a '(' in the condition was never closed".into());
                }
                self.at += 1;
                Ok(inner)
            }
            Some(Tok::Word(w)) => {
                self.at += 1;
                self.test(&w)
            }
            Some(other) => Err(format!("expected a node test, found {}", other.text())),
            None => Err("the condition ended early".into()),
        }
    }

    /// `<node>.<field>` on its own, or followed by an operator and an operand.
    fn test(&mut self, word: &str) -> Result<Expr, String> {
        let (node, field) = word
            .rsplit_once('.')
            .ok_or_else(|| format!("{word:?} needs a field — try {word}.passed or {word}.output contains \"…\""))?;
        if node.is_empty() {
            return Err(format!("{word:?} is missing a node name before the '.'"));
        }
        let node = node.to_string();
        match field {
            "passed" | "ok" => Ok(Expr::State { node, want: State::Passed }),
            "failed" => Ok(Expr::State { node, want: State::Failed }),
            "skipped" => Ok(Expr::State { node, want: State::Skipped }),
            "ran" => Ok(Expr::State { node, want: State::Ran }),
            "approved" => Ok(Expr::State { node, want: State::Approved }),
            "exit" => {
                let op = match self.peek().cloned() {
                    Some(Tok::Op(o)) => {
                        self.at += 1;
                        match o.as_str() {
                            "==" => Cmp::Eq,
                            "!=" => Cmp::Ne,
                            "<" => Cmp::Lt,
                            _ => Cmp::Gt,
                        }
                    }
                    _ => return Err(format!("{node}.exit needs a comparison, e.g. {node}.exit == 1")),
                };
                match self.peek().cloned() {
                    Some(Tok::Num(v)) => {
                        self.at += 1;
                        Ok(Expr::Exit { node, op, value: v })
                    }
                    _ => Err(format!("{node}.exit compares against a number")),
                }
            }
            "output" => {
                let op = match self.peek().cloned() {
                    Some(Tok::Word(w)) if w.eq_ignore_ascii_case("contains") => {
                        self.at += 1;
                        TextOp::Contains
                    }
                    Some(Tok::Word(w)) if w.eq_ignore_ascii_case("matches") => {
                        self.at += 1;
                        TextOp::Matches
                    }
                    Some(Tok::Op(o)) if o == "==" => {
                        self.at += 1;
                        TextOp::Equals
                    }
                    _ => {
                        return Err(format!(
                            "{node}.output needs `contains \"…\"`, `matches /…/` or `== \"…\"`"
                        ))
                    }
                };
                match (op, self.peek().cloned()) {
                    (TextOp::Matches, Some(Tok::Re(r))) => {
                        self.at += 1;
                        // Compile now so a broken pattern is a parse error, not a
                        // condition that quietly never fires.
                        Regex::new(&r).map_err(|e| format!("{node}.output matches /{r}/: {e}"))?;
                        Ok(Expr::Text { node, op, value: r })
                    }
                    (TextOp::Matches, _) => Err(format!("{node}.output matches needs a /pattern/")),
                    (_, Some(Tok::Str(s))) => {
                        self.at += 1;
                        Ok(Expr::Text { node, op, value: s })
                    }
                    _ => Err(format!("{node}.output needs a quoted string to compare against")),
                }
            }
            other => Err(format!(
                "{node}.{other} is not something a condition can ask — try passed, failed, skipped, ran, approved, exit or output"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts_for(pairs: Vec<(&str, Facts)>) -> impl Fn(&str) -> Option<Facts> {
        let owned: Vec<(String, Facts)> = pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        move |name: &str| owned.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    fn passed() -> Facts {
        Facts { ran: true, passed: true, exit: Some(0), output: "0 failed".into(), ..Facts::default() }
    }

    fn failed() -> Facts {
        Facts { ran: true, passed: false, exit: Some(1), output: "2 failed".into(), ..Facts::default() }
    }

    #[test]
    fn the_five_node_states_read_the_way_they_are_written() {
        let f = facts_for(vec![("verify", failed())]);
        for (src, want) in [
            ("verify.failed", true),
            ("verify.passed", false),
            ("verify.ran", true),
            ("verify.skipped", false),
        ] {
            assert_eq!(parse(src).unwrap().eval(&f), want, "{src}");
        }
        let f = facts_for(vec![("verify", passed())]);
        assert!(parse("verify.passed").unwrap().eval(&f));
        assert!(!parse("verify.failed").unwrap().eval(&f));
    }

    #[test]
    fn an_exit_status_compares_four_ways() {
        let f = facts_for(vec![("verify", failed())]);
        for (src, want) in
            [("verify.exit == 1", true), ("verify.exit != 1", false), ("verify.exit > 0", true), ("verify.exit < 1", false)]
        {
            assert_eq!(parse(src).unwrap().eval(&f), want, "{src}");
        }
    }

    #[test]
    fn output_can_be_matched_literally_or_by_pattern() {
        let f = facts_for(vec![("verify", failed())]);
        assert!(parse(r#"verify.output contains "failed""#).unwrap().eval(&f));
        assert!(!parse(r#"verify.output contains "passed""#).unwrap().eval(&f));
        assert!(parse(r"verify.output matches /[0-9]+ failed/").unwrap().eval(&f));
        assert!(!parse(r"verify.output matches /^clean$/").unwrap().eval(&f));
        assert!(parse(r#"verify.output == "2 failed""#).unwrap().eval(&f));
    }

    #[test]
    fn and_or_not_compose_with_parentheses() {
        let f = facts_for(vec![("a", passed()), ("b", failed())]);
        assert!(parse("a.passed and b.failed").unwrap().eval(&f));
        assert!(!parse("a.passed and b.passed").unwrap().eval(&f));
        assert!(parse("a.failed or b.failed").unwrap().eval(&f));
        assert!(parse("not b.passed").unwrap().eval(&f));
        // `and` binds tighter than `or`, and parentheses override it.
        assert!(parse("b.passed and a.passed or b.failed").unwrap().eval(&f));
        assert!(!parse("b.passed and (a.passed or b.failed)").unwrap().eval(&f));
    }

    #[test]
    fn a_node_with_no_facts_is_false_not_an_error() {
        // The branch never happened. "It did not pass" is the truthful answer, and a
        // flow that asks about a retired branch must not die at 3am because of it.
        let f = facts_for(vec![]);
        assert!(!parse("verify.passed").unwrap().eval(&f));
        assert!(!parse("verify.failed").unwrap().eval(&f));
        assert!(!parse("verify.exit == 0").unwrap().eval(&f));
        assert!(parse("not verify.passed").unwrap().eval(&f), "negation still works");
    }

    #[test]
    fn an_approval_is_its_own_state() {
        let yes = Facts { ran: true, passed: true, approved: true, ..Facts::default() };
        let no = Facts { ran: true, passed: true, approved: false, ..Facts::default() };
        assert!(parse("gate.approved").unwrap().eval(&facts_for(vec![("gate", yes)])));
        assert!(!parse("gate.approved").unwrap().eval(&facts_for(vec![("gate", no)])));
    }

    #[test]
    fn every_named_node_is_reported_for_verification() {
        let e = parse(r#"a.passed and (b.exit == 1 or c.output contains "x") and a.failed"#).unwrap();
        assert_eq!(e.nodes(), vec!["a", "b", "c"], "each named once, in the order written");
    }

    #[test]
    fn a_broken_condition_says_what_is_wrong_instead_of_being_false() {
        // Every one of these would otherwise be a condition that silently never
        // fires — the failure mode this whole module exists to prevent.
        for (src, want) in [
            ("verify", "needs a field"),
            ("verify.exploded", "is not something a condition can ask"),
            ("verify.exit", "needs a comparison"),
            ("verify.exit == yes", "compares against a number"),
            ("verify.output", "needs `contains"),
            ("verify.output contains failed", "needs a quoted string"),
            ("verify.output matches /[unclosed/", "matches /[unclosed/"),
            ("(a.passed", "never closed"),
            ("a.passed and", "ended early"),
            ("a.passed b.passed", "unexpected"),
            (r#"a.output contains "x"#, "unclosed \""),
            ("a.passed & b.passed", "no meaning"),
            ("a.exit = 1", "did you mean '=='"),
        ] {
            let err = parse(src).expect_err(&format!("{src:?} must not parse"));
            assert!(err.contains(want), "{src:?} said {err:?}, wanted something about {want:?}");
        }
    }

    #[test]
    fn a_regex_may_contain_an_escaped_slash() {
        let e = parse(r"a.output matches /a\/b/").unwrap();
        assert_eq!(e, Expr::Text { node: "a".into(), op: TextOp::Matches, value: r"a\/b".into() });
    }

    #[test]
    fn node_ids_with_dashes_and_underscores_survive_the_lexer() {
        let e = parse("build-web.passed and run_tests.failed").unwrap();
        assert_eq!(e.nodes(), vec!["build-web", "run_tests"]);
    }
}
