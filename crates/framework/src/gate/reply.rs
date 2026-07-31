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
mod tests;
