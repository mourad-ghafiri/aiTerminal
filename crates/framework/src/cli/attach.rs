// ── attachments: `@<path>` tokens in the prompt ─────────────────────────────

/// Raw-size cap for an attached image/PDF (base64 grows it ~4/3 on the wire).
use crate::cli::agentloop::MAX_ATTACHMENTS;

pub(crate) const MEDIA_ATTACH_MAX: u64 = 4 * 1024 * 1024;
/// Inline cap for an attached text file.
pub(crate) const TEXT_ATTACH_MAX: usize = 48 * 1024;

/// The attachment media type for a path, by extension: `Some(image/*)`,
/// `Some(application/pdf)`, or `None` (treat as text).
fn media_type_of(path: &std::path::Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("pdf") => Some("application/pdf"),
        _ => None,
    }
}

/// Scan the prompt for `@<path>` tokens naming EXISTING files and turn them into
/// attachments: images + PDFs become request media (vision / document caps),
/// text files inline into the context (fenced, size-capped, skipped if binary).
/// The `@` is dropped from the prompt so the model reads a plain path. Pure over
/// the filesystem — no model, no network.
pub(crate) fn collect_attachments(prompt: &str) -> (String, Vec<crate::ai::ImageData>, String) {
    collect_attachments_in(&std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")), prompt)
}

/// [`collect_attachments`], with relative `@paths` resolved against `base` — the
/// workspace passes its ROOT, because the GUI process's own cwd is nowhere near it.
pub(crate) fn collect_attachments_in(base: &std::path::Path, prompt: &str) -> (String, Vec<crate::ai::ImageData>, String) {
    let mut media = Vec::new();
    let mut file_ctx = String::new();
    let mut out: Vec<String> = Vec::new();
    let mut attached = 0usize;
    for token in prompt.split_whitespace() {
        let Some(path_str) = token.strip_prefix('@').filter(|r| !r.is_empty()) else {
            out.push(token.to_string());
            continue;
        };
        let joined;
        let path = match std::path::Path::new(path_str).is_absolute() {
            true => std::path::Path::new(path_str),
            false => {
                joined = base.join(path_str);
                joined.as_path()
            }
        };
        if !path.is_file() {
            out.push(token.to_string()); // not a file — leave the token as typed
            continue;
        }
        // Bound the COUNT too: N × (raw + base64 + request copy) peaks fast.
        if attached >= MAX_ATTACHMENTS {
            eprintln!("aiTerminal: skipping {path_str} (over {MAX_ATTACHMENTS} attachments)");
            out.push(path_str.to_string());
            continue;
        }
        attached += 1;
        match media_type_of(path) {
            Some(mt) => {
                let too_big = std::fs::metadata(path).map(|m| m.len() > MEDIA_ATTACH_MAX).unwrap_or(true);
                if too_big {
                    eprintln!("aiTerminal: skipping {path_str} (over {} MB)", MEDIA_ATTACH_MAX / (1024 * 1024));
                } else if let Ok(bytes) = std::fs::read(path) {
                    media.push(crate::ai::ImageData { media_type: mt.to_string(), b64: corelib::codec::base64_encode(&bytes) });
                }
            }
            None => {
                if let Ok(bytes) = std::fs::read(path) {
                    if bytes.contains(&0) {
                        eprintln!("aiTerminal: skipping {path_str} (binary)");
                    } else {
                        let mut text = String::from_utf8_lossy(&bytes).into_owned();
                        if text.len() > TEXT_ATTACH_MAX {
                            let mut cut = TEXT_ATTACH_MAX;
                            while cut < text.len() && !text.is_char_boundary(cut) {
                                cut += 1;
                            }
                            text.truncate(cut);
                            text.push_str("\n… (truncated)\n");
                        }
                        file_ctx.push_str(&format!("\n## Attached file: {path_str}\n```\n{text}\n```\n"));
                    }
                }
            }
        }
        out.push(path_str.to_string());
    }
    (out.join(" "), media, file_ctx)
}
