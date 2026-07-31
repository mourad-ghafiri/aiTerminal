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
mod tests;
