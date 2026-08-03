//! The guard's declarative vocabulary, and the one parser that reads it.
//!
//! A rule means the same thing wherever it is written — `config.toml`, a profile, or any
//! plugin's `plugin.toml` — because all three come through [`RuleSet::parse`]. There is no
//! second spelling and no per-surface dialect: a user who learns the three tables from the
//! config file can read a plugin's rules, and a plugin author needs no separate reference.
//!
//! ```toml
//! [[guard.command]]
//! pattern = "\\bsudo\\b"
//! rule    = "confirm"        # deny | confirm | allow | auto      (default deny)
//!
//! [[guard.path]]
//! pattern = "(^|/)\\.ssh/"
//! rule    = "deny"           # deny | read-only | allow           (default deny)
//!
//! [[guard.secret]]
//! pattern = "AKIA[0-9A-Z]{16}"
//! name    = "aws-key"        # names its placeholder: «aws-key-1»
//! scope   = "ai"             # ai | terminal | all                (default ai)
//! literal = false            # true = an exact string, no regex
//! ```
//!
//! Nothing here compiles a regex or decides anything. This is the raw, inert form a
//! document carries; [`super::Guard`] is what a set of them compiles into.

use corelib::wire::Toml;

/// What a command rule does when it matches.
///
/// `Allow` is the allow-LIST tier, not a permission: once any allow rule exists, a command
/// matching none of them is denied. That is why it lives beside the refusing tiers rather
/// than reading as their opposite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandRule {
    Deny,
    Confirm,
    Allow,
    /// The Auto-mode safe-list: the agent may run a match un-prompted. Orthogonal to the
    /// three tiers above — `deny`/`confirm` still win.
    Auto,
}

/// What a path rule does when it matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathRule {
    /// Neither read nor written.
    Deny,
    /// Read freely, never modified.
    ReadOnly,
    /// The allow-LIST tier, as for commands.
    Allow,
}

/// Where a secret rule applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Everything bound for a model, a tool, or a chat. Reversible: the value comes back
    /// when the text returns to this machine.
    Ai,
    /// The screen. Irreversible — you cannot restore a screen, and someone who scoped a
    /// rule here meant "I do not want to see this".
    Terminal,
    All,
}

impl Scope {
    /// Absent → `Ai`, the documented default: `cat .env` still shows you your own values,
    /// and only what leaves is rewritten.
    ///
    /// Present but unrecognised → `All`, the **strictest** reading, matching what a
    /// misspelt command or path rule already does. Falling back to egress-only would leave
    /// somebody who wrote `scope = "termnal"` screen-sharing a secret they asked to hide,
    /// and a typo must never quietly widen anything.
    fn parse(s: &str) -> Scope {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "ai" => Scope::Ai,
            "terminal" | "term" => Scope::Terminal,
            _ => Scope::All,
        }
    }
    /// Whether a rule in this scope applies when hiding text bound off this machine.
    pub fn hides(self) -> bool {
        matches!(self, Scope::Ai | Scope::All)
    }
    /// Whether a rule in this scope applies when masking text for the screen.
    pub fn masks(self) -> bool {
        matches!(self, Scope::Terminal | Scope::All)
    }
}

/// One `[[guard.command]]` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub pattern: String,
    pub rule: CommandRule,
}

/// One `[[guard.path]]` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    pub pattern: String,
    pub rule: PathRule,
}

/// One `[[guard.secret]]` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Secret {
    pub pattern: String,
    /// Names the placeholder a match is replaced by (`«aws-key-1»`). Empty → `secret`.
    /// It names the *rule*, never the value: a placeholder that leaked a hint of what it
    /// stood for would be a smaller secret rather than no secret.
    pub name: String,
    pub scope: Scope,
    pub literal: bool,
}

/// Everything one document says about the guard.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuleSet {
    pub commands: Vec<Command>,
    pub paths: Vec<Path>,
    pub secrets: Vec<Secret>,
}

impl RuleSet {
    /// Read a `[guard]` section. A table with no `pattern` is skipped rather than failing
    /// the document: a config file must always load, and [`super::build`] reports what it
    /// could not use.
    pub fn parse(guard: &Toml) -> RuleSet {
        RuleSet {
            commands: tables(guard, "command")
                .map(|t| Command { pattern: pattern(t), rule: command_rule(word(t, "rule")) })
                .collect(),
            paths: tables(guard, "path")
                .map(|t| Path { pattern: pattern(t), rule: path_rule(word(t, "rule")) })
                .collect(),
            secrets: tables(guard, "secret")
                .map(|t| Secret {
                    pattern: pattern(t),
                    name: word(t, "name"),
                    scope: Scope::parse(&word(t, "scope")),
                    literal: t.get("literal").and_then(|v| v.as_bool()).unwrap_or(false),
                })
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.paths.is_empty() && self.secrets.is_empty()
    }

    /// Fold `other` in after this one. Order is the precedence a *tie* resolves by — the
    /// first rule to match names itself in the refusal — so config is folded before
    /// plugins and a user's own rule gets first refusal.
    pub fn extend(&mut self, other: RuleSet) {
        self.commands.extend(other.commands);
        self.paths.extend(other.paths);
        self.secrets.extend(other.secrets);
    }
}

/// The `[[guard.<kind>]]` tables of a `[guard]` section, in document order. A rule with no
/// `pattern` is dropped here: an empty pattern matches at every position, so a templating
/// slip must not become a rule that refuses everything.
fn tables<'a>(guard: &'a Toml, kind: &str) -> impl Iterator<Item = &'a Toml> {
    guard.get(kind).and_then(|v| v.as_array()).unwrap_or(&[]).iter().filter(|t| !pattern(t).is_empty())
}

fn pattern(t: &Toml) -> String {
    t.get("pattern").and_then(|v| v.as_str()).unwrap_or_default().to_string()
}

fn word(t: &Toml, key: &str) -> String {
    t.get(key).and_then(|v| v.as_str()).unwrap_or_default().trim().to_ascii_lowercase()
}

/// Unknown words fall to the strictest reading. A typo in a rule word (`rule = "confrim"`)
/// must never quietly widen what may run.
fn command_rule(word: String) -> CommandRule {
    match word.as_str() {
        "confirm" | "ask" => CommandRule::Confirm,
        "allow" => CommandRule::Allow,
        "auto" => CommandRule::Auto,
        _ => CommandRule::Deny,
    }
}

fn path_rule(word: String) -> PathRule {
    match word.as_str() {
        "read-only" | "read_only" | "readonly" | "read" => PathRule::ReadOnly,
        "allow" => PathRule::Allow,
        _ => PathRule::Deny,
    }
}
