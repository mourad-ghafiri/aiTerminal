//! `{{…}}` — how one node reads another's work.
//!
//! The old flow had a single `chain` switch: on, and every step received every
//! earlier step's answer glued into one string. That is not state, it is a blob —
//! step five cannot ask for step two, so it pays for steps one through four
//! whether they are relevant or not, and the irrelevant ones actively mislead it.
//!
//! A reference replaces the blob. `{{map.output}}` names exactly one upstream
//! result, so a node's context is something its author chose. And because every
//! reference is parsed here rather than substituted blindly, the verifier can walk
//! them **before the flow runs**: a name that does not exist, or one that is not
//! ordered ahead of the node reading it, is an error rather than an empty string
//! quietly pasted into a prompt.

/// What a `{{…}}` points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ref {
    /// The text typed after the flow name.
    Input,
    /// The flow's own name — handy in prompts that report on themselves.
    FlowName,
    /// The current item of the enclosing `map` node, named by its `as`.
    Var(String),
    /// Another node's result.
    Node { id: String, field: Field },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Output,
    Exit,
}

#[derive(Clone, Debug, PartialEq)]
enum Part {
    Lit(String),
    Ref(Ref),
}

/// A prompt, command or `show` block with its references resolved into structure.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Template {
    parts: Vec<Part>,
    src: String,
}

impl Template {
    /// Read a template. An unclosed or empty `{{…}}` is an error here, so it can be
    /// reported against the file rather than surviving into a prompt.
    pub fn parse(src: &str) -> Result<Template, String> {
        let mut parts = Vec::new();
        let mut rest = src;
        while let Some(open) = rest.find("{{") {
            if open > 0 {
                parts.push(Part::Lit(rest[..open].to_string()));
            }
            let after = &rest[open + 2..];
            let close = after
                .find("}}")
                .ok_or_else(|| format!("unclosed {{{{ in {:?}", clip(src)))?;
            let name = after[..close].trim();
            if name.is_empty() {
                return Err(format!("empty {{{{}}}} in {:?}", clip(src)));
            }
            parts.push(Part::Ref(reference(name)?));
            rest = &after[close + 2..];
        }
        if !rest.is_empty() {
            parts.push(Part::Lit(rest.to_string()));
        }
        Ok(Template { parts, src: src.to_string() })
    }

    /// The text as written.
    pub fn source(&self) -> &str {
        &self.src
    }

    /// Every reference, in order — what the verifier walks.
    pub fn refs(&self) -> Vec<&Ref> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Ref(r) => Some(r),
                Part::Lit(_) => None,
            })
            .collect()
    }

    /// Fill it in. `resolve` cannot fail: by the time a flow runs, every reference
    /// has been proved to name a real, upstream node.
    pub fn render(&self, resolve: &dyn Fn(&Ref) -> String) -> String {
        let mut out = String::with_capacity(self.src.len());
        for part in &self.parts {
            match part {
                Part::Lit(s) => out.push_str(s),
                Part::Ref(r) => out.push_str(&resolve(r)),
            }
        }
        out
    }
}

fn reference(name: &str) -> Result<Ref, String> {
    match name {
        "input" => return Ok(Ref::Input),
        "flow.name" => return Ok(Ref::FlowName),
        _ => {}
    }
    let Some((id, field)) = name.rsplit_once('.') else {
        if !id_ok(name) {
            return Err(format!("{{{{{name}}}}} is not a name"));
        }
        return Ok(Ref::Var(name.to_string()));
    };
    if !id_ok(id) {
        return Err(format!("{{{{{name}}}}} does not start with a node id"));
    }
    let field = match field {
        "output" => Field::Output,
        "exit" => Field::Exit,
        other => {
            return Err(format!(
                "{{{{{id}.{other}}}}} — a node offers `.output` and `.exit`, nothing else"
            ))
        }
    };
    Ok(Ref::Node { id: id.to_string(), field })
}

/// The one id shape used everywhere: node ids, map variables, flow names.
pub(crate) fn id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
}

fn clip(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 48 {
        format!("{}…", one.chars().take(48).collect::<String>())
    } else {
        one
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(src: &str) -> Vec<Ref> {
        Template::parse(src).unwrap().refs().into_iter().cloned().collect()
    }

    #[test]
    fn a_template_separates_what_is_written_from_what_is_referenced() {
        let t = Template::parse("Fix this:\n{{verify.output}}\n\nfor: {{input}}").unwrap();
        assert_eq!(
            t.refs(),
            vec![
                &Ref::Node { id: "verify".into(), field: Field::Output },
                &Ref::Input
            ]
        );
        let filled = t.render(&|r| match r {
            Ref::Input => "add a flag".into(),
            Ref::Node { id, .. } => format!("<{id}>"),
            _ => String::new(),
        });
        assert_eq!(filled, "Fix this:\n<verify>\n\nfor: add a flag");
    }

    #[test]
    fn the_four_kinds_of_reference_are_told_apart() {
        assert_eq!(refs("{{input}}"), vec![Ref::Input]);
        assert_eq!(refs("{{flow.name}}"), vec![Ref::FlowName]);
        assert_eq!(refs("{{file}}"), vec![Ref::Var("file".into())]);
        assert_eq!(refs("{{a.output}}"), vec![Ref::Node { id: "a".into(), field: Field::Output }]);
        assert_eq!(refs("{{a.exit}}"), vec![Ref::Node { id: "a".into(), field: Field::Exit }]);
    }

    #[test]
    fn whitespace_inside_the_braces_is_forgiven() {
        assert_eq!(refs("{{  verify.output  }}"), vec![Ref::Node { id: "verify".into(), field: Field::Output }]);
    }

    #[test]
    fn text_with_no_references_survives_untouched() {
        let t = Template::parse("just words").unwrap();
        assert!(t.refs().is_empty());
        assert_eq!(t.render(&|_| "x".into()), "just words");
    }

    #[test]
    fn a_reference_that_could_never_work_is_rejected_at_parse_time() {
        // Each of these would otherwise become an empty string in a prompt — a bug
        // you only find by reading a transcript and wondering why the agent guessed.
        for (src, want) in [
            ("{{verify.output", "unclosed"),
            ("{{}}", "empty"),
            ("{{a.stdout}}", "`.output` and `.exit`"),
            ("{{a b}}", "is not a name"),
            ("{{.output}}", "does not start with a node id"),
        ] {
            let err = Template::parse(src).expect_err(&format!("{src:?} must not parse"));
            assert!(err.contains(want), "{src:?} said {err:?}, wanted {want:?}");
        }
    }

    #[test]
    fn adjacent_and_repeated_references_both_work() {
        let t = Template::parse("{{a.output}}{{b.output}} and {{a.output}}").unwrap();
        assert_eq!(t.refs().len(), 3, "repeats are kept — each is a substitution site");
        assert_eq!(t.render(&|r| match r {
            Ref::Node { id, .. } => id.to_uppercase(),
            _ => String::new(),
        }), "AB and A");
    }

    #[test]
    fn an_id_is_the_same_shape_everywhere() {
        for good in ["a", "build-web", "run_tests", "step2"] {
            assert!(id_ok(good), "{good}");
        }
        for bad in ["", "-lead", "_lead", "has space", "has.dot", "has/slash", "..", "a b"] {
            assert!(!id_ok(bad), "{bad}");
        }
    }
}
