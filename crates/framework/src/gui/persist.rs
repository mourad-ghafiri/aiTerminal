//! Config-file persistence helpers — the testable, line-based TOML upsert behind
//! profile config overlays (`crate::profile::config_set`) and any future config
//! write path. No serializer exists (zero-crate), so edits are line-surgical and
//! preserve the user's comments verbatim.

/// Line-based TOML upsert: within `[section]` (until the next `[...]` header or EOF),
/// replace the first line matching `^\s*#?\s*<field>\s*=` with `<field> = <rendered>`
/// — this also UNCOMMENTS a `# field = ...` default. If no such line exists in the
/// section, insert it right after the section header. If the section header is
/// absent, append the section + line at the end.
pub(crate) fn upsert_line(text: &str, section: &str, field: &str, rendered: &str) -> String {
    let header = format!("[{section}]");
    let new_line = format!("{field} = {rendered}");

    let mut out: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut replaced = false;
    let mut header_seen = false;
    let mut insert_at: Option<usize> = None; // index in `out` right after the header

    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with('[') {
            in_section = trimmed.trim_end() == header;
            if in_section {
                header_seen = true;
                out.push(raw.to_string());
                insert_at = Some(out.len()); // insert right after the header line
                continue;
            }
        }
        if in_section && !replaced && line_matches_field(trimmed, field) {
            out.push(new_line.clone());
            replaced = true;
            continue;
        }
        out.push(raw.to_string());
    }

    if !replaced {
        if let Some(i) = insert_at {
            out.insert(i, new_line);
        } else if !header_seen {
            if !out.is_empty() && !out.last().map(|l| l.is_empty()).unwrap_or(true) {
                out.push(String::new());
            }
            out.push(header);
            out.push(new_line);
        }
    }

    let mut s = out.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Whether `line` (already left-trimmed) assigns `field`, allowing an optional
/// leading `#` (a commented default) and surrounding whitespace before `=`.
fn line_matches_field(line: &str, field: &str) -> bool {
    let l = line.strip_prefix('#').map(str::trim_start).unwrap_or(line);
    let Some(rest) = l.strip_prefix(field) else { return false };
    rest.trim_start().starts_with('=')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_replaces_existing_line() {
        let text = "[appearance]\ntheme = \"noir\"\nfont_size = 15\n\n[ai]\nprovider = \"claude\"\n";
        let out = upsert_line(text, "appearance", "theme", "\"nord\"");
        assert!(out.contains("theme = \"nord\""));
        assert!(!out.contains("theme = \"noir\""));
        // other sections / fields untouched
        assert!(out.contains("font_size = 15"));
        assert!(out.contains("provider = \"claude\""));
        // it replaced in-place within [appearance], not [ai]
        assert_eq!(out.matches("theme =").count(), 1);
    }

    #[test]
    fn upsert_uncomments_a_commented_default() {
        // `# model = ...` under [ai] is the default — the upsert must uncomment it.
        let text = "[ai]\nprovider = \"claude\"\n# model      = \"claude-opus-4-8\"\n# fast_model = \"x\"\n";
        let out = upsert_line(text, "ai", "model", "\"gpt-4o\"");
        assert!(out.contains("model = \"gpt-4o\""), "{out}");
        assert!(!out.contains("# model"), "the commented default was replaced: {out}");
        // the OTHER commented line is left alone
        assert!(out.contains("# fast_model = \"x\""));
    }

    #[test]
    fn upsert_inserts_missing_field_under_section() {
        // [ai] has no `provider` line → insert it right after the header.
        let text = "[appearance]\ntheme = \"noir\"\n\n[ai]\nmax_tokens = 16000\n";
        let out = upsert_line(text, "ai", "provider", "\"openai\"");
        assert!(out.contains("provider = \"openai\""));
        let lines: Vec<&str> = out.lines().collect();
        let ai_idx = lines.iter().position(|l| *l == "[ai]").unwrap();
        assert_eq!(lines[ai_idx + 1], "provider = \"openai\"", "inserted right after [ai]");
        // appearance untouched
        assert!(out.contains("theme = \"noir\""));
    }

    #[test]
    fn upsert_appends_a_missing_section() {
        let out = upsert_line("[appearance]\ntheme = \"noir\"\n", "ai", "memory", "false");
        assert!(out.contains("[ai]\nmemory = false"), "{out}");
        assert!(out.contains("theme = \"noir\""));
    }

}
