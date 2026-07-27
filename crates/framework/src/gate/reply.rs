//! Captured PTY bytes → messages a chat app will actually accept.
//!
//! Two decisions carry this module.
//!
//! **Rendering through a scratch [`Term`] rather than stripping ANSI by hand.** The
//! commands people run remotely are exactly the ones with animated output — `cargo`,
//! `npm`, `docker pull` — and those redraw a single line with `\r` and `ESC[2K`
//! hundreds of times. A stripper emits every frame; a terminal emits the final state.
//! Feeding the repo's own VT engine also resolves cursor addressing for free and
//! costs no new parsing code.
//!
//! **HTML `<pre>` rather than Markdown.** Telegram's MarkdownV2 has eighteen special
//! characters, so terminal output containing `_`, `*` or a backtick fails the whole
//! message with a 400 and the reply is silently lost. Escaping `&`, `<` and `>` for
//! `<pre>` is provably complete.

use platform::term::Term;

/// Payload budget per message, in **UTF-16 code units** — the unit Telegram's 4096
/// limit actually counts, so an emoji costs 2. The remainder covers the `<pre>`
/// wrapper and the header line.
const BUDGET: usize = 3800;

/// One command's output, ready to send.
#[derive(Debug, PartialEq)]
pub struct Reply {
    pub messages: Vec<String>,
    /// Output ran past `max_messages` and was cut — the caller offers `/full`.
    pub truncated: bool,
}

/// Replay `bytes` through a terminal and read back the settled plain text.
pub fn to_lines(bytes: &[u8], cols: u16, max_lines: usize) -> Vec<String> {
    let mut t = Term::with_scrollback(cols.max(20), 24, max_lines + 24);
    t.feed(bytes);
    // `content_ansi` treats the cursor row and everything below it as live input (a
    // half-typed prompt) and drops it. Without this newline the LAST line of output —
    // usually the interesting one — silently disappears.
    t.feed(b"\r\n");
    t.content_ansi(max_lines, None).iter().map(|l| strip_ansi(l).trim_end().to_string()).collect()
}

/// Drop CSI sequences, leaving the text. `content_ansi` only re-emits SGR, but
/// stripping every CSI keeps this correct if that ever widens.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match it.peek() {
            Some('[') => {
                it.next();
                // Parameter and intermediate bytes, then one final byte in @..~.
                for f in it.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&f) {
                        break;
                    }
                }
            }
            // A lone ESC (or any other introducer) is not text; drop just the ESC.
            _ => {}
        }
    }
    out
}

/// Length in UTF-16 code units — what a chat API's character limit counts.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Escape the three characters that can break out of an HTML `<pre>` block.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// The UTF-16 cost of one character once escaped.
fn escaped_cost(c: char) -> usize {
    match c {
        '&' => 5,
        '<' | '>' => 4,
        _ => c.len_utf16(),
    }
}

/// Split one raw line into pieces whose **escaped** form fits `budget`. Splitting
/// before escaping is what keeps an entity like `&amp;` from being cut in half.
fn split_line(line: &str, budget: usize) -> Vec<String> {
    if utf16_len(&escape_html(line)) <= budget {
        return vec![line.to_string()];
    }
    let mut pieces = Vec::new();
    let (mut cur, mut cost) = (String::new(), 0usize);
    for c in line.chars() {
        let w = escaped_cost(c);
        if cost + w > budget && !cur.is_empty() {
            pieces.push(std::mem::take(&mut cur));
            cost = 0;
        }
        cur.push(c);
        cost += w;
    }
    if !cur.is_empty() {
        pieces.push(cur);
    }
    pieces
}

/// Build the messages for one command: a header line, then the output in `<pre>`
/// blocks split on line boundaries.
pub fn format(header: &str, lines: &[String], max_messages: usize) -> Reply {
    let max_messages = max_messages.max(1);
    let mut messages: Vec<String> = Vec::new();
    let mut body = String::new();
    let mut used = 0usize;
    let mut truncated = false;

    // Flush the accumulated body as one message, prefixing the header on the first.
    let flush = |messages: &mut Vec<String>, body: &mut String| {
        if body.is_empty() {
            return;
        }
        let block = format!("<pre>{}</pre>", body.trim_end_matches('\n'));
        if messages.is_empty() && !header.is_empty() {
            messages.push(format!("{}\n{block}", escape_html(header)));
        } else {
            messages.push(block);
        }
        body.clear();
    };

    'outer: for line in lines {
        for piece in split_line(line, BUDGET) {
            let esc = escape_html(&piece);
            let w = utf16_len(&esc) + 1; // + the newline
            if used + w > BUDGET && !body.is_empty() {
                flush(&mut messages, &mut body);
                used = 0;
                if messages.len() >= max_messages {
                    truncated = true;
                    break 'outer;
                }
            }
            body.push_str(&esc);
            body.push('\n');
            used += w;
        }
    }
    if messages.len() < max_messages {
        flush(&mut messages, &mut body);
    } else if !body.is_empty() {
        truncated = true;
    }

    // A command with no output still deserves an acknowledgement — silence reads as
    // a dropped message.
    if messages.is_empty() && !header.is_empty() {
        messages.push(escape_html(header));
    }
    Reply { messages, truncated }
}

/// The whole capture as plain text, for the `/full` attachment.
pub fn plain(header: &str, lines: &[String]) -> String {
    let mut s = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum::<usize>() + header.len() + 2);
    if !header.is_empty() {
        s.push_str(header);
        s.push('\n');
    }
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redrawn_progress_line_collapses_to_its_final_state() {
        // The reason this module renders through a Term instead of stripping escapes:
        // `cargo`/`npm`/`docker` rewrite one line hundreds of times.
        assert_eq!(to_lines(b"10%\r50%\r100%\n", 80, 100), vec!["100%"]);
    }

    #[test]
    fn the_last_output_line_is_not_swallowed() {
        // `content_ansi` drops the cursor row as "live input"; the module compensates.
        // Without that, every reply would be missing its final — often only — line.
        assert_eq!(to_lines(b"total 4\r\nREADME.md", 80, 100), vec!["total 4", "README.md"]);
    }

    #[test]
    fn cursor_addressing_and_erase_resolve() {
        assert_eq!(to_lines(b"scratch\x1b[2K\rfinal\r\n", 80, 100), vec!["final"]);
    }

    #[test]
    fn sgr_styling_is_stripped_but_the_text_survives() {
        let lines = to_lines(b"\x1b[31;1mred\x1b[0m plain\r\n", 80, 100);
        assert_eq!(lines, vec!["red plain"]);
    }

    #[test]
    fn escapes_only_the_three_html_characters() {
        // Over-escaping is the other way to lose a message: Markdown's specials must
        // pass through untouched inside <pre>.
        assert_eq!(escape_html("a<b>&c *d* _e_ `f`"), "a&lt;b&gt;&amp;c *d* _e_ `f`");
    }

    #[test]
    fn output_is_wrapped_in_a_pre_block_under_a_header() {
        let r = format("> ls · ok 0", &["a.txt".into(), "b.txt".into()], 3);
        assert_eq!(r.messages, vec!["&gt; ls · ok 0\n<pre>a.txt\nb.txt</pre>"]);
        assert!(!r.truncated);
    }

    #[test]
    fn a_command_with_no_output_still_gets_an_acknowledgement() {
        let r = format("> touch x · ok 0", &[], 3);
        assert_eq!(r.messages.len(), 1);
        assert!(!r.truncated);
    }

    #[test]
    fn long_output_splits_on_line_boundaries_within_the_budget() {
        let lines: Vec<String> = (0..600).map(|i| format!("line {i:04} {}", "x".repeat(60))).collect();
        let r = format("> dump", &lines, 8);
        assert!(r.messages.len() > 1, "expected several messages");
        for m in &r.messages {
            assert!(utf16_len(m) <= 4096, "message of {} units exceeds the API limit", utf16_len(m));
        }
        // Splitting happened between lines, so no line was cut in half.
        let joined = r.messages.join("");
        assert!(joined.contains("line 0000"), "first line present");
    }

    #[test]
    fn output_past_the_message_cap_is_reported_as_truncated() {
        let lines: Vec<String> = (0..4000).map(|i| format!("line {i}")).collect();
        let r = format("> dump", &lines, 3);
        assert_eq!(r.messages.len(), 3);
        assert!(r.truncated, "the caller must be able to offer /full");
    }

    #[test]
    fn an_overlong_single_line_is_hard_split_at_character_boundaries() {
        // 10k CJK characters: one line, no split point, every char 1 UTF-16 unit but
        // 3 UTF-8 bytes. Every chunk must still be valid UTF-8 and within budget.
        let line = "界".repeat(10_000);
        let r = format("", &[line], 20);
        assert!(r.messages.len() >= 3);
        for m in &r.messages {
            assert!(utf16_len(m) <= 4096);
            assert!(m.starts_with("<pre>") && m.ends_with("</pre>"));
        }
        let recovered: String = r.messages.iter().map(|m| m.trim_start_matches("<pre>").trim_end_matches("</pre>").replace('\n', "")).collect();
        assert_eq!(recovered.chars().count(), 10_000, "no characters lost or duplicated");
    }

    #[test]
    fn an_html_entity_is_never_split_across_two_messages() {
        // Splitting the escaped text would let `&amp;` become `&am` + `p;`, which the
        // API rejects. Lines are split BEFORE escaping to make that impossible.
        let line = "&".repeat(2000);
        let r = format("", &[line], 20);
        for m in &r.messages {
            let inner = m.trim_start_matches("<pre>").trim_end_matches("</pre>");
            assert_eq!(inner.replace("&amp;", "").replace('\n', ""), "", "a partial entity leaked: {inner:.40}");
        }
    }

    #[test]
    fn plain_text_export_keeps_the_header_and_every_line() {
        assert_eq!(plain("> ls", &["a".into(), "b".into()]), "> ls\na\nb\n");
    }
}
