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
            crate::guard::split(cmd).map_err(|e| format!("sys.run: {e}"))?;
            // ONE question. The guard knows how to read a command line — every line, every
            // pipeline segment, each segment's program, and the paths it names — so there is
            // nothing here to keep in step with it.
            ctx.allow(crate::guard::Act::Run(cmd))?;
            // The secrets go back in LAST, immediately before the shell sees the line, so a
            // real value exists only for the length of this call: never in the transcript
            // the model reads back, never in a log, never in the guard's own refusal text.
            // And the line they make is judged again — a value that carries a `;` is a
            // second command the judgement above never saw.
            let cmd = ctx.guard.ready_command(cmd).map_err(|e| format!("sys.run: {e}"))?;
            // Run through /bin/sh so pipes, redirection (`>`), globs, and $VARs behave as a
            // user expects — in the workspace dir, with a PATH that includes the standard
            // locations (the GUI can launch us with a minimal PATH, which is why bare
            // `pwd`/`ls` used to fail). Bounded output + deadline are preserved.
            let mut c = std::process::Command::new("/bin/sh");
            c.arg("-c").arg(&cmd);
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
