//! What a chat message means.
//!
//! One pure parser and one table, so the help text, the chat app's native slash menu,
//! and the dispatcher can never drift apart.
//!
//! The safety rule this module enforces: **an unrecognized slash command is help, never
//! a shell command.** Otherwise a typo'd `/remove-me` would run `remove-me`, and a chat
//! app's own commands (`/start`, `/settings`) would execute as shell input.

/// A parsed chat instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `/pair <code>` — the authorization handshake.
    Pair(String),
    /// Plain text the user sent. In the shell that means "run this"; while a program
    /// owns the terminal it means "type this into it". Kept separate from [`Run`] so
    /// those two can differ — an explicit `/run` is always a shell command, and while
    /// a program is attached that has to be refused rather than typed at it.
    Text(String),
    /// `/run <cmd>` — explicitly a shell command.
    Run(String),
    /// Run out-of-band, without touching the shared shell.
    Sh(String),
    /// Type literal text (no newline).
    Keys(String),
    /// Press a named key.
    Key(String),
    /// Ctrl-C the foreground program.
    Cancel,
    Shot,
    /// Resend the last capture as a file.
    Full,
    /// Ask the terminal's own AI.
    Ai(String),
    Status,
    /// Answer a pending guard confirmation.
    Yes,
    No,
    Help,
    Stop,
    /// Plain text, and `plain_text = "ignore"` is set.
    Ignored(String),
}

/// The command menu — help text, the chat app's slash menu, and the dispatcher all
/// read this one list.
pub const MENU: &[(&str, &str)] = &[
    ("/shot", "screenshot the terminal"),
    ("/run", "run a command in the shared shell"),
    ("/sh", "run a command out-of-band (works while a full-screen app is open)"),
    ("/key", "press a key: enter tab esc up down ctrl-c shift-tab f5, or any character"),
    ("/keys", "type text without pressing enter"),
    ("/cancel", "interrupt the running command (Ctrl-C)"),
    ("/ai", "ask this terminal's AI"),
    ("/full", "resend the last output as a file"),
    ("/status", "what the gate and the shell are doing"),
    ("/help", "this list"),
    ("/stop", "end the gate"),
];

/// Parse one message. `plain_runs` reflects `[gates] plain_text`.
pub fn parse(text: &str, plain_runs: bool) -> Command {
    let t = text.trim();
    if t.is_empty() {
        return Command::Help;
    }
    if !t.starts_with('/') {
        return if plain_runs { Command::Text(t.to_string()) } else { Command::Ignored(t.to_string()) };
    }

    // A chat app may append its bot handle: `/shot@mourad_term_bot`.
    let (head, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
    let verb = head.split('@').next().unwrap_or(head).to_ascii_lowercase();
    let arg = rest.trim().to_string();
    let nonempty = |c: fn(String) -> Command, arg: String| if arg.is_empty() { Command::Help } else { c(arg) };

    match verb.as_str() {
        "/pair" => nonempty(Command::Pair, arg),
        "/run" | "/r" => nonempty(Command::Run, arg),
        "/sh" => nonempty(Command::Sh, arg),
        // The argument is kept untrimmed (indentation matters when typing), but a
        // request that is ONLY whitespace is a slip, not an instruction.
        "/keys" | "/type" => {
            if arg.is_empty() {
                Command::Help
            } else {
                Command::Keys(rest.to_string())
            }
        }
        "/key" => nonempty(Command::Key, arg),
        "/cancel" | "/ctrlc" | "/c" => Command::Cancel,
        "/shot" | "/screen" | "/s" => Command::Shot,
        "/full" => Command::Full,
        "/ai" => nonempty(Command::Ai, arg),
        "/status" => Command::Status,
        "/yes" | "/y" => Command::Yes,
        "/no" | "/n" => Command::No,
        "/stop" | "/quit" => Command::Stop,
        // Everything else — including a chat app's own `/start` and any typo — is help.
        // This arm is the reason `/rm -rf /` can never reach a shell.
        _ => Command::Help,
    }
}

/// The `/help` reply, generated from [`MENU`] so it cannot go stale.
pub fn help_html(plain_runs: bool) -> String {
    let mut s = String::from("<b>aiTerminal gate</b>\n");
    s.push_str(if plain_runs {
        "Send a command and I'll run it in your terminal.\n\
         Start an interactive program (Claude Code, Codex, vim, a REPL) and I'll attach \
         to it — its screen appears here and your messages are typed into it.\n\n"
    } else {
        "Send <code>/run &lt;command&gt;</code> to run something.\n\n"
    });
    for (cmd, desc) in MENU {
        s.push_str(&format!("<code>{cmd}</code> — {}\n", super::reply::escape_html(desc)));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_runs_when_configured_to() {
        assert_eq!(parse("git status", true), Command::Text("git status".into()));
        assert_eq!(parse("  ls -la  ", true), Command::Text("ls -la".into()));
    }

    #[test]
    fn plain_text_is_inert_when_configured_to_be() {
        assert_eq!(parse("git status", false), Command::Ignored("git status".into()));
    }

    #[test]
    fn an_unknown_slash_command_can_never_reach_the_shell() {
        // The one that matters: a typo, or a chat app's own command, must not run.
        for text in ["/rm -rf /", "/start", "/settings", "/RUNN ls", "/", "/12345"] {
            assert_eq!(parse(text, true), Command::Help, "{text} must be inert");
        }
    }

    #[test]
    fn a_command_missing_its_argument_asks_for_help_instead_of_running_nothing() {
        for text in ["/run", "/run   ", "/sh", "/key", "/ai", "/pair", "/keys", "/keys    "] {
            assert_eq!(parse(text, true), Command::Help, "{text}");
        }
    }

    #[test]
    fn every_verb_parses() {
        assert_eq!(parse("/pair 418207", true), Command::Pair("418207".into()));
        assert_eq!(parse("/run echo hi", true), Command::Run("echo hi".into()));
        assert_eq!(parse("/sh git status", true), Command::Sh("git status".into()));
        assert_eq!(parse("/key ctrl-c", true), Command::Key("ctrl-c".into()));
        assert_eq!(parse("/cancel", true), Command::Cancel);
        assert_eq!(parse("/shot", true), Command::Shot);
        assert_eq!(parse("/full", true), Command::Full);
        assert_eq!(parse("/ai what is failing", true), Command::Ai("what is failing".into()));
        assert_eq!(parse("/status", true), Command::Status);
        assert_eq!(parse("/yes", true), Command::Yes);
        assert_eq!(parse("/no", true), Command::No);
        assert_eq!(parse("/help", true), Command::Help);
        assert_eq!(parse("/stop", true), Command::Stop);
    }

    #[test]
    fn a_bot_handle_suffix_is_tolerated() {
        // Group chats address commands as `/shot@my_bot`.
        assert_eq!(parse("/shot@mourad_term_bot", true), Command::Shot);
        assert_eq!(parse("/run@mourad_term_bot ls", true), Command::Run("ls".into()));
        assert_ne!(parse("ls", true), parse("/run ls", true), "plain text and an explicit /run are distinguishable");
    }

    #[test]
    fn typed_text_keeps_its_leading_spaces() {
        // `/keys` types literally — indentation inside a here-doc must survive.
        assert_eq!(parse("/keys    indented", true), Command::Keys("   indented".into()));
    }

    #[test]
    fn help_lists_every_menu_entry_and_escapes_it() {
        let h = help_html(true);
        for (cmd, _) in MENU {
            assert!(h.contains(cmd), "help is missing {cmd}");
        }
        assert!(!h.contains("<command>"), "descriptions must be HTML-escaped");
    }

    #[test]
    fn help_explains_the_configured_plain_text_behaviour() {
        assert!(help_html(true).contains("I'll run it"));
        assert!(help_html(false).contains("/run"));
    }
}
