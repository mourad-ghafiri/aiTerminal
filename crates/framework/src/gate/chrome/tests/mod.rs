use super::*;

#[test]
fn restoration_always_undoes_mouse_paste_and_cursor_state() {
    // A program the gate relayed may have turned any of these on. Leaving one set
    // makes the user's pane behave strangely long after the gate is gone.
    let r = Chrome::restore_bytes(false);
    for seq in ["\x1b[?1000l", "\x1b[?1002l", "\x1b[?1003l", "\x1b[?1006l", "\x1b[?2004l", "\x1b[?25h"] {
        assert!(r.contains(seq), "restore is missing {seq:?}");
    }
}

#[test]
fn the_alternate_screen_is_popped_only_when_we_are_in_it() {
    // Popping a screen that was never pushed wipes the pane on some terminals.
    assert!(!Chrome::restore_bytes(false).contains(LEAVE_ALT));
    assert!(Chrome::restore_bytes(true).contains(LEAVE_ALT));
}

#[test]
fn every_gate_line_carries_a_carriage_return_for_raw_mode() {
    // Raw mode clears ONLCR: a bare "\n" drops a line without returning to column
    // one, so gate output would walk diagonally across the pane.
    let s = Style { accent: String::new(), muted: String::new(), reset: "" };
    for line in [
        s.inbound("Mourad", "cargo build"),
        s.outbound("sent 12 lines"),
        s.notice("blocked by guard"),
        s.banner("telegram", "@bot", Some("418-207")),
        s.farewell("stopped from another pane"),
    ] {
        for part in line.split('\n').filter(|p| !p.is_empty()) {
            assert!(part.ends_with('\r') || line.starts_with(part), "line not CR-terminated: {part:?}");
        }
    }
}

#[test]
fn the_banner_says_plainly_that_nothing_runs_before_pairing() {
    let s = Style { accent: String::new(), muted: String::new(), reset: "" };
    let b = s.banner("telegram", "@mourad_term_bot", Some("418-207"));
    assert!(b.contains("/pair 418-207"));
    assert!(b.contains("nothing runs until you do"));
    assert!(b.contains("@mourad_term_bot"));
}

#[test]
fn a_preauthorized_gate_does_not_advertise_a_code() {
    let s = Style { accent: String::new(), muted: String::new(), reset: "" };
    let b = s.banner("telegram", "@bot", None);
    assert!(!b.contains("/pair"));
    assert!(b.contains("no code needed"));
}
