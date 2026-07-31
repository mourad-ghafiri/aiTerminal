use super::*;

fn cap() -> Capture {
    Capture::new()
}

#[test]
fn a_marked_command_replies_with_its_output_and_exit_status() {
    let mut c = cap();
    assert_eq!(c.submit("ls".into(), false, 0), Submit::Running);
    assert_eq!(c.drain(), vec![Event::Dispatch("ls".into())]);
    c.on_output(b"", &[Mark::Start], false, 10);
    c.on_output(b"a.txt\r\n", &[], false, 20);
    c.on_output(b"", &[Mark::End(0)], false, 30);
    match &c.drain()[..] {
        [Event::Finished { cmd, status, bytes, elapsed_ms, .. }] => {
            assert_eq!(cmd, "ls");
            assert_eq!(*status, Some(0));
            assert_eq!(bytes, b"a.txt\r\n");
            assert_eq!(*elapsed_ms, 30);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(c.is_idle());
}

#[test]
fn output_printed_before_the_start_mark_is_not_captured() {
    // Leftovers from the previous prompt must not be attributed to our command.
    let mut c = cap();
    c.submit("ls".into(), false, 0);
    c.drain();
    c.on_output(b"stale banner\r\n", &[], false, 5);
    c.on_output(b"", &[Mark::Start], false, 10);
    c.on_output(b"real\r\n", &[], false, 15);
    c.on_output(b"", &[Mark::End(0)], false, 20);
    let Event::Finished { bytes, .. } = &c.drain()[0] else { panic!() };
    assert_eq!(bytes, b"real\r\n");
}

#[test]
fn a_local_command_is_recognized_and_never_reported_to_the_chat() {
    let mut c = cap();
    c.on_output(b"", &[Mark::Start], false, 0); // the human ran something
    c.on_output(b"their output\r\n", &[], false, 5);
    c.on_output(b"", &[Mark::End(0)], false, 10);
    assert!(c.drain().is_empty(), "the chat must not receive the local user's output");
    assert!(c.is_idle());
}

#[test]
fn a_remote_command_waits_while_the_local_user_is_typing() {
    // The corruption case: dispatching here would splice `ls` into `git comm`.
    let mut c = cap();
    c.on_local(b"git comm");
    assert_eq!(c.submit("ls".into(), false, 0), Submit::Queued(1));
    assert!(c.drain().is_empty(), "nothing may reach the PTY yet");

    c.on_local(b"it -m x\r"); // the human submits their own line
    c.tick(false, 10);
    assert_eq!(c.drain(), vec![Event::Dispatch("ls".into())], "queued command runs once the line is clear");
}

#[test]
fn after_enter_the_shell_is_treated_as_busy_until_the_prompt_returns() {
    let mut c = cap();
    c.on_output(b"", &[Mark::Start], false, 0); // teach it that marks work
    c.on_output(b"", &[Mark::End(0)], false, 1);
    c.on_local(b"sleep 30\r");
    assert_eq!(c.submit("ls".into(), false, 5), Submit::Queued(1));
    c.on_output(b"", &[Mark::Start], false, 6);
    c.tick(false, 10);
    assert!(c.drain().is_empty(), "still the local user's turn");
    c.on_output(b"", &[Mark::End(0)], false, 20);
    assert_eq!(c.drain(), vec![Event::Dispatch("ls".into())]);
}

#[test]
fn without_marks_a_local_enter_does_not_block_the_gate_forever() {
    // A shell with no integration never reports, so the optimistic "busy after
    // Enter" latch must not engage — otherwise the gate dies at first local use.
    let mut c = cap();
    c.on_local(b"echo hi\r");
    assert_eq!(c.submit("ls".into(), false, 0), Submit::Running);
}

#[test]
fn a_command_is_never_dispatched_into_a_full_screen_program() {
    let mut c = cap();
    assert_eq!(c.submit("ls".into(), true, 0), Submit::Queued(1), "vim is up; typing a command would go to vim");
    c.tick(true, 100);
    assert!(c.drain().is_empty());
    c.tick(false, 200); // the user quit vim
    assert_eq!(c.drain(), vec![Event::Dispatch("ls".into())]);
}

#[test]
fn the_queue_is_bounded() {
    let mut c = cap();
    c.on_local(b"x");
    for i in 1..=QUEUE_CAP {
        assert_eq!(c.submit(format!("c{i}"), false, 0), Submit::Queued(i));
    }
    assert_eq!(c.submit("one too many".into(), false, 0), Submit::Full);
}

#[test]
fn a_shell_without_marks_falls_back_to_silence_detection() {
    let mut c = cap();
    c.submit("ls".into(), false, 0);
    c.drain();
    c.tick(false, MARK_GRACE_MS); // grace expires, no Start ever came
    c.on_output(b"a.txt\r\n", &[], false, MARK_GRACE_MS + 10);
    c.tick(false, MARK_GRACE_MS + 100);
    assert!(c.drain().is_empty(), "still producing output");
    c.tick(false, MARK_GRACE_MS + 10 + DEBOUNCE_QUIET_MS);
    match &c.drain()[..] {
        [Event::Finished { status, bytes, .. }] => {
            assert_eq!(*status, None, "an inferred end has no trustworthy exit status");
            assert_eq!(bytes, b"a.txt\r\n");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_silent_marked_command_prompts_once_then_stays_running() {
    // `sudo` printing `Password:` and waiting: ship what we have, once.
    let mut c = cap();
    c.submit("sudo ls".into(), false, 0);
    c.drain();
    c.on_output(b"", &[Mark::Start], false, 1);
    c.on_output(b"Password:", &[], false, 2);
    c.tick(false, 2 + QUIET_NOTE_MS);
    match &c.drain()[..] {
        [Event::Progress { kind, bytes, .. }] => {
            assert_eq!(*kind, Progress::AwaitingInput);
            assert_eq!(bytes, b"Password:");
        }
        other => panic!("unexpected {other:?}"),
    }
    c.tick(false, 2 + QUIET_NOTE_MS * 3);
    assert!(c.drain().is_empty(), "the note must not repeat");
    assert!(!c.is_idle(), "the command is still running");
}

#[test]
fn a_long_build_gets_progress_notes_and_still_reports_its_real_status() {
    let mut c = cap();
    c.submit("cargo build".into(), false, 0);
    c.drain();
    c.on_output(b"", &[Mark::Start], false, 0);
    let mut t = 0;
    let mut notes = 0;
    // Ten minutes of a compiler chattering away.
    while t < 600_000 {
        t += 30_000;
        c.on_output(b"   Compiling something\r\n", &[], false, t);
        c.tick(false, t);
        notes += c.drain().iter().filter(|e| matches!(e, Event::Progress { .. })).count();
    }
    assert!(notes >= 2, "expected periodic progress, got {notes}");
    c.on_output(b"", &[Mark::End(101)], false, t);
    match &c.drain()[..] {
        [Event::Finished { status, .. }] => assert_eq!(*status, Some(101), "never abandoned mid-run"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn huge_output_is_bounded_keeping_the_head_and_the_tail() {
    let mut c = cap();
    c.submit("dump".into(), false, 0);
    c.drain();
    c.on_output(b"", &[Mark::Start], false, 0);
    c.on_output(b"FIRST\r\n", &[], false, 1);
    for _ in 0..1024 {
        c.on_output(&vec![b'x'; 1024], &[], false, 2); // 1 MiB
    }
    c.on_output(b"LAST\r\n", &[], false, 3);
    c.on_output(b"", &[Mark::End(0)], false, 4);
    let Event::Finished { bytes, elided, .. } = &c.drain()[0] else { panic!() };
    assert!(*elided);
    assert!(bytes.len() <= HEAD_CAP + TAIL_CAP + 64, "buffer grew to {}", bytes.len());
    assert!(bytes.starts_with(b"FIRST"), "the invocation's first output is kept");
    assert!(bytes.ends_with(b"LAST\r\n"), "the tail — where errors live — is kept");
    assert!(String::from_utf8_lossy(bytes).contains("elided"), "the gap is disclosed");
}

#[test]
fn entering_a_full_screen_program_is_recorded_on_the_capture() {
    let mut c = cap();
    c.submit("vim x".into(), false, 0);
    c.drain();
    c.on_output(b"", &[Mark::Start], false, 1);
    c.on_output(b"\x1b[?1049h", &[], true, 2);
    c.on_output(b"", &[Mark::End(0)], false, 3);
    let Event::Finished { saw_alt, .. } = &c.drain()[0] else { panic!() };
    assert!(*saw_alt, "the driver answers with a screenshot instead of empty text");
}

#[test]
fn a_stray_end_mark_before_anything_ran_is_ignored() {
    // `precmd` fires for the very first prompt, with no command before it.
    let mut c = cap();
    c.on_output(b"", &[Mark::End(0)], false, 0);
    assert!(c.drain().is_empty());
    assert!(c.is_idle());
    assert_eq!(c.submit("ls".into(), false, 1), Submit::Running);
}
