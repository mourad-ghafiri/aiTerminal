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

## The redactor — secrets stay on your machine

A secret you `cat` is *yours*: it is already on your screen, on your disk, in your
environment. The boundary that matters is **egress** — the moment text is about to
leave for a model, a chat app, or a file another process can read. The redactor sits
exactly there, rewriting matches to `«redacted»` before they cross.

### Scopes — where a rule applies

| Scope | Applied to |
| --- | --- |
| `ai` | Everything bound for a model: the terminal context `@ai` grounds on, tool results returned to an agent, the `@loop`/`@job` prompt context, and the session-context file the GUI writes for the CLI. |
| `terminal` | Live PTY output, as it is displayed. Applied **per printable run**, never to an escape sequence, so colors and cursor moves can't be corrupted. |
| `all` | Both. This is the default when a rule omits `scope`. |

`@gate` deliberately applies **both** scopes to anything it sends to a chat — a phone
is off-machine either way, so the narrower reading would be the wrong one.

### The default rules

Nine patterns ship as the `redactor` plugin (`builtin/plugins/redactor/plugin.toml`),
all scoped `ai`. The *mechanism* is native; the plugin only supplies the rules, so you
can edit, extend, or `@plugin disable redactor` them.

| Catches | Pattern |
| --- | --- |
| AWS access key ids | `AKIA[0-9A-Z]{16}` |
| OpenAI / Anthropic keys | `sk-[A-Za-z0-9_-]{16,}` |
| GitHub tokens | `gh[pousr]_[A-Za-z0-9]{20,}` |
| Slack tokens | `xox[abps]-[A-Za-z0-9-]{8,}` |
| Google API keys | `AIza[A-Za-z0-9_-]{20,}` |
| `Authorization: Bearer …` | `(?i)bearer\s+[A-Za-z0-9._~+/-]+=*` |
| JWTs | `eyJ….eyJ….…` |
| Any sensitive-looking `KEY=value` | `(?i)(api[_-]?key\|access[_-]?key\|client[_-]?secret\|token\|secret\|password\|passwd\|credential\|auth)["']?\s*[:=]\s*\S+` |
| PEM private-key blocks | `-----BEGIN … PRIVATE KEY----- … -----END …-----` |

**They are off your screen by default.** Every shipped rule is `scope = "ai"`, so
`cat .env` still shows you your own values — only what leaves is rewritten. Add
`scope = "terminal"` rules if you also want them masked in the display (useful when
you screen-share or record).

### What it looks like

Real output from the shipped rules:

```
DATABASE_URL=postgres://db.internal/prod      →  DATABASE_URL=postgres://db.internal/prod
AWS_ACCESS_KEY_ID=AKIA3RJHF2P9QLXMZB4T        →  AWS_ACCESS_KEY_ID=«redacted»
ANTHROPIC_API_KEY=sk-ant-api03-9Fk2LmQ7xTvB   →  ANTHROPIC_«redacted»
GITHUB_TOKEN=ghp_8sK2mVx91QpLzR4tYnB7wDe3Fg   →  GITHUB_«redacted»
LOG_LEVEL=debug                               →  LOG_LEVEL=debug
```

Two things to read off that. Redaction is **targeted** — the connection string and the
log level pass through untouched, so the model keeps enough context to be useful. And
rules **compose**: each runs over the previous one's output, so a key caught by the
`sk-` rule can then be caught again by the `KEY=value` rule, which takes the key *name*
with it. That is why `ANTHROPIC_API_KEY=…` becomes `ANTHROPIC_«redacted»` while
`AWS_ACCESS_KEY_ID=…` keeps its name (`ACCESS_KEY_ID=` doesn't match `access[_-]?key`
followed immediately by `=`). Over-redacting is the safe direction.

### Your own rules

Add `[[redact]]` tables to `config.toml` (or to any plugin's `plugin.toml`):

```toml
[[redact]]
pattern     = "acme_[a-z0-9]{32}"   # regex by default
replacement = "«acme key»"          # optional; defaults to «redacted»
scope       = "all"                 # terminal | ai | all (default all)

[[redact]]
pattern = "10.0.42.17"              # an exact string
literal = true                      # skip the regex engine entirely
scope   = "ai"
```

Config rules are applied **before** plugin rules, so yours get first refusal on a
match.

### Properties worth knowing

- **The regex engine is step-budgeted.** A pathological pattern fails fast instead of
  hanging your terminal — there is no ReDoS surface, even from a plugin you installed.
- **A bad pattern is skipped, not fatal.** It prints `security rule skipped — …` and
  the rest of the policy still loads.
- **Clean text costs nothing.** Each rule carries its mandatory literal head (`sk-`,
  `AKIA`) as a `contains` prefilter, and a rule that changes nothing never reallocates
  — this runs on every PTY chunk.
- **Agents can call it too.** `sec.redact(text, scope)` is in the tool catalog, so an
  agent can scrub something before writing it into a file or a report.
- **It is not a secret scanner.** It rewrites text in flight; it does not search your
  disk, and it cannot mask a secret that never passes through it. API keys are never
  hunted for on your machine — only config and env supply them.

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

## Remote gateways (`@gate`)

`@gate` hands a shell to a chat app, so it gets the strictest defaults in the product:
off unless `[gates] enabled = true`, and **nothing is accepted from any chat** until it
sends the six-digit code printed in your terminal. An unpaired chat gets no reply at
all — a stranger who finds the bot cannot even confirm it is live. Five wrong codes
close pairing for the session; while nobody is paired the code rotates every ten
minutes. Only one chat may be paired at a time.

Once paired, every command goes through the same command guard above (`denied_commands`
blocks, `confirm_commands` waits for an explicit `/yes` from the chat), and every
`[[redact]]` rule is applied to output before it leaves the machine — in **both** the
`terminal` and `ai` scopes, since a chat app is off-machine either way. Live gate
records under `~/.aiTerminal/gates/` are written `0600` and are on the agent's blocked
path list.

**The guard covers shell commands, not program input.** When `@gate` attaches to an
interactive program (Claude Code, a REPL, `vim`), what you send is typed *into that
program* — it is not a shell line, and there is nothing for a command regex to check. An
explicit `/run` is refused outright while a program holds the terminal rather than being
queued for later. In that mode the program's own confirmations are the control, which is
the point: you answer them from your phone.

Stated plainly: **the guard is a speed bump, not a sandbox** (`l=rm; $l -rf /` defeats
any regex), and a paired chat has your shell — your files, your keys, your money.
Pairing is the real control. The full threat discussion, including what this does *not*
protect against, is in [gate.md](gate.md#security-model).

## Terminal hardening

- The PTY reader isolates the parser (a malformed byte stream can't kill the app)
  and optionally redacts displayed output.
- The session-context file is written 0600 and removed when sharing is off.
- Shell integration only ever *adds* sourcing around your own rc files, and plugin
  snippets run only for trusted (builtin/user-installed) plugins.
