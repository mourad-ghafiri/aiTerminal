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
mod tests;
