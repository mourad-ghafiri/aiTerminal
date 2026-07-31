use super::*;

fn policy_with(deny: &[&str], confirm: &[&str]) -> Arc<Policy> {
    let mut p = Policy::new();
    for d in deny {
        p.add_deny(d).unwrap();
    }
    for c in confirm {
        p.add_confirm(c).unwrap();
    }
    Arc::new(p)
}

fn gate_with(policy: Arc<Policy>, plain_runs: bool) -> Gate {
    let auth = Auth::new(true, Vec::new(), 0, "418207".into());
    Gate::new(auth, policy, Settings { plain_runs, max_reply_messages: 3, screenshot: FileKind::Document, cols: 80, attach: true })
}

fn paired() -> Gate {
    let mut g = gate_with(policy_with(&[], &[]), true);
    g.on_chat(7, "Mourad", "/pair 418-207", 0);
    g
}

/// Everything the gate would write to the shell.
fn pty_bytes(acts: &[Action]) -> Vec<u8> {
    acts.iter()
        .filter_map(|a| match a {
            Action::Pty(b) => Some(b.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn said(acts: &[Action]) -> String {
    acts.iter()
        .filter_map(|a| match a {
            Action::Say(s) => Some(s.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn an_unpaired_chat_gets_nothing_and_reaches_nothing() {
    let mut g = gate_with(policy_with(&[], &[]), true);
    let acts = g.on_chat(7, "Stranger", "rm -rf /", 0);
    assert!(acts.is_empty(), "no reply, no echo, and above all no shell write");
}

#[test]
fn pairing_welcomes_and_publishes_the_peer() {
    let mut g = gate_with(policy_with(&[], &[]), true);
    let acts = g.on_chat(7, "Mourad", "/pair 418-207", 0);
    assert!(acts.iter().any(|a| matches!(a, Action::Peer(p) if p.contains("Mourad"))));
    assert!(said(&acts).contains("paired"));
    assert!(said(&acts).contains("/shot"), "the welcome doubles as help");
    assert!(pty_bytes(&acts).is_empty());
}

#[test]
fn a_paired_chat_runs_a_plain_command() {
    let mut g = paired();
    let acts = g.on_chat(7, "Mourad", "git status", 100);
    assert_eq!(pty_bytes(&acts), b"git status\r");
    assert!(acts.iter().any(|a| matches!(a, Action::Local(l) if l.contains("git status"))), "echoed locally");
}

#[test]
fn a_denied_command_never_reaches_the_shell() {
    // The single most important test in this module.
    let mut g = gate_with(policy_with(&["^sudo\\b"], &[]), true);
    g.on_chat(7, "M", "/pair 418207", 0);
    let acts = g.on_chat(7, "M", "sudo rm -rf /", 1);
    assert!(pty_bytes(&acts).is_empty(), "a blocked command must not be written");
    assert!(said(&acts).contains("blocked"));
    assert!(acts.iter().any(|a| matches!(a, Action::Local(l) if l.contains("blocked"))), "and it is surfaced locally");
}

#[test]
fn a_confirm_command_waits_for_an_explicit_yes() {
    let mut g = gate_with(policy_with(&[], &["rm"]), true);
    g.on_chat(7, "M", "/pair 418207", 0);
    let acts = g.on_chat(7, "M", "rm -rf build", 1);
    assert!(pty_bytes(&acts).is_empty(), "nothing runs before confirmation");
    assert!(said(&acts).contains("/yes"));

    let acts = g.on_chat(7, "M", "/yes", 2);
    assert_eq!(pty_bytes(&acts), b"rm -rf build\r");
}

#[test]
fn a_confirmation_can_be_declined_and_expires() {
    let mut g = gate_with(policy_with(&[], &["rm"]), true);
    g.on_chat(7, "M", "/pair 418207", 0);
    g.on_chat(7, "M", "rm -rf build", 1);
    let acts = g.on_chat(7, "M", "/no", 2);
    assert!(pty_bytes(&acts).is_empty());
    assert!(said(&acts).contains("dropped"));

    g.on_chat(7, "M", "rm -rf build", 10);
    let acts = g.on_chat(7, "M", "/yes", 10 + CONFIRM_TTL_MS + 1);
    assert!(pty_bytes(&acts).is_empty(), "a stale yes must not fire");
    assert!(said(&acts).contains("expired"));
}

#[test]
fn an_unknown_slash_command_produces_help_and_no_shell_write() {
    let mut g = paired();
    let acts = g.on_chat(7, "M", "/rm -rf /", 1);
    assert!(pty_bytes(&acts).is_empty());
    assert!(said(&acts).contains("/shot"), "help was sent");
}

#[test]
fn plain_text_is_inert_when_configured_that_way() {
    let mut g = gate_with(policy_with(&[], &[]), false);
    g.on_chat(7, "M", "/pair 418207", 0);
    let acts = g.on_chat(7, "M", "rm -rf /", 1);
    assert!(pty_bytes(&acts).is_empty());
    assert!(said(&acts).contains("/run"));
}

#[test]
fn keys_and_named_keys_reach_the_shell_verbatim() {
    let mut g = paired();
    assert_eq!(pty_bytes(&g.on_chat(7, "M", "/keys hello", 1)), b"hello");
    assert_eq!(pty_bytes(&g.on_chat(7, "M", "/key enter", 2)), b"\r");
    assert_eq!(pty_bytes(&g.on_chat(7, "M", "/cancel", 3)), &[0x03]);
}

#[test]
fn an_unknown_key_name_is_refused_rather_than_typed() {
    let mut g = paired();
    let acts = g.on_chat(7, "M", "/key destroy-everything", 1);
    assert!(pty_bytes(&acts).is_empty(), "the name must never be typed as text");
    assert!(said(&acts).contains("unknown key"));
}


#[test]
fn text_becomes_stdin_while_a_command_is_waiting_for_input() {
    let mut g = paired();
    g.on_chat(7, "M", "sudo ls", 0);
    g.on_output(b"", &[Mark::Start], 1);
    g.on_output(b"Password:", &[], 2);
    g.tick(2 + 8_000); // the quiet note fires: it is waiting

    let acts = g.on_chat(7, "M", "hunter2", 20_000);
    assert_eq!(pty_bytes(&acts), b"hunter2\r");
    assert!(said(&acts).contains("running command"), "and it is not treated as a new command");
}

#[test]
fn a_finished_command_is_reported_with_its_output_and_status() {
    let mut g = paired();
    g.on_chat(7, "M", "ls", 0);
    g.on_output(b"", &[Mark::Start], 1);
    g.on_output(b"a.txt\r\nb.txt\r\n", &[], 2);
    let acts = g.on_output(b"", &[Mark::End(0)], 1_400);
    let text = said(&acts);
    assert!(text.contains("a.txt") && text.contains("b.txt"), "{text}");
    assert!(text.contains('✓'), "{text}");
}

#[test]
fn a_failing_command_reports_its_exit_code() {
    let mut g = paired();
    g.on_chat(7, "M", "false", 0);
    g.on_output(b"", &[Mark::Start], 1);
    assert!(said(&g.on_output(b"", &[Mark::End(1)], 2)).contains("✗ 1"));
}

#[test]
fn secrets_are_redacted_before_output_leaves_the_machine() {
    let mut p = Policy::new();
    p.add_redaction("AKIA[A-Z0-9]+", "«redacted»", RedactScope::Ai, false).unwrap();
    let mut g = gate_with(Arc::new(p), true);
    g.on_chat(7, "M", "/pair 418207", 0);
    g.on_chat(7, "M", "env", 1);
    g.on_output(b"", &[Mark::Start], 2);
    g.on_output(b"AWS_KEY=AKIA1234567890\r\n", &[], 3);
    let text = said(&g.on_output(b"", &[Mark::End(0)], 4));
    assert!(!text.contains("AKIA1234567890"), "a secret reached the chat: {text}");
    assert!(text.contains("redacted"), "{text}");
}


#[test]
fn full_resends_the_last_capture_as_a_file() {
    let mut g = paired();
    assert!(said(&g.on_chat(7, "M", "/full", 1)).contains("nothing captured"));
    g.on_chat(7, "M", "ls", 2);
    g.on_output(b"", &[Mark::Start], 3);
    g.on_output(b"a.txt\r\n", &[], 4);
    g.on_output(b"", &[Mark::End(0)], 5);
    match &g.on_chat(7, "M", "/full", 6)[0] {
        Action::File { text, name, .. } => {
            assert!(text.contains("a.txt"));
            assert_eq!(name, "output.txt");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn stop_from_the_chat_ends_the_gate() {
    let mut g = paired();
    let acts = g.on_chat(7, "M", "/stop", 1);
    assert!(acts.iter().any(|a| matches!(a, Action::Stop(_))));
}

#[test]
fn status_admits_when_completion_detection_is_only_approximate() {
    let mut g = paired();
    assert!(g.on_chat(7, "M", "/status", 1)[0] == Action::Say(g.status_html()));
    assert!(g.status_html().contains("approximate"), "a degraded session must say so");
    g.on_output(b"", &[Mark::Start], 2);
    g.on_output(b"", &[Mark::End(0)], 3);
    assert!(g.status_html().contains("exact"));
}

#[test]
fn the_ai_command_is_submitted_to_the_shell() {
    let mut g = paired();
    let acts = g.on_chat(7, "M", "/ai why did the build fail", 1);
    assert_eq!(pty_bytes(&acts), b"@ai why did the build fail\r");
}


#[test]
fn durations_read_naturally() {
    assert_eq!(human_ms(420), "420ms");
    assert_eq!(human_ms(1_400), "1.4s");
    assert_eq!(human_ms(95_000), "1m35s");
}

/// Put the gate in the state a program taking the terminal produces.
fn attach_app(g: &mut Gate) {
    g.observe(
        Mirror {
            signals: attach::Signals {
                bracketed: true,
                app_cursor: true,
                command_running: true,
                ..Default::default()
            },
            generation: 1,
            ..Default::default()
        },
        0,
    );
    assert!(g.attached());
}

#[test]
fn a_program_taking_the_terminal_attaches_and_announces_it() {
    let mut g = paired();
    let acts = g.observe(
        Mirror {
            signals: attach::Signals { alt: true, ..Default::default() },
            generation: 1,
            ..Default::default()
        },
        0,
    );
    assert!(said(&acts).contains("attached"), "{:?}", said(&acts));
    assert!(acts.iter().any(|a| matches!(a, Action::Local(_))), "and the pane says so too");
    assert!(g.take_frame(), "the first screen goes out immediately");
}

#[test]
fn while_attached_plain_text_is_typed_into_the_program_and_submitted() {
    let mut g = paired();
    attach_app(&mut g);
    // A single line is typed as-is; only a block that contains newlines needs the
    // paste wrapper (a bracketed single keystroke breaks vim and raw readers).
    let acts = g.on_chat(7, "M", "refactor the parser", 100);
    assert_eq!(pty_bytes(&acts), b"refactor the parser\r");

    let acts = g.on_chat(7, "M", "line one\nline two", 200);
    assert_eq!(pty_bytes(&acts), b"\x1b[200~line one\rline two\x1b[201~\r");
}

#[test]
fn while_attached_text_never_reaches_the_shell_machinery() {
    // The capture is for shell commands; feeding it app input would make the gate
    // think a command is running and start capturing repaint escapes.
    let mut g = paired();
    attach_app(&mut g);
    g.on_chat(7, "M", "some prompt", 100);
    assert!(g.capture().is_idle(), "no shell command was started");
}

#[test]
fn while_attached_an_explicit_run_is_refused_rather_than_queued() {
    // The old behaviour queued it and fired it AFTER the program exited — a command
    // running minutes later, unattended, that nobody was watching for.
    let mut g = paired();
    attach_app(&mut g);
    let acts = g.on_chat(7, "M", "/run rm -rf build", 100);
    assert!(pty_bytes(&acts).is_empty(), "nothing may reach the shell");
    assert!(said(&acts).contains("busy"), "{:?}", said(&acts));
    assert!(said(&acts).contains("/sh"), "and it says how to run it anyway");
    assert!(g.capture().is_idle(), "and nothing is waiting to fire later");
}

#[test]
fn keys_are_encoded_the_way_the_attached_program_asked() {
    let mut g = paired();
    attach_app(&mut g); // app_cursor = true
    assert_eq!(pty_bytes(&g.on_chat(7, "M", "/key up", 1)), b"\x1bOA");
    // Detached, the same key is the ordinary CSI form.
    let mut g2 = paired();
    assert_eq!(pty_bytes(&g2.on_chat(7, "M", "/key up", 1)), b"\x1b[A");
}

#[test]
fn the_live_screen_carries_the_programs_own_choices_as_buttons() {
    let mut g = paired();
    attach_app(&mut g);
    let screen: Vec<String> = ["Do you want to make this edit?", "❯ 1. Yes", "  2. No"]
        .iter().map(|s| s.to_string()).collect();
    match &g.frame(&screen)[0] {
        Action::Live { html, keys } => {
            assert!(html.contains("Do you want to make this edit?"));
            let data: Vec<&str> =
                keys.0.iter().flatten().map(|(_, d)| d.as_str()).collect();
            assert!(data.contains(&"k:1") && data.contains(&"k:2"), "{data:?}");
            // …and the keys you always want, whatever is on screen.
            assert!(data.contains(&"k:enter") && data.contains(&"k:ctrl-c") && data.contains(&"shot"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn the_live_screen_is_redacted_like_every_other_path_off_the_machine() {
    let mut p = Policy::new();
    p.add_redaction("AKIA[A-Z0-9]+", "«redacted»", RedactScope::Ai, false).unwrap();
    let mut g = gate_with(Arc::new(p), true);
    g.on_chat(7, "M", "/pair 418207", 0);
    attach_app(&mut g);
    match &g.frame(&["AWS_KEY=AKIA1234567890".to_string()])[0] {
        Action::Live { html, .. } => {
            assert!(!html.contains("AKIA1234567890"), "a secret reached the chat: {html}");
            assert!(html.contains("redacted"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_button_tap_is_acknowledged_and_acts_like_the_key_it_shows() {
    let mut g = paired();
    attach_app(&mut g);
    let acts = g.on_callback(7, "M", "cb1", "k:1", 100);
    assert!(acts.iter().any(|a| matches!(a, Action::Answer(id) if id == "cb1")), "the client must stop spinning");
    assert_eq!(pty_bytes(&acts), b"1");
}

#[test]
fn a_tap_from_an_unpaired_chat_is_acknowledged_but_does_nothing() {
    // Buttons live on a message anyone in the chat can see; a tap must re-enter the
    // same authorization as a typed message.
    let mut g = gate_with(policy_with(&[], &[]), true);
    let acts = g.on_callback(99, "Stranger", "cb9", "k:ctrl-c", 1);
    assert!(pty_bytes(&acts).is_empty(), "no key may reach the terminal");
    assert!(acts.iter().any(|a| matches!(a, Action::Answer(_))));
}

#[test]
fn an_attached_program_exiting_reports_its_status_not_its_repaint_soup() {
    let mut g = paired();
    g.on_chat(7, "M", "vim notes.md", 0);
    g.on_output(b"", &[Mark::Start], 1);
    attach_app(&mut g);
    g.on_output(b"\x1b[?1049h\x1b[2J\x1b[Hlots of repaint escapes", &[], 2);
    let acts = g.on_output(b"", &[Mark::End(0)], 3_000);
    let text = said(&acts);
    assert!(text.contains("exited"), "{text}");
    assert!(!text.contains("repaint escapes"), "the capture must not be dumped: {text}");
}

#[test]
fn help_explains_the_program_when_one_is_attached() {
    let mut g = paired();
    assert!(said(&g.on_chat(7, "M", "/help", 1)).contains("/run"), "the shell menu when detached");
    attach_app(&mut g);
    let h = said(&g.on_chat(7, "M", "/help", 2));
    assert!(h.contains("typed into it"), "{h}");
    assert!(h.contains("/keys"));
}

#[test]
fn attaching_can_be_turned_off_entirely() {
    let auth = Auth::new(true, Vec::new(), 0, "418207".into());
    let mut g = Gate::new(auth, policy_with(&[], &[]), Settings {
        plain_runs: true, max_reply_messages: 3, screenshot: FileKind::Document, cols: 80, attach: false,
    });
    g.on_chat(7, "M", "/pair 418207", 0);
    assert!(g
        .observe(
            Mirror {
                signals: attach::Signals { alt: true, ..Default::default() },
                generation: 1,
                ..Default::default()
            },
            0
        )
        .is_empty());
    assert!(!g.attached(), "[gates] attach = false keeps the old shell-only behaviour");
}

#[test]
fn a_command_arriving_mid_typing_is_queued_not_spliced() {
    // The corruption case: dispatching here would splice `ls` into `git comm`.
    let mut g = paired();
    g.on_local(b"git comm");
    let acts = g.on_chat(7, "M", "ls", 1);
    assert!(pty_bytes(&acts).is_empty(), "splicing would run a command neither party asked for");
    assert!(said(&acts).contains("queued"));
}
