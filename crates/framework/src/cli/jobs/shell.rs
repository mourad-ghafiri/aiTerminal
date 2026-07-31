
/// Why a job's command must not run, or `None` when it may.
///
/// A job has nobody to answer a prompt, so "ask first" is a refusal here — which is also the
/// check that matters most for a command the *model* proposed.
pub(crate) fn guard_refusal(policy: &crate::security::Policy, line: &str) -> Option<String> {
    match policy.check_command(line) {
        crate::security::Verdict::Allow => None,
        crate::security::Verdict::Deny { reason } => Some(format!("blocked by the command guard: {reason}")),
        crate::security::Verdict::Confirm { reason } => {
            Some(format!("needs confirmation ({reason}) \u{2014} a job can't ask, so it was not run"))
        }
    }
}

/// Run a job's command: guard-checked, in the job's folder, output streamed to its log
/// (and to this terminal when the job is in the foreground).
pub(crate) fn run_shell_job(cmd: &crate::jobs::Cmd, cwd: &str, log: Option<std::fs::File>, foreground: bool) -> i32 {
    use std::io::Write;
    let line = cmd.display();
    let cfg = crate::config::Config::load();
    let registry = crate::plugin::load_registry(&cfg);
    let policy = crate::security::build_policy(&cfg, &registry);
    let refusal = guard_refusal(&policy, &line);
    let mut sink = Sink { log, echo: foreground, written: 0, cap: cfg.jobs_max_log_bytes };
    if let Some(reason) = refusal {
        sink.write_line(&format!("aiTerminal: {reason}"));
        // The sink already echoed it when this job is in the foreground; a detached one has
        // only its log, so say it on stderr too.
        if !foreground {
            eprintln!("aiTerminal: {reason}");
        }
        return 2;
    }
    sink.write_line(&format!("$ {line}"));

    let mut command = match cmd {
        crate::jobs::Cmd::Line(l) => {
            let mut c = std::process::Command::new("/bin/sh");
            c.arg("-c").arg(l);
            c
        }
        crate::jobs::Cmd::Argv(argv) => {
            let mut c = std::process::Command::new(&argv[0]);
            c.args(&argv[1..]);
            c
        }
    };
    if !cwd.is_empty() && std::path::Path::new(cwd).is_dir() {
        command.current_dir(cwd);
    }
    let mut child = match command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("aiTerminal: {line}: {e}");
            sink.write_line(&msg);
            eprintln!("{msg}");
            return 127;
        }
    };
    // Drain both pipes on threads so a chatty command can't dead-lock on a full pipe.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    for stream in [child.stdout.take().map(Pipe::Out), child.stderr.take().map(Pipe::Err)].into_iter().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 8192];
            let mut reader: Box<dyn Read + Send> = match stream {
                Pipe::Out(o) => Box::new(o),
                Pipe::Err(e) => Box::new(e),
            };
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(String::from_utf8_lossy(&buf[..n]).into_owned()).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    for chunk in rx {
        sink.write(&chunk);
    }
    let status = child.wait();
    let code = match status {
        Ok(st) => st.code().unwrap_or(130),
        Err(e) => {
            sink.write_line(&format!("aiTerminal: {e}"));
            1
        }
    };
    sink.write_line(&format!("\n[exit {code}]"));
    let _ = std::io::stdout().flush();
    code
}

/// Which pipe a drained chunk came from (both go to the same place).
enum Pipe {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

/// Where a shell job's output goes: the run log (size-capped) and, in the foreground, the
/// terminal. A job that prints forever costs a bounded log instead of the disk.
struct Sink {
    log: Option<std::fs::File>,
    echo: bool,
    written: u64,
    cap: u64,
}

impl Sink {
    fn write(&mut self, text: &str) {
        use std::io::Write;
        if self.echo {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        let Some(log) = self.log.as_mut() else { return };
        if self.written >= self.cap {
            return;
        }
        let room = (self.cap - self.written) as usize;
        let slice = if text.len() > room { &text[..text.floor_char_boundary(room)] } else { text };
        if log.write_all(slice.as_bytes()).is_ok() {
            self.written += slice.len() as u64;
            if self.written >= self.cap {
                let _ = log.write_all(b"\n[log truncated]\n");
            }
        }
    }

    fn write_line(&mut self, text: &str) {
        self.write(text);
        self.write("\n");
    }
}
