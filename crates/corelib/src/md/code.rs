//! Syntax highlighting for fenced code — one lexer, per-language tables.
//!
//! Not a parser: a code block in a README or an AI answer needs comments, strings,
//! numbers and keywords told apart, and that is a lexical question. Everything a
//! language-specific grammar would buy beyond this costs far more than it shows at
//! terminal size, so the whole thing is a table plus one scan.
//!
//! An unknown language yields a single `Plain` run, which renders exactly as code did
//! before highlighting existed.

/// What a run of characters is, so the renderer can color it from the theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Plain,
    Comment,
    Str,
    Number,
    Keyword,
    Type,
    /// A whole line that is an addition / removal in a diff.
    Added,
    Removed,
}

/// One language's lexical shape.
struct Spec {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// Quote characters that open a string.
    quotes: &'static [char],
    keywords: &'static [&'static str],
    types: &'static [&'static str],
}

const C_LIKE_TYPES: &[&str] = &["int", "long", "short", "char", "bool", "float", "double", "void", "string", "String", "byte", "true", "false", "null", "nil", "None", "True", "False"];

/// The language table. Families share a spec: what matters is how a line is *lexed*, and
/// most languages answer that the same way.
fn spec(lang: &str) -> Option<Spec> {
    let l = lang.trim().to_ascii_lowercase();
    let l = l.split(['-', '.', ',', ' ']).next().unwrap_or(&l);
    Some(match l {
        "rust" | "rs" => Spec {
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: &['"', '\''],
            keywords: &["as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "type", "unsafe", "use", "where", "while"],
            types: &["bool", "char", "str", "String", "Vec", "Option", "Result", "Some", "None", "Ok", "Err", "usize", "isize", "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32", "f64", "true", "false"],
        },
        "go" | "golang" => Spec {
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: &['"', '`', '\''],
            keywords: &["break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func", "go", "goto", "if", "import", "interface", "map", "package", "range", "return", "select", "struct", "switch", "type", "var"],
            types: &["bool", "string", "int", "int8", "int16", "int32", "int64", "uint", "byte", "rune", "float32", "float64", "error", "nil", "true", "false"],
        },
        "js" | "javascript" | "ts" | "typescript" | "jsx" | "tsx" | "java" | "c" | "cpp" | "c++" | "cs" | "csharp" | "swift" | "kotlin" | "scala" | "php" | "dart" | "zig" => Spec {
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: &['"', '\'', '`'],
            keywords: &["abstract", "async", "await", "break", "case", "catch", "class", "const", "continue", "def", "default", "delete", "do", "else", "enum", "export", "extends", "final", "finally", "for", "from", "func", "function", "if", "implements", "import", "in", "instanceof", "interface", "let", "new", "of", "package", "private", "protected", "public", "return", "static", "super", "switch", "this", "throw", "try", "typeof", "var", "void", "while", "yield"],
            types: C_LIKE_TYPES,
        },
        "python" | "py" => Spec {
            line_comments: &["#"],
            block_comment: None,
            quotes: &['"', '\''],
            keywords: &["and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield"],
            types: &["int", "str", "float", "bool", "list", "dict", "set", "tuple", "bytes", "None", "True", "False", "self"],
        },
        "sh" | "bash" | "zsh" | "shell" | "console" | "fish" | "ruby" | "rb" | "perl" | "r" | "make" | "makefile" | "dockerfile" | "conf" | "ini" | "yaml" | "yml" | "toml" => Spec {
            line_comments: &["#"],
            block_comment: None,
            quotes: &['"', '\''],
            keywords: &["case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in", "local", "return", "select", "set", "then", "unset", "until", "while", "source", "echo", "cd", "true", "false", "def", "end", "require", "module", "class", "FROM", "RUN", "CMD", "COPY", "ENV", "WORKDIR", "ENTRYPOINT"],
            types: &[],
        },
        "sql" => Spec {
            line_comments: &["--"],
            block_comment: Some(("/*", "*/")),
            quotes: &['\'', '"'],
            keywords: &["ALTER", "AND", "AS", "BY", "CREATE", "DELETE", "DROP", "FROM", "GROUP", "HAVING", "INDEX", "INNER", "INSERT", "INTO", "JOIN", "LEFT", "LIMIT", "NOT", "NULL", "ON", "OR", "ORDER", "OUTER", "SELECT", "SET", "TABLE", "UPDATE", "VALUES", "WHERE", "WITH", "select", "from", "where", "insert", "update", "delete", "join", "on", "and", "or", "not", "null", "order", "by", "group", "limit"],
            types: &["int", "integer", "text", "varchar", "boolean", "date", "timestamp", "serial", "primary", "key", "references"],
        },
        "json" => Spec {
            line_comments: &[],
            block_comment: None,
            quotes: &['"'],
            keywords: &["true", "false", "null"],
            types: &[],
        },
        "html" | "xml" | "svg" | "vue" => Spec {
            line_comments: &[],
            block_comment: Some(("<!--", "-->")),
            quotes: &['"', '\''],
            keywords: &[],
            types: &[],
        },
        "css" | "scss" | "less" => Spec {
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: &['"', '\''],
            keywords: &["import", "media", "keyframes", "from", "to", "important"],
            types: &[],
        },
        "lisp" | "clojure" | "clj" | "scheme" | "elisp" => Spec {
            line_comments: &[";"],
            block_comment: None,
            quotes: &['"'],
            keywords: &["def", "defn", "defmacro", "let", "fn", "if", "cond", "when", "do", "loop", "recur", "ns", "require", "lambda", "define"],
            types: &["nil", "true", "false"],
        },
        "diff" | "patch" => Spec { line_comments: &[], block_comment: None, quotes: &[], keywords: &[], types: &[] },
        _ => return None,
    })
}

/// Lexes one code block, line by line, carrying block-comment state across lines.
pub(super) struct Highlighter {
    spec: Option<Spec>,
    diff: bool,
    in_block: bool,
}

impl Highlighter {
    pub fn new(lang: &str) -> Self {
        let l = lang.trim().to_ascii_lowercase();
        Highlighter { spec: spec(&l), diff: matches!(l.as_str(), "diff" | "patch"), in_block: false }
    }

    /// True when this block gets no highlighting at all (unknown language).
    pub fn plain(&self) -> bool {
        self.spec.is_none() && !self.diff
    }

    /// Split one line into colored runs.
    pub fn line(&mut self, text: &str) -> Vec<(Kind, String)> {
        if self.diff {
            let kind = match text.chars().next() {
                Some('+') if !text.starts_with("+++") => Kind::Added,
                Some('-') if !text.starts_with("---") => Kind::Removed,
                Some('@') => Kind::Keyword,
                Some('+') | Some('-') => Kind::Comment,
                _ => Kind::Plain,
            };
            return vec![(kind, text.to_string())];
        }
        let Some(spec) = &self.spec else { return vec![(Kind::Plain, text.to_string())] };
        let mut out: Vec<(Kind, String)> = Vec::new();
        let b = text.as_bytes();
        let mut i = 0;
        let mut plain = String::new();
        let push = |kind: Kind, s: String, out: &mut Vec<(Kind, String)>| {
            if !s.is_empty() {
                out.push((kind, s));
            }
        };
        while i < b.len() {
            // Inside a block comment, everything up to the terminator is comment.
            if self.in_block {
                let (open, close) = spec.block_comment.expect("in a block comment implies the language has them");
                let _ = open;
                match text[i..].find(close) {
                    Some(p) => {
                        push(Kind::Comment, text[i..i + p + close.len()].to_string(), &mut out);
                        i += p + close.len();
                        self.in_block = false;
                    }
                    None => {
                        push(Kind::Comment, text[i..].to_string(), &mut out);
                        i = b.len();
                    }
                }
                continue;
            }
            // A line comment runs to the end of the line.
            if let Some(marker) = spec.line_comments.iter().find(|m| text[i..].starts_with(**m)) {
                let _ = marker;
                push(Kind::Plain, std::mem::take(&mut plain), &mut out);
                push(Kind::Comment, text[i..].to_string(), &mut out);
                i = b.len();
                continue;
            }
            if let Some((open, close)) = spec.block_comment {
                if text[i..].starts_with(open) {
                    push(Kind::Plain, std::mem::take(&mut plain), &mut out);
                    match text[i + open.len()..].find(close) {
                        Some(p) => {
                            let end = i + open.len() + p + close.len();
                            push(Kind::Comment, text[i..end].to_string(), &mut out);
                            i = end;
                        }
                        None => {
                            push(Kind::Comment, text[i..].to_string(), &mut out);
                            i = b.len();
                            self.in_block = true;
                        }
                    }
                    continue;
                }
            }
            let c = b[i] as char;
            // A string runs to its matching quote, honoring backslash escapes.
            if spec.quotes.contains(&c) {
                push(Kind::Plain, std::mem::take(&mut plain), &mut out);
                let start = i;
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] as char == c {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                push(Kind::Str, text[start..i.min(text.len())].to_string(), &mut out);
                continue;
            }
            // A number: digits, with the usual decimal/hex/underscore spellings.
            if c.is_ascii_digit() && !prev_is_word(b, i) {
                push(Kind::Plain, std::mem::take(&mut plain), &mut out);
                let start = i;
                while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                    i += 1;
                }
                push(Kind::Number, text[start..i].to_string(), &mut out);
                continue;
            }
            // A word: keyword, type, or ordinary.
            if c.is_ascii_alphabetic() || c == '_' {
                let start = i;
                while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                let word = &text[start..i];
                let kind = if spec.keywords.contains(&word) {
                    Kind::Keyword
                } else if spec.types.contains(&word) {
                    Kind::Type
                } else {
                    Kind::Plain
                };
                if kind == Kind::Plain {
                    plain.push_str(word);
                } else {
                    push(Kind::Plain, std::mem::take(&mut plain), &mut out);
                    push(kind, word.to_string(), &mut out);
                }
                continue;
            }
            let len = utf8_len(b[i]);
            plain.push_str(&text[i..(i + len).min(text.len())]);
            i += len;
        }
        push(Kind::Plain, plain, &mut out);
        out
    }
}

/// Is the byte before `i` part of a word? (`x2` is one identifier, not `x` then `2`.)
fn prev_is_word(b: &[u8], i: usize) -> bool {
    i > 0 && ((b[i - 1] as char).is_ascii_alphanumeric() || b[i - 1] == b'_')
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lang: &str, src: &str) -> Vec<(Kind, String)> {
        let mut h = Highlighter::new(lang);
        src.lines().flat_map(|l| h.line(l)).collect()
    }

    fn has(runs: &[(Kind, String)], kind: Kind, text: &str) -> bool {
        runs.iter().any(|(k, t)| *k == kind && t.contains(text))
    }

    #[test]
    fn rust_keywords_types_strings_numbers_and_comments() {
        let runs = kinds("rust", "let x: u32 = 42; // the answer\nlet s = \"hi\";");
        assert!(has(&runs, Kind::Keyword, "let"));
        assert!(has(&runs, Kind::Type, "u32"));
        assert!(has(&runs, Kind::Number, "42"));
        assert!(has(&runs, Kind::Comment, "the answer"));
        assert!(has(&runs, Kind::Str, "\"hi\""));
    }

    #[test]
    fn a_block_comment_carries_across_lines() {
        let runs = kinds("js", "/* one\n   two */ let x = 1;");
        assert!(has(&runs, Kind::Comment, "one"));
        assert!(has(&runs, Kind::Comment, "two"));
        assert!(has(&runs, Kind::Keyword, "let"), "code after the comment is code again");
    }

    #[test]
    fn hash_comment_languages() {
        let runs = kinds("bash", "export PATH=/usr/bin # a note");
        assert!(has(&runs, Kind::Keyword, "export"));
        assert!(has(&runs, Kind::Comment, "a note"));
    }

    #[test]
    fn a_hash_inside_a_string_is_not_a_comment() {
        let runs = kinds("python", "s = \"# not a comment\"");
        assert!(has(&runs, Kind::Str, "# not a comment"));
        assert!(!has(&runs, Kind::Comment, "not a comment"));
    }

    #[test]
    fn diff_lines_are_classified_whole() {
        let runs = kinds("diff", "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new");
        assert!(has(&runs, Kind::Removed, "-old"));
        assert!(has(&runs, Kind::Added, "+new"));
        assert!(has(&runs, Kind::Keyword, "@@"));
        assert!(!has(&runs, Kind::Removed, "--- a/x"), "file headers are not removals");
    }

    #[test]
    fn an_unknown_language_is_left_plain() {
        let mut h = Highlighter::new("brainfuck");
        assert!(h.plain());
        assert_eq!(h.line("+[->+<]"), vec![(Kind::Plain, "+[->+<]".to_string())]);
    }

    #[test]
    fn identifiers_that_contain_digits_stay_whole() {
        let runs = kinds("rust", "let x2 = 1;");
        assert!(!has(&runs, Kind::Number, "2"), "x2 is one identifier: {runs:?}");
    }

    #[test]
    fn unterminated_quotes_and_multibyte_never_panic() {
        for (lang, src) in [("rust", "let s = \"unterminated"), ("python", "s = 'héllo"), ("js", "/* open"), ("json", "\"ünicode\": 1")] {
            let _ = kinds(lang, src);
        }
    }
}
