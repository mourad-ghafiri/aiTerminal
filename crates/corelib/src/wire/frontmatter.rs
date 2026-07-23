//! Frontmatter splitter: a leading TOML header fenced by `---` or `+++`, then a
//! Markdown body. Used for `~/.aiTerminal/.terminal/agents/<name>.md` and
//! `ai/skills/<name>.md`, where the header carries metadata (provider, model,
//! tools, schedule, …) and the body is the system prompt / skill instructions.
//!
//! A fence is only recognized when it is the **first line** of the document, so
//! a Markdown thematic break (`---`) inside the body is never mistaken for it.
//! No fence (or no closing fence) ⇒ empty header + the whole text as the body.

use crate::wire::toml::Toml;

/// A parsed frontmatter document.
#[derive(Clone, Debug, PartialEq)]
pub struct Frontmatter {
    /// The TOML header (a [`Toml::Table`]; empty table when there is no header).
    pub header: Toml,
    /// The Markdown body following the header fence (or the whole text).
    pub body: String,
}

impl Frontmatter {
    /// Split `text` into a TOML header + Markdown body.
    pub fn parse(text: &str) -> Frontmatter {
        let no_header = |body: &str| Frontmatter { header: Toml::Table(Vec::new()), body: body.to_string() };

        let delim = match text.lines().next().map(str::trim_end) {
            Some("---") => "---",
            Some("+++") => "+++",
            _ => return no_header(text),
        };

        let mut head = String::new();
        let mut body: Vec<&str> = Vec::new();
        let mut closed = false;
        for line in text.lines().skip(1) {
            if !closed && line.trim_end() == delim {
                closed = true;
                continue;
            }
            if closed {
                body.push(line);
            } else {
                head.push_str(line);
                head.push('\n');
            }
        }

        if !closed {
            // Opening fence with no close ⇒ not frontmatter; keep text verbatim.
            return no_header(text);
        }
        Frontmatter {
            header: Toml::parse(&head).unwrap_or_else(|_| Toml::Table(Vec::new())),
            body: body.join("\n"),
        }
    }

    /// Convenience: a string field from the header.
    pub fn str(&self, key: &str) -> Option<&str> {
        self.header.get(key).and_then(|v| v.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fence_is_all_body() {
        let fm = Frontmatter::parse("# Title\n\nbody text");
        assert_eq!(fm.header, Toml::Table(Vec::new()));
        assert_eq!(fm.body, "# Title\n\nbody text");
    }

    #[test]
    fn dashed_fence_splits_header_and_body() {
        let src = "---\nprovider = \"claude\"\nmodel = \"opus\"\n---\nYou are a helpful agent.\nBe concise.";
        let fm = Frontmatter::parse(src);
        assert_eq!(fm.str("provider"), Some("claude"));
        assert_eq!(fm.str("model"), Some("opus"));
        assert_eq!(fm.body, "You are a helpful agent.\nBe concise.");
    }

    #[test]
    fn plus_fence_works_too() {
        let fm = Frontmatter::parse("+++\nname = \"coder\"\n+++\nSystem prompt.");
        assert_eq!(fm.str("name"), Some("coder"));
        assert_eq!(fm.body, "System prompt.");
    }

    #[test]
    fn thematic_break_in_body_is_not_a_fence() {
        let src = "---\ntitle = \"x\"\n---\nintro\n\n---\n\nmore";
        let fm = Frontmatter::parse(src);
        assert_eq!(fm.str("title"), Some("x"));
        assert_eq!(fm.body, "intro\n\n---\n\nmore");
    }

    #[test]
    fn unclosed_fence_is_all_body() {
        let src = "---\nprovider = \"claude\"\nno closing fence";
        let fm = Frontmatter::parse(src);
        assert_eq!(fm.header, Toml::Table(Vec::new()));
        assert_eq!(fm.body, src);
    }

    #[test]
    fn nested_header_in_frontmatter() {
        let src = "---\n[schedule]\nevery_secs = 3600\n---\nbody";
        let fm = Frontmatter::parse(src);
        assert_eq!(
            fm.header.get("schedule").unwrap().get("every_secs").unwrap().as_int(),
            Some(3600)
        );
    }
}
