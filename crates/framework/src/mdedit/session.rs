use std::io::{IsTerminal, Read, Write};

use crate::mdedit::chrome::ScreenGuard;
use crate::mdedit::editor::{Editor, layout};
use crate::mdedit::key::parse_key;
use crate::mdedit::preview::build_preview;

/// Run the interactive split editor on `path`. Returns a process exit code.
pub fn run(path: &str) -> i32 {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("@md edit needs an interactive terminal.");
        return 2;
    }
    // Missing file → start empty (created on first save); other errors are fatal.
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            eprintln!("@md: cannot read {path}: {e}");
            return 1;
        }
    };

    let Some(_raw) = platform::os::raw_mode() else {
        eprintln!("@md edit: could not enter raw mode.");
        return 2;
    };
    let _screen = ScreenGuard::enter();
    let sigwinch = platform::os::sigwinch_flag();

    let mut ed = Editor::new(path, &text);
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut pending: Vec<u8> = Vec::new();
    let mut rd = [0u8; 1024];
    let mut redraw = true;

    while !ed.quit {
        let size = platform::os::terminal_size().map(|(c, r)| (c as usize, r as usize)).unwrap_or((80, 24));
        if redraw {
            let l = layout(size.0, size.1, ed.buf.lines.len());
            let preview = build_preview(&ed.buf.text(), l.preview_w, crate::cli::md_style());
            let frame = ed.frame(&preview, size);
            let _ = stdout.write_all(frame.as_bytes());
            let _ = stdout.flush();
            redraw = false;
        }
        match stdin.read(&mut rd) {
            Ok(0) => break,
            Ok(n) => pending.extend_from_slice(&rd[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                if sigwinch.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    redraw = true;
                }
                continue;
            }
            Err(_) => break,
        }
        let l = layout(size.0, size.1, ed.buf.lines.len());
        while let Some((key, used)) = parse_key(&pending) {
            pending.drain(..used);
            ed.on_key(key, &l);
            redraw = true;
            if ed.quit {
                break;
            }
        }
    }
    0
}
