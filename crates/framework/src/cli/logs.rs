//! Reading a run's log back.
//!
//! Every log a run leaves is a `.md` on disk — `# node` / `## asked` / `## answered` for a
//! flow node, `## iteration 3` / `### verifier` for a loop, an agent's own answer for a
//! job. We were writing documents and then printing them as syntax, so the person who
//! asked what their agent said was shown `## heading` and a raw diagram fence.
//!
//! ONE reader for all three commands, because `@job log` had grown its own copy of the
//! same tail loop and copies do not learn from each other.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cli::media::{write_chunk, DIAGRAM_LANG};
use crate::cli::style::{md_style, md_width, out_is_tty};

/// How long the follower waits between looks at a growing file.
const POLL: std::time::Duration = std::time::Duration::from_millis(300);

/// How a run's log reaches the screen.
///
/// A Strategy, because there are exactly two shapes and the difference matters at every
/// byte. A Markdown document is **drawn** — block by block as the file grows, which is
/// what makes `-f` work on a document at all rather than waiting for the run to end. A
/// command's output goes through **untouched**, because a build log is not prose: a line
/// starting `#` is a comment and not a heading, and re-wrapping `git status` is not
/// reading it.
pub(crate) enum LogSink {
    /// The renderer, and the folder the log lives in — so a relative image inside a
    /// transcript resolves the way the document meant it.
    Drawn { md: Box<corelib::md::StreamRenderer>, base: PathBuf },
    Verbatim,
}

impl LogSink {
    /// The sink for `path`: drawn only when the log **is** a document and there is a
    /// terminal to draw it on. A pipe gets the file's own bytes, so `@job log > run.md`
    /// still writes what was written.
    pub(crate) fn open(markdown: bool, tty: bool, path: &Path) -> LogSink {
        if !markdown || !tty {
            return LogSink::Verbatim;
        }
        LogSink::Drawn {
            md: Box::new(corelib::md::StreamRenderer::new(md_style(), md_width(), &[DIAGRAM_LANG])),
            base: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        }
    }

    /// Another slice of the log — everything appended since the last call.
    pub(crate) fn feed(&mut self, w: &mut dyn Write, text: &str) {
        match self {
            LogSink::Drawn { md, base } => {
                for c in md.push(text) {
                    write_chunk(w, c, base, true);
                }
            }
            LogSink::Verbatim => {
                let _ = w.write_all(text.as_bytes());
            }
        }
        let _ = w.flush();
    }

    /// The log is not going to grow again: commit whatever block was still open.
    pub(crate) fn close(&mut self, w: &mut dyn Write) {
        if let LogSink::Drawn { md, base } = self {
            for c in md.finish() {
                write_chunk(w, c, base, true);
            }
        }
        let _ = w.flush();
    }
}

/// Print a log, then — with `follow` — keep printing what is appended while `live` holds.
///
/// `markdown` is what the caller knows about what it is holding, never a guess about the
/// text: an agent writes Markdown and a command writes whatever it writes, and every
/// caller here can already tell which from the record it just read.
pub(crate) fn show_log(path: &Path, follow: bool, markdown: bool, live: &dyn Fn() -> bool) -> i32 {
    use std::io::{Read, Seek};
    let Ok(mut f) = std::fs::File::open(path) else {
        eprintln!("aiTerminal: can't read {}", path.display());
        return 1;
    };
    let mut sink = LogSink::open(markdown, out_is_tty(), path);
    let mut out = std::io::stdout();
    let mut text = String::new();
    let _ = f.read_to_string(&mut text);
    sink.feed(&mut out, &text);
    if !follow {
        sink.close(&mut out);
        return 0;
    }
    // Follow: poll for growth while the run is still going, so `-f` ends by itself.
    let mut at = f.stream_position().unwrap_or(0);
    loop {
        std::thread::sleep(POLL);
        if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > at {
            let _ = f.seek(std::io::SeekFrom::Start(at));
            let mut more = String::new();
            let _ = f.read_to_string(&mut more);
            at += more.len() as u64;
            sink.feed(&mut out, &more);
        }
        if !live() {
            sink.close(&mut out);
            return 0;
        }
    }
}
