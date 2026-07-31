
use crate::mdedit::buffer::Buffer;
use crate::mdedit::editor::{Editor, Mode, layout};
use crate::mdedit::key::{Key, parse_key};
use crate::mdedit::pager::Pager;
use crate::mdedit::preview::{DiagramPaint, PRow, build_preview_with, hslice_ansi, hslice_plain, preview_height};

#[test]
fn buffer_edits_across_multibyte_lines() {
    let mut b = Buffer::from_str("héllo\nworld");
    assert_eq!(b.lines.len(), 2);
    // Move to end of "héllo" and insert.
    b.cx = 5;
    b.insert_char('!');
    assert_eq!(b.lines[0], "héllo!");
    // Newline splits the line at the cursor.
    b.cx = 1;
    b.cy = 1;
    b.insert_newline();
    assert_eq!(b.lines[1], "w");
    assert_eq!(b.lines[2], "orld");
    // Backspace at column 0 joins with the previous line.
    b.cx = 0;
    b.cy = 2;
    b.backspace();
    assert_eq!(b.lines[1], "world");
    assert!(b.dirty);
}

#[test]
fn backspace_over_a_multibyte_char_removes_one_char() {
    let mut b = Buffer::from_str("café");
    b.cx = 4;
    b.backspace();
    assert_eq!(b.lines[0], "caf");
    assert_eq!(b.cx, 3);
}

#[test]
fn hslice_plain_clips_and_pads_by_display_width() {
    // Full within width → unchanged + padded.
    assert_eq!(hslice_plain("abc", 0, 5), "abc  ");
    // Left offset drops leading columns.
    assert_eq!(hslice_plain("abcdef", 2, 3), "cde");
    // A wide (2-col) char is not split across the right edge.
    let s = hslice_plain("a世b", 0, 2); // 'a'(1) + '世'(2) would exceed 2 → stop after 'a'
    assert_eq!(s, "a ");
}

#[test]
fn hslice_ansi_preserves_color_across_the_cut() {
    let styled = "\x1b[31mRED\x1b[0mplain";
    let out = hslice_ansi(styled, 0, 4);
    assert!(out.starts_with("\x1b[31m"), "keeps the opening color");
    assert!(out.contains("RED"));
    assert!(out.ends_with("\x1b[0m"));
    // Offsetting past the colored run still carries the SGR verbatim.
    let out2 = hslice_ansi(styled, 3, 5);
    assert!(out2.contains("\x1b[31m") && out2.contains("plain"));
}

#[test]
fn parse_key_handles_text_controls_and_sequences() {
    assert_eq!(parse_key(b"a"), Some((Key::Char('a'), 1)));
    assert_eq!(parse_key(b"\r"), Some((Key::Enter, 1)));
    assert_eq!(parse_key(b"\x13"), Some((Key::Ctrl('s'), 1))); // Ctrl+S
    assert_eq!(parse_key(b"\x1b[A"), Some((Key::Up, 3)));
    assert_eq!(parse_key(b"\x1b[3~"), Some((Key::Delete, 4)));
    assert_eq!(parse_key(b"\x1b[6~"), Some((Key::PageDown, 4)));
    assert_eq!(parse_key(b"\x1b"), Some((Key::Esc, 1)));
    // Incomplete CSI → None until the rest arrives.
    assert_eq!(parse_key(b"\x1b["), None);
    // A multibyte char split across reads waits.
    assert_eq!(parse_key(&[0xc3]), None);
    assert_eq!(parse_key("é".as_bytes()), Some((Key::Char('é'), 2)));
}

#[test]
fn parse_sgr_mouse_decodes_button_and_zero_based_cell() {
    // Wheel-up at 1-based (10, 5) → 0-based (9, 4).
    assert_eq!(parse_key(b"\x1b[<64;10;5M"), Some((Key::Mouse { btn: 64, col: 9, row: 4, pressed: true }, 11)));
    // Left release.
    assert_eq!(parse_key(b"\x1b[<0;3;2m"), Some((Key::Mouse { btn: 0, col: 2, row: 1, pressed: false }, 9)));
    // Incomplete → None.
    assert_eq!(parse_key(b"\x1b[<64;10"), None);
}

#[test]
fn preview_model_reserves_rows_for_diagrams() {
    let doc = "# Title\n\n```mermaid\nflowchart TD\n A-->B\n```\n";
    let rows = build_preview_with(doc, 60, plain_style(), DiagramPaint::Art);
    let diagram_rows = rows.iter().filter(|r| matches!(r, PRow::Object { .. })).count();
    assert!(diagram_rows >= 3, "a diagram reserves several rows: {diagram_rows}");
    assert!(rows.iter().any(|r| matches!(r, PRow::Text(t) if t.contains("Title"))));
    // The diagram source is carried, not shown as text.
    assert!(!rows.iter().any(|r| matches!(r, PRow::Text(t) if t.contains("flowchart"))));
}

#[test]
fn layout_splits_into_editor_divider_preview() {
    let l = layout(81, 24, 10);
    assert_eq!(l.body_h, 22);
    assert_eq!(l.editor_w + 1 + l.preview_w, 81);
    assert!(l.text_w < l.editor_w, "gutter takes some editor width");
}

#[test]
fn confirm_flow_saves_or_discards_only_when_dirty() {
    let mut ed = Editor::new("x.md", "hi");
    let l = layout(80, 24, 1);
    // Clean buffer: Ctrl+Q quits immediately (no prompt).
    ed.on_key(Key::Ctrl('q'), &l);
    assert!(ed.quit);
    // Dirty buffer: Ctrl+Q asks; Esc cancels back to editing.
    let mut ed = Editor::new("x.md", "hi");
    ed.buf.insert_char('!');
    ed.on_key(Key::Ctrl('q'), &l);
    assert!(!ed.quit && ed.mode == Mode::Confirm);
    ed.on_key(Key::Esc, &l);
    assert!(ed.mode == Mode::Edit);
}

#[test]
fn pager_scroll_clamps_to_content() {
    let len = 100;
    let body_h = 20;
    let mut pg = Pager::new("x.md");
    // Down past the end clamps to the last page.
    pg.on_key(Key::End, body_h, len);
    assert_eq!(pg.top, len - body_h);
    pg.on_key(Key::Down, body_h, len);
    assert_eq!(pg.top, len - body_h, "cannot scroll past the bottom");
    // Home returns to the top; Up clamps at 0.
    pg.on_key(Key::Home, body_h, len);
    assert_eq!(pg.top, 0);
    pg.on_key(Key::Up, body_h, len);
    assert_eq!(pg.top, 0);
    // Page down advances by a page; Space is an alias.
    pg.on_key(Key::PageDown, body_h, len);
    assert_eq!(pg.top, body_h - 1);
    pg.on_key(Key::Char(' '), body_h, len);
    assert_eq!(pg.top, 2 * (body_h - 1));
    // 'q' quits; wheel scrolls.
    pg.on_key(Key::Char('q'), body_h, len);
    assert!(pg.quit);
}

#[test]
fn pager_frame_positions_a_diagram_at_its_row() {
    let doc = "para one\n\n```mermaid\nflowchart TD\n A-->B\n```\n";
    let preview = build_preview_with(doc, 40, plain_style(), DiagramPaint::Native);
    let mut pg = Pager::new("x.md");
    pg.paint = DiagramPaint::Native;
    let frame = pg.frame(&preview, (40, 24));
    // The diagram is emitted as an OSC 1338 confined to the full width.
    assert!(frame.contains("\x1b]1338;"), "native diagram placement emitted");
    assert!(frame.contains(";40\x07"), "confined to the pager width");
}

#[test]
fn pager_paints_diagram_art_off_our_terminal() {
    // Anywhere but our GUI the reserved rows carry the drawn picture, never a native
    // escape the host can't read and never blank space.
    let doc = "para one\n\n```mermaid\nflowchart TD\n A[Start]-->B[End]\n```\n";
    let preview = build_preview_with(doc, 40, plain_style(), DiagramPaint::Art);
    let mut pg = Pager::new("x.md");
    pg.paint = DiagramPaint::Art;
    let frame = pg.frame(&preview, (40, 24));
    assert!(!frame.contains("\x1b]1338;"), "no native placement off our terminal");
    assert!(frame.contains("Start") && frame.contains("End"), "the picture is painted: {frame:?}");
}

#[test]
fn preview_height_counts_text_and_diagram_rows() {
    let doc = "# Title\n\n```mermaid\nflowchart TD\n A-->B\n```\n";
    let h = preview_height(doc, 60, plain_style());
    assert!(h >= 5, "title + blank + several diagram rows: {h}");
}

fn plain_style() -> corelib::md::Style {
    corelib::md::Style { enabled: false, ..corelib::md::Style::default() }
}
