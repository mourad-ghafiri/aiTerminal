//! `re` — a small, std-only regular-expression engine (no third-party crates).
//!
//! Supports a practical subset: literals, `.`, `*` `+` `?` (greedy, with a
//! trailing `?` for lazy), character classes `[...]` (ranges + negation `[^...]`),
//! anchors `^` `$`, alternation `|`, groups `(...)` (non-capturing), the escapes
//! `\d \w \s \D \W \S \n \t \r` and `\<punct>`, and a leading `(?i)` for
//! case-insensitive matching. Matching is a step-capped backtracker, so a
//! pathological pattern fails fast instead of hanging (no ReDoS).
#![forbid(unsafe_code)]

use std::cell::Cell;

/// A compiled regular expression.
#[derive(Clone, Debug)]
pub struct Regex {
    root: Node,
    ci: bool,
    src: String,
    /// The pattern's mandatory literal head (`sk-` in `sk-[a-z]+`), empty when
    /// none — a `contains` prefilter that lets clean text skip the matcher (and
    /// its `Vec<char>` allocation) entirely. Case-sensitive patterns only.
    prefix: String,
}

#[derive(Clone, Debug)]
enum Node {
    Char(char),
    Any,
    Class { neg: bool, items: Vec<ClassItem> },
    Start,
    End,
    /// `\b` (true) / `\B` (false) — word-boundary assertion.
    WordBoundary(bool),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Star(Box<Node>, bool),  // greedy?
    Plus(Box<Node>, bool),
    Quest(Box<Node>, bool),
}

#[derive(Clone, Debug)]
enum ClassItem {
    Ch(char),
    Range(char, char),
    Digit,
    Word,
    Space,
    NotDigit,
    NotWord,
    NotSpace,
}

const STEP_CAP: usize = 1_000_000;

/// The TOTAL matcher-step budget for one `is_match`/`find`/`replace_all` call —
/// shared across every start position (a per-position budget would multiply the
/// cap by the input length: the ReDoS hole this closes). The linear term keeps
/// legitimate scans over large inputs (session context, attachments) inside the
/// budget; a catastrophic pattern still dies in O(input), never O(input × cap).
fn step_budget(input_len: usize) -> usize {
    STEP_CAP + input_len.saturating_mul(64)
}

/// The outcome of a budgeted scan — `Overflow` (budget exhausted) is distinct
/// from `NoMatch` so redaction can refuse to half-apply a pathological rule.
enum FindResult {
    Match(usize, usize),
    NoMatch,
    Overflow,
}

impl Regex {
    /// Compile `pattern`. A leading `(?i)` enables case-insensitive matching.
    pub fn new(pattern: &str) -> Result<Regex, String> {
        let (ci, pat) = match pattern.strip_prefix("(?i)") {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        let chars: Vec<char> = pat.chars().collect();
        let mut p = Parser { chars: &chars, i: 0 };
        let root = p.parse_alt()?;
        if p.i != chars.len() {
            return Err(format!("unexpected `{}` at position {}", chars[p.i], p.i));
        }
        let prefix = if ci { String::new() } else { literal_prefix(&root) };
        Ok(Regex { root, ci, src: pattern.to_string(), prefix })
    }

    /// The original pattern source.
    pub fn as_str(&self) -> &str {
        &self.src
    }

    /// Does `text` contain a match anywhere? Budget exhaustion counts as no
    /// match (never hang).
    pub fn is_match(&self, text: &str) -> bool {
        if self.prefiltered_out(text) {
            return false;
        }
        let chars: Vec<char> = text.chars().collect();
        let steps = Cell::new(0);
        let budget = step_budget(chars.len());
        matches!(self.find_in(&chars, 0, &steps, budget), FindResult::Match(..))
    }

    /// The leftmost match span (in `char` indices), if any. Part of the complete
    /// regex primitive even when the security layer doesn't currently call it.
    #[allow(dead_code)]
    pub fn find(&self, text: &str) -> Option<(usize, usize)> {
        if self.prefiltered_out(text) {
            return None;
        }
        let chars: Vec<char> = text.chars().collect();
        let steps = Cell::new(0);
        let budget = step_budget(chars.len());
        match self.find_in(&chars, 0, &steps, budget) {
            FindResult::Match(s, e) => Some((s, e)),
            _ => None,
        }
    }

    /// Replace every non-overlapping match with `replacement`. Part of the
    /// complete regex primitive; hot callers use [`replace_all_opt`](Self::replace_all_opt).
    #[allow(dead_code)]
    pub fn replace_all(&self, text: &str, replacement: &str) -> String {
        self.replace_all_opt(text, replacement).unwrap_or_else(|| text.to_string())
    }

    /// Like [`replace_all`](Self::replace_all) but `None` when the text is
    /// UNTOUCHED — no match, prefilter miss, or budget overflow — so hot callers
    /// (per-chunk redaction) skip the reallocation. On overflow the input is
    /// returned unchanged and a warning is logged: never hang, and never emit a
    /// silently half-redacted string.
    pub fn replace_all_opt(&self, text: &str, replacement: &str) -> Option<String> {
        if self.prefiltered_out(text) {
            return None;
        }
        let chars: Vec<char> = text.chars().collect();
        let steps = Cell::new(0);
        let budget = step_budget(chars.len());
        let mut out = String::new();
        let mut i = 0;
        let mut replaced = false;
        while i <= chars.len() {
            match self.find_in(&chars, i, &steps, budget) {
                FindResult::Match(s, e) => {
                    replaced = true;
                    out.extend(chars[i..s].iter());
                    out.push_str(replacement);
                    if e > s {
                        i = e;
                    } else {
                        // empty match: emit one char to make progress
                        if s < chars.len() {
                            out.push(chars[s]);
                        }
                        i = s + 1;
                    }
                }
                FindResult::NoMatch => {
                    if !replaced {
                        return None;
                    }
                    out.extend(chars[i..].iter());
                    break;
                }
                FindResult::Overflow => {
                    platform::warn!(
                        "regex `{}` exhausted its step budget on a {}-char input — rule skipped for this text",
                        self.src,
                        chars.len()
                    );
                    return None;
                }
            }
        }
        Some(out)
    }

    /// `true` when the literal-prefix prefilter proves `text` cannot match.
    fn prefiltered_out(&self, text: &str) -> bool {
        !self.prefix.is_empty() && !text.contains(&self.prefix)
    }

    /// Scan for the leftmost match from `from`, drawing every start position's
    /// work from ONE shared step budget.
    fn find_in(&self, chars: &[char], from: usize, steps: &Cell<usize>, budget: usize) -> FindResult {
        for start in from..=chars.len() {
            let m = Matcher { chars, ci: self.ci, steps, budget };
            let mut end = None;
            let ok = m.m(&self.root, start, &mut |p| {
                end = Some(p);
                true
            });
            if ok {
                return FindResult::Match(start, end.unwrap());
            }
            if steps.get() >= budget {
                return FindResult::Overflow;
            }
        }
        FindResult::NoMatch
    }
}

/// The pattern's mandatory literal head: a run of plain `Char` nodes at the start
/// (a leading `^` anchor is transparent). Empty when the pattern starts with
/// anything else — classes, alternation, quantifiers.
fn literal_prefix(root: &Node) -> String {
    let mut out = String::new();
    match root {
        Node::Char(c) => out.push(*c),
        Node::Concat(seq) => {
            for n in seq {
                match n {
                    Node::Char(c) => out.push(*c),
                    Node::Start if out.is_empty() => continue,
                    _ => break,
                }
            }
        }
        _ => {}
    }
    out
}

struct Matcher<'a> {
    chars: &'a [char],
    ci: bool,
    /// The CALL-wide step counter — shared across all start positions of one
    /// `find_in` scan, so the cap bounds the whole operation.
    steps: &'a Cell<usize>,
    budget: usize,
}

impl Matcher<'_> {
    fn budget(&self) -> bool {
        let s = self.steps.get() + 1;
        self.steps.set(s);
        s < self.budget
    }

    /// Match `node` at `pos`, then call the continuation `k` with the new
    /// position; returns whether the whole thing (node + continuation) matched.
    fn m(&self, node: &Node, pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        if !self.budget() {
            return false;
        }
        match node {
            Node::Char(c) => pos < self.chars.len() && eqc(self.chars[pos], *c, self.ci) && k(pos + 1),
            Node::Any => pos < self.chars.len() && self.chars[pos] != '\n' && k(pos + 1),
            Node::Class { neg, items } => {
                pos < self.chars.len() && class_match(self.chars[pos], *neg, items, self.ci) && k(pos + 1)
            }
            Node::Start => pos == 0 && k(pos),
            Node::End => pos == self.chars.len() && k(pos),
            Node::WordBoundary(want) => (self.is_boundary(pos) == *want) && k(pos),
            Node::Concat(seq) => self.m_seq(seq, pos, k),
            Node::Alt(alts) => alts.iter().any(|a| self.m(a, pos, k)),
            Node::Star(inner, g) => self.m_star(inner, pos, *g, k),
            Node::Plus(inner, g) => self.m(inner, pos, &mut |p| self.m_star(inner, p, *g, k)),
            Node::Quest(inner, g) => {
                if *g {
                    self.m(inner, pos, k) || k(pos)
                } else {
                    k(pos) || self.m(inner, pos, k)
                }
            }
        }
    }

    fn is_boundary(&self, pos: usize) -> bool {
        let before = pos > 0 && is_word(self.chars[pos - 1]);
        let after = pos < self.chars.len() && is_word(self.chars[pos]);
        before != after
    }

    fn m_seq(&self, seq: &[Node], pos: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        match seq.split_first() {
            None => k(pos),
            Some((first, rest)) => self.m(first, pos, &mut |p| self.m_seq(rest, p, k)),
        }
    }

    fn m_star(&self, inner: &Node, pos: usize, greedy: bool, k: &mut dyn FnMut(usize) -> bool) -> bool {
        if greedy {
            self.m(inner, pos, &mut |p| p > pos && self.m_star(inner, p, greedy, k)) || k(pos)
        } else {
            k(pos) || self.m(inner, pos, &mut |p| p > pos && self.m_star(inner, p, greedy, k))
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn eqc(a: char, b: char, ci: bool) -> bool {
    if ci {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

fn class_match(c: char, neg: bool, items: &[ClassItem], ci: bool) -> bool {
    let hit = items.iter().any(|it| item_match(c, it, ci));
    hit != neg
}

fn item_match(c: char, it: &ClassItem, ci: bool) -> bool {
    match it {
        ClassItem::Ch(x) => eqc(c, *x, ci),
        ClassItem::Range(a, b) => {
            if ci {
                let lc = c.to_ascii_lowercase();
                let uc = c.to_ascii_uppercase();
                (lc >= *a && lc <= *b) || (uc >= *a && uc <= *b) || (c >= *a && c <= *b)
            } else {
                c >= *a && c <= *b
            }
        }
        ClassItem::Digit => c.is_ascii_digit(),
        ClassItem::Word => c.is_alphanumeric() || c == '_',
        ClassItem::Space => c.is_whitespace(),
        ClassItem::NotDigit => !c.is_ascii_digit(),
        ClassItem::NotWord => !(c.is_alphanumeric() || c == '_'),
        ClassItem::NotSpace => !c.is_whitespace(),
    }
}

fn class1(it: ClassItem) -> Node {
    Node::Class { neg: false, items: vec![it] }
}

struct Parser<'a> {
    chars: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    fn parse_alt(&mut self) -> Result<Node, String> {
        let mut alts = vec![self.parse_concat()?];
        while self.peek() == Some('|') {
            self.bump();
            alts.push(self.parse_concat()?);
        }
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn parse_concat(&mut self) -> Result<Node, String> {
        let mut seq = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            seq.push(self.parse_quant()?);
        }
        Ok(match seq.len() {
            1 => seq.pop().unwrap(),
            _ => Node::Concat(seq),
        })
    }

    fn parse_quant(&mut self) -> Result<Node, String> {
        let atom = self.parse_atom()?;
        let node = match self.peek() {
            Some('*') => {
                self.bump();
                Node::Star(Box::new(atom), self.greedy())
            }
            Some('+') => {
                self.bump();
                Node::Plus(Box::new(atom), self.greedy())
            }
            Some('?') => {
                self.bump();
                Node::Quest(Box::new(atom), self.greedy())
            }
            Some('{') if self.brace_ahead() => self.parse_brace(atom)?,
            _ => atom,
        };
        Ok(node)
    }

    /// Is `{` the start of a `{n}` / `{n,}` / `{n,m}` quantifier (vs a literal)?
    fn brace_ahead(&self) -> bool {
        matches!(self.chars.get(self.i + 1), Some(c) if c.is_ascii_digit() || *c == ',')
    }

    fn parse_brace(&mut self, atom: Node) -> Result<Node, String> {
        self.bump(); // '{'
        let min_opt = self.parse_num();
        let (min, max) = if self.peek() == Some(',') {
            self.bump();
            (min_opt.unwrap_or(0), self.parse_num()) // {n,} / {n,m} / {,m}
        } else {
            let n = min_opt.ok_or("empty {}")?;
            (n, Some(n))
        };
        if self.bump() != Some('}') {
            return Err("missing `}`".to_string());
        }
        let greedy = self.greedy();
        // Desugar into copies of `atom` over the existing node kinds.
        let mut seq: Vec<Node> = Vec::new();
        for _ in 0..min {
            seq.push(atom.clone());
        }
        match max {
            None => seq.push(Node::Star(Box::new(atom), greedy)),
            Some(m) => {
                for _ in min..m {
                    seq.push(Node::Quest(Box::new(atom.clone()), greedy));
                }
            }
        }
        Ok(match seq.len() {
            1 => seq.pop().unwrap(),
            _ => Node::Concat(seq),
        })
    }

    fn parse_num(&mut self) -> Option<usize> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
        }
        if self.i == start {
            None
        } else {
            self.chars[start..self.i].iter().collect::<String>().parse().ok()
        }
    }

    /// A trailing `?` after a quantifier makes it lazy (non-greedy).
    fn greedy(&mut self) -> bool {
        if self.peek() == Some('?') {
            self.bump();
            false
        } else {
            true
        }
    }

    fn parse_atom(&mut self) -> Result<Node, String> {
        match self.bump() {
            Some('(') => {
                let inner = self.parse_alt()?;
                if self.bump() != Some(')') {
                    return Err("missing `)`".to_string());
                }
                Ok(inner) // non-capturing group is transparent
            }
            Some('[') => self.parse_class(),
            Some('.') => Ok(Node::Any),
            Some('^') => Ok(Node::Start),
            Some('$') => Ok(Node::End),
            Some('\\') => self.parse_escape(),
            Some(c) => Ok(Node::Char(c)),
            None => Err("unexpected end of pattern".to_string()),
        }
    }

    fn parse_escape(&mut self) -> Result<Node, String> {
        match self.bump() {
            Some('d') => Ok(class1(ClassItem::Digit)),
            Some('w') => Ok(class1(ClassItem::Word)),
            Some('s') => Ok(class1(ClassItem::Space)),
            Some('D') => Ok(class1(ClassItem::NotDigit)),
            Some('W') => Ok(class1(ClassItem::NotWord)),
            Some('S') => Ok(class1(ClassItem::NotSpace)),
            Some('b') => Ok(Node::WordBoundary(true)),
            Some('B') => Ok(Node::WordBoundary(false)),
            Some('n') => Ok(Node::Char('\n')),
            Some('t') => Ok(Node::Char('\t')),
            Some('r') => Ok(Node::Char('\r')),
            Some(c) => Ok(Node::Char(c)),
            None => Err("trailing backslash".to_string()),
        }
    }

    fn parse_class(&mut self) -> Result<Node, String> {
        let neg = if self.peek() == Some('^') {
            self.bump();
            true
        } else {
            false
        };
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == ']' {
                self.bump();
                return Ok(Node::Class { neg, items });
            }
            self.bump();
            let ch = if c == '\\' {
                match self.bump() {
                    Some('d') => {
                        items.push(ClassItem::Digit);
                        continue;
                    }
                    Some('w') => {
                        items.push(ClassItem::Word);
                        continue;
                    }
                    Some('s') => {
                        items.push(ClassItem::Space);
                        continue;
                    }
                    Some('D') => {
                        items.push(ClassItem::NotDigit);
                        continue;
                    }
                    Some('W') => {
                        items.push(ClassItem::NotWord);
                        continue;
                    }
                    Some('S') => {
                        items.push(ClassItem::NotSpace);
                        continue;
                    }
                    Some('n') => '\n',
                    Some('t') => '\t',
                    Some('r') => '\r',
                    Some(x) => x,
                    None => return Err("bad class escape".to_string()),
                }
            } else {
                c
            };
            // range a-b (but not a trailing `-` before `]`)
            if self.peek() == Some('-') && self.chars.get(self.i + 1).is_some_and(|&x| x != ']') {
                self.bump(); // '-'
                let mut end = self.bump().ok_or("bad range")?;
                if end == '\\' {
                    end = self.bump().ok_or("bad range")?;
                }
                items.push(ClassItem::Range(ch, end));
            } else {
                items.push(ClassItem::Ch(ch));
            }
        }
        Err("unclosed `[`".to_string())
    }
}

#[cfg(test)]
mod tests {
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
}
