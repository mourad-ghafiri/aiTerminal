use corelib::wire::Json;

use crate::caps::*;

/// `sys.run` output cap (per combined result) and wall-clock deadline: a chatty
/// or hung command is truncated / killed instead of flooding the transcript.
const SYS_RUN_CAP: usize = 256 * 1024;
const SYS_RUN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

// ----- sys.run (guard chokepoint) ------------------------------------------

pub(crate) fn sys(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    match method {
        "sys.run" => {
            let cmd = arg(args, 0, "cmd").ok_or("sys.run: missing cmd")?.trim();
            if cmd.is_empty() {
                return Err("sys.run: empty command".into());
            }
            // Reject an unterminated quote up front so we never hand a half-parsed line to
            // the shell (this is also what stops a broken command from hanging a shell).
            let _ = shell_split(cmd)?;
            // Guard the WHOLE command AND each pipeline/list segment (see `guard_shell`).
            guard_shell(cmd, ctx)?;
            // Run through /bin/sh so pipes, redirection (`>`), globs, and $VARs behave as a
            // user expects — in the workspace dir, with a PATH that includes the standard
            // locations (the GUI can launch us with a minimal PATH, which is why bare
            // `pwd`/`ls` used to fail). Bounded output + deadline are preserved.
            let mut c = std::process::Command::new("/bin/sh");
            c.arg("-c").arg(cmd);
            if let Some(dir) = ctx.sandbox.as_ref() {
                c.current_dir(dir);
            }
            c.env("PATH", ensured_path());
            let out = crate::procio::run_bounded(c, SYS_RUN_DEADLINE, SYS_RUN_CAP).map_err(|e| e.to_string())?;
            if out.timed_out {
                return Err(format!("sys.run: command timed out after {}s", SYS_RUN_DEADLINE.as_secs()));
            }
            let mut s = out.stdout;
            s.push_str(&out.stderr);
            if out.truncated {
                s.push_str("\n…[output truncated at 256 KiB]");
            }
            Ok(Json::Str(s))
        }
        _ => Err(format!("unknown sys method '{method}'")),
    }
}

/// Vet a shell command before `/bin/sh -c` runs it. A real shell can chain and pipe, so
/// we probe the WHOLE command string, then each segment (split on top-level `;`/`&&`/`||`/
/// `|`/`&`/newline) plus that segment's program basename, against the command guard. ANY
/// non-Allow blocks; a `Confirm` is a block on this non-interactive path (deny-wins). This
/// keeps the guard's intent (you cannot slip a denied program past it via a pipeline).
fn guard_shell(cmd: &str, ctx: &CapCtx) -> Result<(), String> {
    let mut probes: Vec<String> = vec![cmd.to_string()];
    for seg in split_shell_segments(cmd) {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        probes.push(seg.to_string());
        if let Ok(argv) = shell_split(seg) {
            if let Some(prog) = argv.first() {
                let base = std::path::Path::new(prog).file_name().and_then(|n| n.to_str()).unwrap_or(prog);
                probes.push(base.to_string());
            }
        }
    }
    for probe in probes {
        match ctx.policy.check_command(&probe) {
            crate::security::Verdict::Deny { reason } => return Err(format!("blocked by guard: {reason}")),
            crate::security::Verdict::Confirm { reason } => return Err(format!("requires confirmation (guard): {reason}")),
            crate::security::Verdict::Allow => {}
        }
    }
    Ok(())
}

/// Split a shell command into pipeline/list segments on top-level `;` `&&` `||` `|` `&`
/// and newlines, ignoring separators inside single/double quotes. Used only for GUARDING
/// (the shell itself does the real parsing) so each stage can be checked independently.
fn split_shell_segments(cmd: &str) -> Vec<String> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut segs = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '|' | '&' => {
                    segs.push(std::mem::take(&mut cur));
                    if i + 1 < chars.len() && chars[i + 1] == c {
                        i += 1; // consume the doubled operator (`&&` / `||`)
                    }
                }
                ';' | '\n' => segs.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            },
        }
        i += 1;
    }
    segs.push(cur);
    segs
}

/// A PATH that always includes the standard locations, whatever the app was launched with
/// (a GUI launch often has a minimal PATH). Existing entries keep priority; standard dirs
/// are appended; duplicates removed.
fn ensured_path() -> String {
    const STD: [&str; 6] = ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"];
    let existing = std::env::var("PATH").unwrap_or_default();
    let mut seen = std::collections::BTreeSet::new();
    let mut parts: Vec<String> = Vec::new();
    for p in existing.split(':').chain(STD) {
        if !p.is_empty() && seen.insert(p.to_string()) {
            parts.push(p.to_string());
        }
    }
    parts.join(":")
}

/// Split a command into argv, honoring single/double quotes (no other shell features — no
/// globs, pipes, `$()`, or redirection). An unterminated quote is an ERROR (rather than a
/// silently-mangled token), so the guard and the executor never disagree.
fn shell_split(cmd: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in cmd.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            None => {
                cur.push(c);
                any = true;
            }
        }
    }
    if quote.is_some() {
        return Err("sys.run: unterminated quote in command".into());
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}
