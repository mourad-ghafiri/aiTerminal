use super::*;

fn line_text(t: &Term, y: u16) -> String {
    t.row(y)
        .iter()
        .filter(|c| !c.is_wide_spacer())
        .map(|c| c.ch)
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn prints_and_wraps() {
    let mut t = Term::new(4, 3);
    t.feed(b"abcdef");
    assert_eq!(line_text(&t, 0), "abcd");
    assert_eq!(line_text(&t, 1), "ef");
}

#[test]
fn resize_leaves_scrollback_ragged_and_readers_stay_safe() {
    // The contract after the perf fix: a resize NEVER rewrites history (that made
    // a window drag O(scrollback × events)). Scrollback rows keep their captured
    // width; every reader clamps instead of indexing 0..cols.
    let mut t = Term::with_scrollback(5, 2, 50);
    for _ in 0..10 {
        t.feed(b"abcde\r\n"); // push rows into scrollback at width 5
    }
    assert!(t.scrollback_len() > 0);
    t.scroll_view(5); // scroll up so display_row returns scrollback rows
    t.resize(12, 2);
    // History keeps its width-5 rows; the content is intact and readable through
    // the clamping accessors (a renderer uses row.get(x), never row[x]).
    for g in 0..t.scrollback.len() {
        assert_eq!(t.scrollback[g].len(), 5, "history is not rewritten on resize");
    }
    for y in 0..t.rows() {
        let row = t.display_row(y);
        let text: String = row.iter().map(|c| c.ch).collect();
        assert!(text.trim_end() == "abcde" || text.trim_end().is_empty());
    }
    // Narrow/widen churn stays consistent (no panic, content preserved).
    t.resize(3, 2);
    t.resize(20, 2);
    assert!(t.scroll_offset() <= t.scrollback_len());
}

#[test]
fn resize_storm_is_cheap_with_deep_scrollback() {
    // A live window drag fires resize continuously; with 10k scrollback lines the
    // old per-event re-widening made that O(scrollback × events). 500 alternating
    // resizes must complete in far under the old cost.
    let mut t = Term::with_scrollback(80, 24, 10_000);
    for i in 0..10_000 {
        t.feed(format!("line {i}\r\n").as_bytes());
    }
    assert!(t.scrollback_len() >= 9_000);
    let start = std::time::Instant::now();
    for i in 0..500 {
        t.resize(if i % 2 == 0 { 79 } else { 121 }, 24);
    }
    assert!(start.elapsed() < std::time::Duration::from_millis(100), "took {:?}", start.elapsed());
    // History is still intact and clamped reads still work after the churn.
    t.scroll_view(50);
    let any: String = t.display_row(0).iter().map(|c| c.ch).collect();
    assert!(any.starts_with("line "));
}

#[test]
fn content_ansi_builds_only_the_requested_tail() {
    // 5000 numbered lines, cap 100: the dump must be exactly the LAST 100 lines
    // (same content the full build used to produce for that range).
    let mut t = Term::with_scrollback(40, 5, 10_000);
    for i in 0..5_000 {
        t.feed(format!("row-{i}\r\n").as_bytes());
    }
    let dump = t.content_ansi(100, None);
    assert_eq!(dump.len(), 100);
    assert!(dump[0].contains("row-4900"), "starts 100 from the end: {:?}", &dump[0]);
    assert!(dump[99].contains("row-4999"), "ends at the last content row: {:?}", &dump[99]);
    // A large cap on a small buffer returns everything, trailing blanks trimmed.
    let mut small = Term::new(20, 5);
    small.feed(b"only\r\n");
    let d = small.content_ansi(1000, None);
    assert_eq!(d.len(), 1, "trailing blank screen rows are dropped");
    assert!(d[0].contains("only"));
}

#[test]
fn scroll_recycle_keeps_scrollback_bounded_and_correct() {
    // Overflow the cap so scroll_up recycles the line dropped off the front. The
    // scrollback must stay capped, keep the most-recent rows, and every row stays
    // exactly `cols` wide (recycled buffers are cleared + resized, not reused dirty).
    let mut t = Term::with_scrollback(4, 2, 5); // cap 5 history lines
    let line_text = |row: &[Cell]| row.iter().map(|c| c.ch).collect::<String>();
    for i in 0..20 {
        // each row a distinct char so we can identify which survived eviction
        let ch = (b'a' + (i % 26)) as char;
        t.feed(format!("{ch}{ch}{ch}{ch}\r\n").as_bytes());
    }
    assert_eq!(t.scrollback_len(), 5, "scrollback stays capped despite recycling");
    for g in 0..t.scrollback_len() {
        assert_eq!(t.scrollback[g].len(), 4, "every recycled row is exactly cols wide");
        let txt = line_text(&t.scrollback[g]);
        assert!(txt.chars().all(|c| c == txt.chars().next().unwrap()), "no stale cells left in a recycled row: {txt:?}");
    }
    // The newest evicted row ('s' = index 18, since 19 'tttt' is on screen) is retained.
    assert_eq!(line_text(&t.scrollback[4]), "ssss");
}

#[test]
fn newline_and_carriage_return() {
    let mut t = Term::new(10, 3);
    t.feed(b"hi\r\nthere");
    assert_eq!(line_text(&t, 0), "hi");
    assert_eq!(line_text(&t, 1), "there");
}

#[test]
fn delete_line_at_row0_does_not_pollute_scrollback() {
    // DL (`ESC[M`) at the top row temporarily set scroll_top=0 and scroll_up'd, which
    // (with the old `top==0` capture) pushed DELETED lines into history. They must not.
    let mut t = Term::with_scrollback(20, 4, 100);
    t.feed(b"aaa\r\nbbb\r\nccc\r\nddd");
    t.feed(b"\x1b[H"); // cursor home (row 0)
    t.feed(b"\x1b[2M"); // delete 2 lines at row 0
    assert_eq!(t.scrollback_len(), 0, "deleted lines are gone, never history");
    assert_eq!(line_text(&t, 0), "ccc", "content below shifted up");
}

#[test]
fn ris_preserves_the_configured_scrollback_cap() {
    // RIS (`ESC c`) must not silently revert a custom scrollback cap to the 10 000 default.
    let mut t = Term::with_scrollback(20, 3, 250);
    t.feed(b"\x1bc");
    for i in 0..400 {
        t.feed(format!("row-{i}\r\n").as_bytes());
    }
    assert_eq!(t.scrollback_len(), 250, "the 250-line cap survived RIS");
}

#[test]
fn overwriting_a_wide_char_half_leaves_no_orphan() {
    // Writing a narrow char over one half of a CJK pair must clean up the partner cell,
    // not strand a hole (orphan spacer) or a doubled glyph (orphan lead).
    let mut t = Term::new(10, 1);
    t.feed("你好".as_bytes()); // two wide chars → cells 0-1 (你), 2-3 (好)
    t.feed(b"\x1b[1G"); // cursor to column 1 (col index 0)
    t.feed(b"x"); // overwrite the LEAD of 你
    let row = t.row(0);
    assert_eq!(row[0].ch, 'x');
    assert!(!row[1].is_wide_spacer(), "the orphaned spacer was cleared");
    // Now overwrite the SPACER half of 好 (col index 3).
    t.feed(b"\x1b[4G");
    t.feed(b"y");
    let row = t.row(0);
    assert_eq!(row[3].ch, 'y');
    assert_eq!(row[2].ch, ' ', "the orphaned lead was cleared");
}

#[test]
fn echo_ich_dch_edit_the_line_correctly() {
    // ECH blanks in place; ICH shifts right; DCH shifts left — the ncurses editing ops.
    let mut t = Term::new(10, 1);
    t.feed(b"abcdef");
    t.feed(b"\x1b[1G\x1b[2X"); // home, erase 2 chars → "  cdef"
    assert_eq!(line_text(&t, 0), "  cdef");
    t.feed(b"abcdef");
    t.feed(b"\x1b[1G\x1b[2@"); // home, insert 2 blanks at the front → "  abcdef"
    assert_eq!(line_text(&t, 0), "  abcdef");
    t.feed(b"\x1b[1G\x1b[2P"); // home, delete 2 → "abcdef"
    assert_eq!(line_text(&t, 0), "abcdef");
}

#[test]
fn cnl_cpl_move_to_column_zero() {
    // CNL (E) / CPL (F): down/up N rows AND to column 0.
    let mut t = Term::new(10, 4);
    t.feed(b"\x1b[2;5H"); // row 2, col 5
    t.feed(b"\x1b[1E"); // CNL 1 → row 3, col 0
    assert_eq!(t.cursor(), (0, 2));
    t.feed(b"\x1b[2;5H");
    t.feed(b"\x1b[1F"); // CPL 1 → row 1, col 0
    assert_eq!(t.cursor(), (0, 0));
}

#[test]
fn ed3_clears_scrollback_so_clear_truly_clears() {
    // The reported bug: `clear`, close, reopen → the old commands were back.
    // `clear` sends `ESC[2J` (screen) + `ESC[3J` (scrollback); ED 3 must purge the
    // deque, or the workspace save still dumps the history that `clear` "removed".
    let mut t = Term::with_scrollback(20, 3, 100);
    for i in 0..30 {
        t.feed(format!("cmd-{i}\r\n").as_bytes()); // overflow the screen → scrollback fills
    }
    assert!(t.scrollback_len() > 0, "precondition: history accumulated");
    t.feed(b"\x1b[H\x1b[2J\x1b[3J"); // exactly what `clear(1)` emits
    assert_eq!(t.scrollback_len(), 0, "ED 3 purges the scrollback deque");
    assert!(t.content_ansi(1000, None).is_empty(), "a cleared terminal saves nothing");
    // ED 2 alone (no 3) must NOT drop history — scrolling up still shows it live.
    for i in 0..30 {
        t.feed(format!("again-{i}\r\n").as_bytes());
    }
    let before = t.scrollback_len();
    t.feed(b"\x1b[2J");
    assert_eq!(t.scrollback_len(), before, "ED 2 leaves scrollback intact");
}

#[test]
fn content_ansi_drops_the_live_prompt_line() {
    // The reported bug: every close + reopen stacked one more "~ ❯" — the live
    // prompt row (where the cursor waits for input) was saved as content, then
    // the fresh shell printed its own prompt beneath it. The cursor row and
    // everything below it are live input, never history.
    let mut t = Term::new(20, 5);
    t.feed("echo hi\r\nhi\r\n~ \u{276F} ".as_bytes()); // finished output, then the prompt
    let dump = t.content_ansi(100, None);
    assert_eq!(dump.len(), 2, "the live prompt row is not saved: {dump:?}");
    assert!(dump[1].contains("hi"));
    // A typed-but-unsubmitted command sits on the cursor row too — also transient.
    t.feed(b"cargo tes");
    assert_eq!(t.content_ansi(100, None).len(), 2);
}

#[test]
fn osc_1338_records_a_diagram_placement_and_reserves_rows() {
    let mut t = Term::new(20, 10);
    t.feed(b"hi\r\n"); // cursor to row 1
    let start_cy = t.cursor().1;
    let src = "flowchart TD\n A --> B";
    let b64 = corelib::codec::base64_encode(src.as_bytes());
    t.feed(format!("\x1b]1338;4;{b64}\x07").as_bytes());
    let p = t.placements();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].rows, 4);
    assert_eq!(p[0].source, src);
    assert_eq!(p[0].g, 1, "anchored at the cursor's global line");
    assert!(t.cursor().1 >= start_cy + 4, "4 rows reserved below");
    // ED 3 (clear scrollback / `clear`) drops all placements.
    t.feed(b"\x1b[3J");
    assert!(t.placements().is_empty());
}

#[test]
fn resize_drops_diagram_placements() {
    let mut t = Term::new(30, 10);
    let b64 = corelib::codec::base64_encode(b"flowchart TD\n A-->B");
    t.feed(format!("\x1b]1338;3;{b64}\x07").as_bytes());
    assert_eq!(t.placements().len(), 1);
    t.resize(40, 12);
    assert!(t.placements().is_empty(), "reflow drops placements to avoid misalignment");
}

#[test]
fn osc_1338_in_alt_screen_positions_by_cell_and_confines_to_cols() {
    let mut t = Term::new(80, 24);
    t.feed(b"\x1b[?1049h"); // enter alt screen (the full-screen editor)
    t.feed(b"\x1b[3;41H"); // move cursor to row 3, col 41 (1-based) = preview column
    let src = "flowchart LR\n A-->B";
    let b64 = corelib::codec::base64_encode(src.as_bytes());
    t.feed(format!("\x1b]1338;5;{b64};38\x07").as_bytes()); // rows=5, cols=38
    assert!(t.placements().is_empty(), "alt screen never uses the primary placement list");
    let a = t.alt_placements();
    assert_eq!(a.len(), 1);
    assert_eq!((a[0].row, a[0].col), (2, 40), "0-based cursor cell");
    assert_eq!((a[0].rows, a[0].cols), (5, 38));
    assert_eq!(a[0].source, src);
    assert_eq!(t.cursor(), (40, 2), "no rows reserved — the app owns layout");
}

#[test]
fn ed2_in_alt_screen_clears_alt_placements_so_frames_dont_ghost() {
    let mut t = Term::new(40, 12);
    t.feed(b"\x1b[?1049h");
    let b64 = corelib::codec::base64_encode(b"flowchart TD\n A-->B");
    t.feed(format!("\x1b]1338;3;{b64}\x07").as_bytes());
    assert_eq!(t.alt_placements().len(), 1);
    t.feed(b"\x1b[2J"); // the editor clears before repainting each frame
    assert!(t.alt_placements().is_empty(), "a fresh frame starts with no diagrams");
    // Leaving the alt screen also drops them.
    t.feed(format!("\x1b]1338;3;{b64}\x07").as_bytes());
    t.feed(b"\x1b[?1049l");
    assert!(t.alt_placements().is_empty());
}

#[test]
fn mouse_modes_are_tracked_for_the_host_to_forward() {
    let mut t = Term::new(40, 12);
    assert!(!t.wants_mouse() && !t.mouse_sgr());
    t.feed(b"\x1b[?1000h\x1b[?1006h"); // click reporting + SGR encoding
    assert!(t.wants_mouse() && t.mouse_sgr());
    t.feed(b"\x1b[?1002h"); // upgrade to click+drag
    assert!(t.wants_mouse());
    t.feed(b"\x1b[?1002l"); // disable → off
    assert!(!t.wants_mouse());
    t.feed(b"\x1b[?1006l");
    assert!(!t.mouse_sgr());
}

#[test]
fn one_sequence_can_set_several_modes_at_once() {
    // `ESC[?1000;1002;1006h` is what ratatui and bubbletea actually send. Applying
    // only the first parameter drops the SGR half of the mouse handshake — reports
    // then arrive in the legacy encoding the host never asked for.
    let mut t = Term::new(40, 12);
    t.feed(b"\x1b[?1000;1002;1006h");
    assert!(t.wants_mouse(), "tracking enabled");
    assert!(t.mouse_sgr(), "the third parameter must not be dropped");

    t.feed(b"\x1b[?1002;1006l");
    assert!(!t.wants_mouse() && !t.mouse_sgr(), "and a combined reset clears them all");
}

#[test]
fn bracketed_paste_and_application_cursor_keys_are_tracked() {
    // Both tell a host HOW to talk to the program: wrap pastes, and send SS3 arrows.
    let mut t = Term::new(40, 12);
    assert!(!t.bracketed_paste() && !t.app_cursor_keys());
    t.feed(b"\x1b[?2004h\x1b[?1h");
    assert!(t.bracketed_paste() && t.app_cursor_keys());
    t.feed(b"\x1b[?2004l\x1b[?1l");
    assert!(!t.bracketed_paste() && !t.app_cursor_keys());
}

#[test]
fn every_mode_a_program_can_declare_is_reported_independently() {
    // The emulator reports facts and takes no view on what they mean — it has to be
    // that way, because a shell's line editor sets two of these at every prompt.
    let mut t = Term::new(40, 12);
    t.feed(b"\x1b[?1049h\x1b[?2004h\x1b[?1h\x1b[?1000;1006h");
    assert!(t.in_alt_screen() && t.bracketed_paste() && t.app_cursor_keys() && t.wants_mouse());
    t.feed(b"\x1b[?1000l\x1b[?1l\x1b[?2004l\x1b[?1049l");
    assert!(!t.in_alt_screen() && !t.bracketed_paste() && !t.app_cursor_keys() && !t.wants_mouse());
}

#[test]
fn screen_text_reads_the_visible_screen_including_the_alternate_one() {
    // `content_ansi` deliberately reads the PRIMARY buffer even while alt is live
    // (it serves session restore). Showing what a program displays needs the grid
    // actually in front of you.
    let mut t = Term::new(20, 5);
    t.feed(b"shell line\r\n");
    assert_eq!(t.screen_text(), vec!["shell line"]);

    t.feed(b"\x1b[?1049h\x1b[HTUI frame\r\nsecond row");
    assert_eq!(t.screen_text(), vec!["TUI frame", "second row"]);
    assert!(!t.content_ansi(20, None).iter().any(|l| l.contains("TUI")), "alt content is not history");

    t.feed(b"\x1b[?1049l");
    assert_eq!(t.screen_text(), vec!["shell line"], "the primary comes back intact");
}

#[test]
fn screen_text_drops_padding_without_eating_real_content() {
    let mut t = Term::new(24, 6);
    t.feed(b"first\r\n\r\nthird   \r\n");
    // Interior blank lines are content; only the trailing run of empties goes.
    assert_eq!(t.screen_text(), vec!["first", "", "third"]);
}

#[test]
fn screen_text_does_not_pad_wide_glyphs() {
    // A double-width glyph occupies two cells; the spacer carries a blank that would
    // otherwise appear after every CJK character.
    let mut t = Term::new(20, 3);
    t.feed("日本語 ok".as_bytes());
    assert_eq!(t.screen_text(), vec!["日本語 ok"]);
}

#[test]
fn content_ansi_scrubs_the_selection_band_background() {
    // The reported bug: a live shift-selection at save time was baked into the
    // restored content as an un-dismissable highlight. The host passes its
    // selection-band color; those backgrounds serialize as DEFAULT. Other
    // backgrounds (real program output) are preserved untouched.
    let mut t = Term::new(20, 3);
    t.feed(b"\x1b[48;2;80;83;88mselected\x1b[0m plain \x1b[48;2;200;0;0mred\x1b[0m\r\n");
    let scrubbed = t.content_ansi(10, Some((80, 83, 88))).join("\n");
    assert!(!scrubbed.contains("48;2;80;83;88"), "band scrubbed: {scrubbed:?}");
    assert!(scrubbed.contains("48;2;200;0;0"), "real bg colors survive: {scrubbed:?}");
    assert!(scrubbed.contains("selected"), "text itself survives");
    let kept = t.content_ansi(10, None).join("\n");
    assert!(kept.contains("48;2;80;83;88"), "no strip requested → band kept");
}

#[test]
fn osc_52_stages_clipboard_text_for_the_host() {
    let mut t = Term::new(10, 2);
    assert_eq!(t.take_clipboard(), None);
    t.feed(b"\x1b]52;c;aGVsbG8=\x07"); // base64("hello")
    assert_eq!(t.take_clipboard(), Some("hello".into()));
    assert_eq!(t.take_clipboard(), None, "drained once");
    t.feed(b"\x1b]52;c;?\x07"); // a query must never stage (or leak) anything
    assert_eq!(t.take_clipboard(), None);
    t.feed(b"\x1b]52;c;!!!not-base64\x07"); // garbage is ignored
    assert_eq!(t.take_clipboard(), None);
}

#[test]
fn fed_within_reflects_recent_input() {
    let mut t = Term::new(4, 2);
    assert!(!t.fed_within_ms(1000), "a fresh terminal has no feed");
    t.feed(b"x");
    assert!(t.fed_within_ms(60_000), "a just-fed terminal reports recent input");
}

#[test]
fn generation_bumps_on_feed_and_resize_only() {
    let mut t = Term::new(20, 3);
    let g0 = t.generation();
    t.feed(b"");
    assert_eq!(t.generation(), g0, "an empty feed is not a content change");
    t.feed(b"x");
    let g1 = t.generation();
    assert!(g1 > g0, "output bumps the generation");
    t.resize(30, 4);
    assert!(t.generation() > g1, "a resize is a visible change too");
    let g2 = t.generation();
    assert_eq!(t.generation(), g2, "reading never bumps it");
}

#[test]
fn osc_7_reports_remote_cwd_and_host() {
    let mut t = Term::new(20, 3);
    assert_eq!(t.cwd(), None);
    // OSC 7 ; file://prod/var/www ST → remote host + path (the SSH case)
    t.feed(b"\x1b]7;file://prod/var/www\x1b\\");
    assert_eq!(t.cwd(), Some(("prod", "/var/www")));
    let seq1 = t.cwd_seq();
    assert!(seq1 > 0);
    // Re-reporting the same dir does NOT bump the sequence.
    t.feed(b"\x1b]7;file://prod/var/www\x1b\\");
    assert_eq!(t.cwd_seq(), seq1);
    // A `cd` (new path) bumps it; `localhost` normalizes to a local (empty) host; %20 decodes.
    t.feed(b"\x1b]7;file://localhost/home/ada/my%20proj\x1b\\");
    assert_eq!(t.cwd(), Some(("", "/home/ada/my proj")));
    assert!(t.cwd_seq() > seq1);
    // iTerm-style OSC 1337 CurrentDir (path only → local).
    t.feed(b"\x1b]1337;CurrentDir=/tmp\x1b\\");
    assert_eq!(t.cwd(), Some(("", "/tmp")));
    // A non-file URL is ignored (cwd unchanged).
    let seq = t.cwd_seq();
    t.feed(b"\x1b]7;http://evil/x\x1b\\");
    assert_eq!(t.cwd_seq(), seq);
}

#[test]
fn cursor_position_and_overwrite() {
    let mut t = Term::new(10, 3);
    t.feed(b"\x1b[1;1Hxx\x1b[1;1HY");
    assert_eq!(line_text(&t, 0), "Yx");
    assert_eq!(t.cursor(), (1, 0));
}

#[test]
fn erase_display_clears() {
    let mut t = Term::new(5, 2);
    t.feed(b"hello\r\nworld");
    t.feed(b"\x1b[H\x1b[2J");
    assert_eq!(line_text(&t, 0), "");
    assert_eq!(line_text(&t, 1), "");
}

#[test]
fn sgr_sets_truecolor_fg() {
    let mut t = Term::new(4, 1);
    t.feed(b"\x1b[38;2;10;20;30mA");
    assert_eq!(t.row(0)[0].fg, Color::Rgb(10, 20, 30));
    assert_eq!(t.row(0)[0].ch, 'A');
}

#[test]
fn sgr_bold_then_reset() {
    let mut t = Term::new(4, 1);
    t.feed(b"\x1b[1mA\x1b[0mB");
    assert!(t.row(0)[0].flags.contains(CellFlags::BOLD));
    assert!(!t.row(0)[1].flags.contains(CellFlags::BOLD));
}

#[test]
fn wide_char_takes_two_columns() {
    let mut t = Term::new(6, 1);
    t.feed("世a".as_bytes());
    assert_eq!(t.row(0)[0].ch, '世');
    assert!(t.row(0)[1].is_wide_spacer());
    assert_eq!(t.row(0)[2].ch, 'a');
}

#[test]
fn scroll_pushes_to_scrollback() {
    let mut t = Term::new(4, 2);
    t.feed(b"a\r\nb\r\nc");
    // 3 logical lines in a 2-row screen → one line scrolled off
    assert_eq!(t.scrollback_len(), 1);
    assert_eq!(line_text(&t, 0), "b");
    assert_eq!(line_text(&t, 1), "c");
}

fn disp_text(t: &Term, y: u16) -> String {
    t.display_row(y).iter().filter(|c| !c.is_wide_spacer()).map(|c| c.ch).collect::<String>().trim_end().to_string()
}

#[test]
fn scroll_view_shows_scrollback_history() {
    let mut t = Term::new(4, 2);
    t.feed(b"1\r\n2\r\n3\r\n4\r\n5"); // scrollback [1,2,3], screen [4,5]
    assert_eq!(t.scrollback_len(), 3);
    assert!(t.at_bottom());
    assert_eq!(disp_text(&t, 0), "4");
    assert_eq!(disp_text(&t, 1), "5");
    // scroll up 2 → the viewport shows older history
    t.scroll_view(2);
    assert_eq!(t.scroll_offset(), 2);
    assert!(!t.at_bottom());
    assert_eq!(disp_text(&t, 0), "2");
    assert_eq!(disp_text(&t, 1), "3");
    // clamp + jump helpers
    t.scroll_view(99);
    assert_eq!(t.scroll_offset(), 3); // clamped to scrollback_len
    assert_eq!(disp_text(&t, 0), "1");
    t.scroll_to_bottom();
    assert!(t.at_bottom());
    assert_eq!(disp_text(&t, 0), "4");
    t.scroll_to_top();
    assert_eq!(disp_text(&t, 0), "1");
}

#[test]
fn scroll_stays_put_on_new_output() {
    let mut t = Term::new(4, 2);
    t.feed(b"1\r\n2\r\n3\r\n4\r\n5"); // scrollback [1,2,3]
    t.scroll_view(2);
    assert_eq!(disp_text(&t, 0), "2");
    // new output evicts a line to scrollback — the view stays locked on "2"
    t.feed(b"\r\n6");
    assert_eq!(t.scroll_offset(), 3, "offset tracked the evicted line");
    assert_eq!(disp_text(&t, 0), "2", "viewport stayed put on history");
}

#[test]
fn scroll_is_noop_on_alt_screen() {
    let mut t = Term::new(4, 2);
    t.feed(b"1\r\n2\r\n3");
    t.feed(b"\x1b[?1049h"); // enter alt → offset reset, scroll disabled
    assert_eq!(t.scroll_offset(), 0);
    t.scroll_view(5);
    assert_eq!(t.scroll_offset(), 0, "the alt screen keeps no scrollback");
}

#[test]
fn alt_screen_swaps_and_restores() {
    let mut t = Term::new(6, 2);
    t.feed(b"main");
    t.feed(b"\x1b[?1049h"); // enter alt
    assert!(t.in_alt_screen());
    assert_eq!(line_text(&t, 0), "");
    t.feed(b"alt");
    t.feed(b"\x1b[?1049l"); // leave alt
    assert!(!t.in_alt_screen());
    assert_eq!(line_text(&t, 0), "main");
}

#[test]
fn cursor_hide_show() {
    let mut t = Term::new(4, 1);
    t.feed(b"\x1b[?25l");
    assert!(!t.cursor_visible());
    t.feed(b"\x1b[?25h");
    assert!(t.cursor_visible());
}

#[test]
fn osc_sets_title() {
    let mut t = Term::new(4, 1);
    t.feed(b"\x1b]0;hello\x07");
    assert_eq!(t.title(), "hello");
}

#[test]
fn resize_height_shrink_scrolls_top_into_scrollback_not_the_bottom() {
    // Shrinking height must keep the cursor line + recent output on screen, pushing the
    // TOP rows into scrollback — the OLD code chopped the BOTTOM, deleting the cursor
    // line and latest output (what made a vertical split wreck its sibling).
    let mut t = Term::with_scrollback(20, 5, 100);
    t.feed(b"r0\r\nr1\r\nr2\r\nr3\r\nr4"); // cursor on "r4" (bottom row)
    assert_eq!(t.scrollback_len(), 0);
    t.resize(20, 3); // 5 → 3 rows
    // The bottom (recent) rows stay; the top scrolled into history.
    assert_eq!(line_text(&t, 0), "r2");
    assert_eq!(line_text(&t, 2), "r4", "the cursor line + newest output are kept");
    assert_eq!(t.scrollback_len(), 2, "the 2 top rows went to scrollback");
}

#[test]
fn resize_shrink_then_grow_round_trips_exactly() {
    // The split/close + window-resize guarantee, on a REALISTIC pane: a few lines of
    // output then a prompt, with blank space below (the normal shell state). Steal the
    // pane's space on both axes (a split appears) then give it back (it closes) →
    // byte-identical, and nothing moved into scrollback.
    let mut t = Term::with_scrollback(40, 12, 500);
    t.feed(b"line-0-aaaaaaaaaaaaaaaaaaaaaaaaaa\r\nline-1-bbbbbbbbbbbbbbbbbbbbbbbbbb\r\n~ > "); // prompt, blanks below
    let before: Vec<String> = (0..12).map(|y| line_text(&t, y)).collect();
    t.resize(18, 5); // a neighbouring split squeezes this pane on both axes…
    t.resize(40, 12); // …then the split closes.
    let after: Vec<String> = (0..12).map(|y| line_text(&t, y)).collect();
    assert_eq!(before, after, "shrink→grow restored every row exactly");
    assert_eq!(t.scrollback_len(), 0, "a pane with headroom never spills into scrollback on resize");
}

#[test]
fn resize_keeps_the_prompt_visible_not_buried_in_scrollback() {
    // The window-resize disorder: a fresh split pane has its prompt near the TOP with
    // blank space beneath. Shrinking must drop the trailing BLANK rows, keeping the
    // prompt on screen — the old code scrolled the top (the prompt!) into scrollback,
    // leaving a blank pane and a prompt that jumped around during a drag.
    let mut t = Term::with_scrollback(40, 20, 500);
    t.feed(b"~/project > "); // one prompt line at the top, 19 blank rows below
    for target in [14, 9, 5, 3, 18, 20] {
        t.resize(40, target); // simulate a resize drag oscillating the height
        assert_eq!(line_text(&t, 0), "~/project >", "the prompt stays on the top row at height {target}");
        assert_eq!(t.scrollback_len(), 0, "no history is fabricated by resizing an empty-ish pane");
    }
}

#[test]
fn resize_width_is_lossless_and_reversible() {
    // A width-shrink must NOT destroy content (the old clamp truncated to "hel", so a
    // split that squeezed a pane then closed left the pane clipped forever). The line
    // keeps its full text off-screen; the RENDERER clips to `cols`, and a widen reveals
    // it again intact.
    let mut t = Term::new(10, 2);
    t.feed(b"hello");
    t.resize(3, 2); // width 10 → 3, same rows (isolate the width axis)
    assert_eq!(t.cols(), 3);
    assert_eq!(line_text(&t, 0), "hello", "content survives the shrink (clipped only at render)");
    // The visible slice is clipped to cols — what the user actually sees.
    let visible: String = t.display_row(0).iter().take(t.cols() as usize).map(|c| c.ch).collect::<String>().trim_end().to_string();
    assert_eq!(visible, "hel", "the render clips to the narrow width");
    // Grow back → the full line is whole again. Split-then-close is a no-op.
    t.resize(10, 2);
    assert_eq!(line_text(&t, 0), "hello");
}

#[test]
fn restoring_at_the_saved_width_keeps_wide_lines_on_one_row() {
    // The restore-scramble bug: a line wider than the restore grid re-wraps. The fix
    // rebuilds the pane at the SAVED width, so a dump replays to identical rows.
    let mut t = Term::with_scrollback(100, 6, 500);
    let wide: String = (0..90).map(|i| char::from(b'a' + (i % 26) as u8)).collect(); // 90 cols > 80
    t.feed(format!("{wide}\r\nsecond line\r\n").as_bytes());
    let dump = t.content_ansi(1000, None);

    // Replaying at the SAVED width (100) — the 90-char line stays ONE physical row.
    let mut same = Term::with_scrollback(100, 6, 500);
    for l in &dump {
        same.feed(l.as_bytes());
        same.feed(b"\r\n");
    }
    assert_eq!(line_text(&same, 0), wide, "wide line intact on one row at the saved width");
    assert_eq!(line_text(&same, 1), "second line");

    // Replaying at the OLD fixed 80 width would have split the 90-char line across two
    // rows — proving why the pane must be rebuilt at its saved size.
    let mut narrow = Term::with_scrollback(80, 6, 500);
    for l in &dump {
        narrow.feed(l.as_bytes());
        narrow.feed(b"\r\n");
    }
    assert_ne!(line_text(&narrow, 1), "second line", "at 80 the wide line wraps and shoves everything down");
}

#[test]
fn content_ansi_round_trips_styles_through_a_fresh_term() {
    let mut t = Term::with_scrollback(20, 3, 100);
    // Colored + attributed content across scrollback and screen.
    t.feed(b"\x1b[31mred\x1b[0m plain\r\n\x1b[1;38;5;42mbold-green\x1b[0m\r\n\x1b[48;2;10;20;30mbgtc\x1b[0m tail\r\nlast\r\n");
    let dump = t.content_ansi(100, None);
    assert_eq!(dump.len(), 4);
    assert!(dump[0].contains("\x1b["), "styling survives the dump: {:?}", dump[0]);
    // Feed the dump into a FRESH term → the cells (glyphs + colors + attrs) match.
    let mut back = Term::with_scrollback(20, 3, 100);
    for line in &dump {
        back.feed(line.as_bytes());
        back.feed(b"\r\n");
    }
    back.feed(b"\x1b[A"); // cursor movement doesn't matter; compare content
    let orig: Vec<Vec<Cell>> = t.content_rows_for_test();
    let rest: Vec<Vec<Cell>> = back.content_rows_for_test();
    // Compare the meaningful prefix of each restored line.
    for (a, b) in orig.iter().zip(rest.iter()) {
        let w = a.iter().rposition(|c| c.ch != ' ' || c.fg != Color::Default || c.bg != Color::Default || c.flags.bits() != 0).map(|i| i + 1).unwrap_or(0);
        assert_eq!(&a[..w], &b[..w], "restored cells match the original");
    }
    // The alt screen never leaks into the dump — the primary does.
    t.feed(b"\x1b[?1049halt-screen-stuff");
    assert!(!t.content_ansi(100, None).iter().any(|l| l.contains("alt-screen")));
    // The cap keeps the LAST lines.
    assert_eq!(t.content_ansi(1, None).len(), 1);
}
