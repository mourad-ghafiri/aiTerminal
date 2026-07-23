# Security

The model: **the AI proposes, the guard disposes** — every path from a model to your
system re-enters the same policy, and every path from your system to a model is
redacted.

## The command guard

A three-tier regex policy (in-house `re` engine), compiled from config +
plugin rules. **Deny > confirm > allow**; an empty allow-list means "everything not
denied".

- `@ai` suggestions are collected fully and guard-checked **before** the shell sees
  them: allowed → run/review per `[ai] mode`; confirm-tier → always drops to an
  editable review line; denied → shown as a comment, never run.
- Agent `sys.run` re-enters the same guard; in the non-interactive CLI path a
  confirm-tier command is refused (there is no one to ask).
- Defaults ship as the `command-guard` plugin: catastrophic `rm`, fork bombs, etc.
  denied; `sudo`, force-pushes, recursive deletes → confirm; plus the Auto-mode
  safe-list (`auto_safe_commands`).

## Redaction

Scoped rules (`terminal` / `ai` / `all`) from config + the `redactor` plugin mask
keys, tokens, and secrets:

- **AI scope** — applied to everything sent to a model (terminal context, tool
  results) and to the session-context file.
- **Terminal scope** — applied to displayed PTY output (ANSI sequences preserved).

The defaults cover Anthropic/OpenAI-style keys, AWS keys, bearer tokens,
`KEY=value` secrets, and more. API keys are **never** searched for on your machine —
only config/env supply them.

## Agent confinement

- An agent may only call the tools its definition lists; unknown tools are refused.
- Agent names are validated (`[A-Za-z0-9_-]+` only) before touching the
  filesystem — `@../../x` can never load a file outside the agents dirs as a
  system prompt. Job ids are contained the same way.
- File **writes** (`fs.write/edit/delete/…`) are confined to the directory the run
  was invoked from (the sandbox);
  reads of secret paths (key files, credential stores) are blocked.
- Sub-agents (`task.run`) are safe-tools-only (read/search) and cannot delegate
  further.
- `[ai] network = false` disables `web`/`net`/`http` tools entirely; when on,
  fetches pass an SSRF rule (no private/loopback/encoded hosts) and use the system
  curl as the only egress.
- Every tool result is redacted (AI scope) before it re-enters the loop.

## Terminal hardening

- The PTY reader isolates the parser (a malformed byte stream can't kill the app)
  and optionally redacts displayed output.
- The session-context file is written 0600 and removed when sharing is off.
- Shell integration only ever *adds* sourcing around your own rc files, and plugin
  snippets run only for trusted (builtin/user-installed) plugins.
