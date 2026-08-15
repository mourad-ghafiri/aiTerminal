//! The typed surface: what a line of workspace input IS, decided before anything
//! can reach a model.
//!
//! One router, one fixed order, stated in `/help` — the `@flow` parser's
//! refuse-don't-guess spirit applied to a conversation:
//!
//! 1. `/word …`  — a slash command (built-ins + every prompt in the overlay);
//! 2. `!cmd`     — one shell command, judged by the guard like any other;
//! 3. `@word …`  — the product's own language: the reserved verbs, then an
//!    installed agent, then an existing path (an attachment), else literal text;
//! 4. anything else — a conversation turn.
//!
//! The slash commands are a Command-pattern registry: `/help` renders the same
//! list the parser matches, so the surface and its help cannot drift.

/// One slash command, as the registry and `/help` both see it.
pub(crate) struct SlashCommand {
    pub name: &'static str,
    pub about: &'static str,
}

/// The built-in surface. Custom prompt commands ride beside these at parse time.
pub(crate) const BUILTINS: &[SlashCommand] = &[
    SlashCommand { name: "/help", about: "this list, the @ verbs, and the input rules" },
    SlashCommand { name: "/init", about: "scan the project and write its aiTerminal.md" },
    SlashCommand { name: "/clear", about: "start a fresh conversation" },
    SlashCommand { name: "/compact", about: "fold the conversation's history down now" },
    SlashCommand { name: "/model", about: "the pool and its strategy; /model <id> pins one for this sitting" },
    SlashCommand { name: "/agent", about: "pin an agent persona for plain turns; /agent - unpins" },
    SlashCommand { name: "/agents", about: "the installed agents (project overlay first)" },
    SlashCommand { name: "/mcp", about: "the declared MCP servers, connected" },
    SlashCommand { name: "/memory", about: "what this folder remembers; /memory <note> adds" },
    SlashCommand { name: "/cost", about: "this sitting's tokens and cost" },
    SlashCommand { name: "/readonly", about: "toggle read-only tools (plan mode)" },
    SlashCommand { name: "/status", about: "the sitting on one card: model, usage, overlay, conversation" },
    SlashCommand { name: "/retry", about: "run your last prompt again as a fresh turn" },
    SlashCommand { name: "/save", about: "write the last answer to a file (guarded); /save <path> names it" },
    SlashCommand { name: "/files", about: "list project files; /files <glob> filters" },
    SlashCommand { name: "/skills", about: "the skills the overlay serves, project-first" },
    SlashCommand { name: "/keys", about: "the key table" },
    SlashCommand { name: "/trust", about: "re-open the project trust question" },
    SlashCommand { name: "/sessions", about: "this folder's conversations; /resume <n> folds one in" },
    SlashCommand { name: "/resume", about: "reload a conversation (latest, or /resume <n>)" },
    SlashCommand { name: "/undo", about: "take back the last exchange" },
    SlashCommand { name: "/redo", about: "restore what /undo took back" },
    SlashCommand { name: "/export", about: "write the whole conversation to a file (guarded)" },
    SlashCommand { name: "/thinking", about: "toggle showing the model's reasoning" },
    SlashCommand { name: "/exit", about: "leave workspace mode" },
];

/// What one line of input asks for.
#[derive(Debug, PartialEq)]
pub(crate) enum Route {
    Help,
    Init,
    Clear,
    Compact,
    Model(Option<String>),
    /// Pin (`Some`) or unpin (`None`) a persona.
    Agent(Option<String>),
    Agents,
    Mcp,
    /// Show memory (`None`) or add a note.
    Memory(Option<String>),
    Cost,
    StatusCard,
    Retry,
    Save(Option<String>),
    Files(Option<String>),
    Skills,
    Keys,
    Readonly,
    Trust,
    Sessions,
    /// Reload a conversation: the latest (`None`) or a numbered one.
    Resume(Option<usize>),
    Undo,
    Redo,
    Export(Option<String>),
    Thinking,
    Exit,
    /// A custom prompt command: its body, with the rest of the line spliced in.
    Prompt(String),
    /// `!cmd` — one guarded shell command.
    Bang(String),
    /// `@flow …` / `@job …` / `@loop …` / `@agent …` — the real commands, inline.
    Command(Vec<String>),
    /// `@<name> task` for an installed agent.
    AgentRun { name: String, task: String },
    /// A conversation turn (attachments still resolved downstream).
    Turn(String),
    /// A `/word` nothing matches — refused with the list, never sent to a model.
    Unknown(String),
}

/// Split off the first whitespace-delimited token.
fn token(line: &str) -> (&str, &str) {
    let line = line.trim();
    match line.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (line, ""),
    }
}

/// Route one line. `prompts` are the overlay's custom commands (name → body);
/// `agents` the installed agent names, project overlay included.
pub(crate) fn route(line: &str, prompts: &[crate::ai::defs::Prompt], agents: &[String]) -> Route {
    let trimmed = line.trim();
    if let Some(cmd) = trimmed.strip_prefix('!') {
        return Route::Bang(cmd.trim().to_string());
    }
    if trimmed.starts_with('/') {
        let (head, rest) = token(trimmed);
        return match head {
            "/help" => Route::Help,
            "/init" => Route::Init,
            "/clear" | "/new" => Route::Clear,
            "/compact" => Route::Compact,
            "/model" => Route::Model((!rest.is_empty()).then(|| rest.to_string())),
            "/agent" => Route::Agent((!rest.is_empty() && rest != "-").then(|| rest.to_string())),
            "/agents" => Route::Agents,
            "/mcp" => Route::Mcp,
            "/memory" => Route::Memory((!rest.is_empty()).then(|| rest.to_string())),
            "/cost" | "/usage" => Route::Cost,
            "/status" => Route::StatusCard,
            "/retry" => Route::Retry,
            "/save" => Route::Save((!rest.is_empty()).then(|| rest.to_string())),
            "/files" => Route::Files((!rest.is_empty()).then(|| rest.to_string())),
            "/skills" => Route::Skills,
            "/keys" => Route::Keys,
            "/readonly" | "/plan" => Route::Readonly,
            "/trust" => Route::Trust,
            "/sessions" => Route::Sessions,
            "/resume" | "/continue" => Route::Resume(rest.parse().ok()),
            "/undo" => Route::Undo,
            "/redo" => Route::Redo,
            "/export" => Route::Export((!rest.is_empty()).then(|| rest.to_string())),
            "/thinking" => Route::Thinking,
            "/exit" | "/quit" => Route::Exit,
            other => match prompts.iter().find(|p| format!("/{}", p.name) == other) {
                Some(p) => Route::Prompt(splice(&p.body, rest)),
                None => Route::Unknown(other.to_string()),
            },
        };
    }
    if let Some(rest) = trimmed.strip_prefix('@') {
        let (word, _) = token(rest);
        // The fixed order: reserved verbs, installed agents, then the attachment
        // machinery (which itself only treats EXISTING paths as files).
        match word {
            "flow" | "job" | "loop" | "agent" | "mcp" => {
                let argv: Vec<String> = trimmed[1..].split_whitespace().map(str::to_string).collect();
                return Route::Command(argv);
            }
            name if agents.iter().any(|a| a == name) => {
                let (_, task) = token(rest);
                return Route::AgentRun { name: name.to_string(), task: task.to_string() };
            }
            _ => {} // `@path/to/file` and friends stay in the turn for the attachment pass
        }
    }
    Route::Turn(trimmed.to_string())
}

/// A prompt command's body with the line's remainder spliced in: `{{input}}` where
/// the file asks for it, appended otherwise.
fn splice(body: &str, input: &str) -> String {
    match body.contains("{{input}}") {
        true => body.replace("{{input}}", input),
        false if input.is_empty() => body.to_string(),
        false => format!("{body}\n\n{input}"),
    }
}

/// Everything Tab can complete: the slash surface (built-ins + prompts) and the
/// `@` vocabulary (verbs + agents).
pub(crate) fn completions(prompts: &[crate::ai::defs::Prompt], agents: &[String]) -> Vec<String> {
    let mut out: Vec<String> = BUILTINS.iter().map(|c| c.name.to_string()).collect();
    out.extend(prompts.iter().map(|p| format!("/{}", p.name)));
    for verb in ["@flow", "@job", "@loop", "@agent", "@mcp"] {
        out.push(verb.to_string());
    }
    out.extend(agents.iter().map(|a| format!("@{a}")));
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests;
