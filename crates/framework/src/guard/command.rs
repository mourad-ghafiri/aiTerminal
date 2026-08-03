//! Reading a command line, and judging what it would do.
//!
//! A shell line is not one thing. `a && b | c` runs three programs, a pasted suggestion
//! can carry several lines, and every one of them runs. So the guard never judges "the
//! command" — it judges every **line**, every top-level **segment** of every line, and
//! each segment's **program**, because a benign first word must not shield what follows it.
//!
//! It also reads the **paths** a command names. A path rule that only reached the `fs.*`
//! tools would be decoration: `cat ~/.ssh/id_rsa` never goes near them.

use std::path::PathBuf;

use super::path::Paths;
use super::regex::Regex;
use super::rules::CommandRule;
use super::{Base, Decision};

/// The command tiers, compiled.
#[derive(Clone, Default)]
pub(crate) struct Commands {
    deny: Vec<Regex>,
    confirm: Vec<Regex>,
    allow: Vec<Regex>,
    auto: Vec<Regex>,
}

impl Commands {
    /// Add a compiled rule to its tier.
    pub(crate) fn add(&mut self, rule: CommandRule, re: Regex) {
        match rule {
            CommandRule::Deny => self.deny.push(re),
            CommandRule::Confirm => self.confirm.push(re),
            CommandRule::Allow => self.allow.push(re),
            CommandRule::Auto => self.auto.push(re),
        }
    }

    /// The patterns of a tier, for the briefing.
    pub(crate) fn patterns(&self, rule: CommandRule) -> Vec<&str> {
        let tier = match rule {
            CommandRule::Deny => &self.deny,
            CommandRule::Confirm => &self.confirm,
            CommandRule::Allow => &self.allow,
            CommandRule::Auto => &self.auto,
        };
        tier.iter().map(|r| r.as_str()).collect()
    }

    /// **deny > confirm > allow-list**, over every probe the line yields, and then over the
    /// paths it names.
    pub(crate) fn judge(&self, cmd: &str, paths: &Paths, base: &Base) -> Decision {
        let segments = segments_of(cmd);
        if segments.is_empty() {
            return Decision::Allow;
        }
        let probes = probes(cmd, &segments);
        if let Some(d) = first_match(&self.deny, &probes, "a denied command") {
            return d.deny();
        }
        // A path the command names is a read of that path, whatever program does the
        // reading. Judged after the command tiers so a deny rule names itself first.
        //
        // The refusal quotes the token as the command WROTE it (`~/.ssh/id_rsa`), not the
        // absolute path it resolved to: that is the thing you would go and change.
        for token in named_paths(&segments) {
            if let Some(why) = paths.refuses_read(&resolve(&token, base)) {
                return Decision::Deny { reason: format!("it names {token:?}, which {why}") };
            }
        }
        if let Some(d) = first_match(&self.confirm, &probes, "a confirm-first command") {
            return d.confirm();
        }
        // Allow-list mode: every SEGMENT must be allow-listed. Per segment rather than per
        // line, because `ls | <anything>` would otherwise sail through an `^ls` allow rule.
        if !self.allow.is_empty() {
            if let Some(seg) = segments.iter().find(|s| !self.allow.iter().any(|r| r.is_match(s))) {
                return Decision::Deny { reason: format!("{seg:?} is not in the allow-list") };
            }
        }
        Decision::Allow
    }

    /// Whether Auto mode may run this un-prompted. A pure read of the `auto` tier — the
    /// tiers above are consulted separately and still win. An empty tier means nothing
    /// qualifies, so Auto then prompts for everything.
    pub(crate) fn auto_runs(&self, cmd: &str) -> bool {
        let segments = segments_of(cmd);
        !segments.is_empty() && segments.iter().all(|s| self.auto.iter().any(|r| r.is_match(s)))
    }
}

/// A matched rule, named so the refusal can quote it.
struct Hit {
    probe: String,
    pattern: String,
    kind: &'static str,
}

impl Hit {
    fn deny(self) -> Decision {
        Decision::Deny { reason: self.why() }
    }
    fn confirm(self) -> Decision {
        Decision::Confirm { reason: self.why() }
    }
    fn why(&self) -> String {
        format!("{:?} matches {}  /{}/", self.probe, self.kind, self.pattern)
    }
}

fn first_match(tier: &[Regex], probes: &[String], kind: &'static str) -> Option<Hit> {
    probes.iter().find_map(|p| {
        tier.iter().find(|r| r.is_match(p)).map(|r| Hit { probe: p.clone(), pattern: r.as_str().to_string(), kind })
    })
}

/// Everything a deny/confirm rule is tested against: the whole command, each segment, and
/// each segment's program basename — so `/usr/bin/sudo` is caught by a rule written
/// `\bsudo\b`, and a pipeline cannot hide a program behind a harmless first stage.
fn probes(cmd: &str, segments: &[String]) -> Vec<String> {
    let mut out = vec![cmd.trim().to_string()];
    for seg in segments {
        out.push(seg.clone());
        if let Some(prog) = program_of(seg) {
            if !out.iter().any(|p| *p == prog) {
                out.push(prog);
            }
        }
    }
    out
}

/// Non-empty top-level segments of every line: split on `;` `&&` `||` `|` `&` and newlines,
/// ignoring separators inside quotes. The shell does the real parsing; this exists so each
/// stage can be judged on its own.
pub(crate) fn segments_of(cmd: &str) -> Vec<String> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut segs: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    let push = |cur: &mut String, segs: &mut Vec<String>| {
        let s = cur.trim().to_string();
        cur.clear();
        if !s.is_empty() {
            segs.push(s);
        }
    };
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '|' | '&' => {
                    push(&mut cur, &mut segs);
                    if i + 1 < chars.len() && chars[i + 1] == c {
                        i += 1; // consume the doubled operator (`&&` / `||`)
                    }
                }
                ';' | '\n' => push(&mut cur, &mut segs),
                _ => cur.push(c),
            },
        }
        i += 1;
    }
    push(&mut cur, &mut segs);
    segs
}

/// Split a command into argv, honoring single/double quotes (no other shell features — no
/// globs, `$()`, or redirection). An unterminated quote is an ERROR rather than a silently
/// mangled token, so the guard and the shell never disagree about where a word ends.
pub fn split(cmd: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in cmd.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            None => {
                cur.push(c);
                any = true;
            }
        }
    }
    if quote.is_some() {
        return Err("unterminated quote in the command".into());
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// The program a segment runs, by basename — `git`, not `/usr/local/bin/git`.
///
/// Leading `NAME=value` words are the shell's way of setting a variable for one command,
/// not the command: `FOO=1 sudo apt` runs `sudo`. Reading the assignment as the program
/// would let a rule anchored `^sudo` be walked past by typing one env var first.
pub(crate) fn program_of(segment: &str) -> Option<String> {
    let argv = split(segment).ok()?;
    let prog = argv.iter().find(|w| !is_assignment(w))?;
    Some(std::path::Path::new(prog).file_name().and_then(|n| n.to_str()).unwrap_or(prog).to_string())
}

/// `NAME=value` — a shell variable assignment rather than a word of the command.
fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

/// The paths a command names, as written.
///
/// A heuristic, and honestly so: it catches `/etc/x`, `~/.ssh/id_rsa`, `./build`, `a/b` and
/// `>secrets/out.txt`, and it will not catch a path assembled at run time. That is the same
/// bargain the command rules already make — this closes the everyday bypass, and the guard
/// remains a speed bump rather than a sandbox.
pub(crate) fn named_paths(segments: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for seg in segments {
        let Ok(argv) = split(seg) else { continue };
        // Including the FIRST word, when it looks like a path. Running a script is reading
        // it, so `./deploy.sh` and `/etc/cron.d/x` are reads of exactly the paths a rule is
        // about — and a bare `ls` carries no separator, so nothing ordinary is affected.
        for token in argv {
            let t = token
                .trim_start_matches(['<', '>', '&', '(', '{', '`', '$'])
                .trim_end_matches([';', ',', ')', '}', '`']);
            // A URL is not a path, a flag is not a path even when it carries a slash
            // (`--exclude=a/b` is one, but reading it as a path would refuse more than it
            // protects), and neither is `FOO=bar`. All three are left to the command rules.
            if t.contains("://") || t.starts_with('-') || t.is_empty() || is_assignment(t) {
                continue;
            }
            let looks_like = t.starts_with('/') || t.starts_with('~') || t.starts_with("./") || t.starts_with("../") || t.contains('/');
            if looks_like && !out.iter().any(|p| p == t) {
                out.push(t.to_string());
            }
        }
    }
    out
}

/// A named path as an absolute one: `~` against home, anything relative against the
/// directory the run was started in. Both come from [`Base`], captured once at build, so
/// judging stays pure and a test can point them somewhere harmless.
pub(crate) fn resolve(token: &str, base: &Base) -> PathBuf {
    if let Some(rest) = token.strip_prefix("~/") {
        return base.home.as_ref().map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(token));
    }
    if token == "~" {
        return base.home.clone().unwrap_or_else(|| PathBuf::from(token));
    }
    let p = PathBuf::from(token);
    if p.is_absolute() {
        return p;
    }
    base.cwd.as_ref().map(|c| c.join(&p)).unwrap_or(p)
}
