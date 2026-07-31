use super::*;

#[test]
fn login_argv0_prefixes_basename() {
    assert_eq!(login_argv0("/bin/zsh"), "-zsh");
    assert_eq!(login_argv0("zsh"), "-zsh");
    assert_eq!(login_argv0("/usr/local/bin/fish"), "-fish");
}

#[test]
fn which_resolves_absolute_and_path() {
    assert_eq!(which("/bin/sh").as_deref(), Some("/bin/sh"));
    assert!(which("sh").is_some(), "sh should be on PATH");
    assert!(which("definitely-not-a-real-binary-xyz123").is_none());
    assert!(which("").is_none());
}

#[test]
fn resolve_shell_always_returns_an_executable() {
    let s = resolve_shell("");
    assert!(s.starts_with('/') && is_executable(&s), "default shell {s:?} not executable");
    // an explicit, valid shell wins
    assert_eq!(resolve_shell("/bin/sh"), "/bin/sh");
    // a bogus configured shell still yields a working fallback
    assert!(is_executable(&resolve_shell("/no/such/shell-xyz")));
}

#[test]
fn echo_through_pty_round_trips() {
    let cmd = PtyCommand {
        program: "/bin/echo".into(),
        args: vec!["pty-ok".into()],
        cols: 80,
        rows: 24,
        login: false,
        ..Default::default()
    };
    let pty = spawn(&cmd).expect("spawn echo");
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        match pty.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => panic!("read error: {e}"),
        }
        if out.len() > 4096 {
            break;
        }
    }
    let s = String::from_utf8_lossy(&out);
    assert!(s.contains("pty-ok"), "pty output was {s:?}");
}

#[test]
fn shell_dash_c_runs_and_writes_input() {
    let cmd = PtyCommand {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "printf 'A%sB' hello".into()],
        cols: 80,
        rows: 24,
        login: false,
        ..Default::default()
    };
    let pty = spawn(&cmd).expect("spawn sh");
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    while let Ok(n) = pty.read(&mut buf) {
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() > 4096 {
            break;
        }
    }
    assert!(String::from_utf8_lossy(&out).contains("AhelloB"));
}

#[test]
fn term_is_exported_to_the_child() {
    // Even with no inherited shell env (the GUI-launch case), the child must see
    // our TERM — proves build_envp + execve carry it through.
    let cmd = PtyCommand {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "printf 'T=%s' \"$TERM\"".into()],
        cols: 80,
        rows: 24,
        login: false,
        ..Default::default()
    };
    let pty = spawn(&cmd).expect("spawn sh");
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    while let Ok(n) = pty.read(&mut buf) {
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() > 4096 {
            break;
        }
    }
    assert!(
        String::from_utf8_lossy(&out).contains("T=xterm-256color"),
        "child TERM was {:?}",
        String::from_utf8_lossy(&out)
    );
}
