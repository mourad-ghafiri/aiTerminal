# The AI — `@ai`, agents, flows, jobs

All AI in aiTerminal runs **through the terminal**. The window itself never talks to
a model: the shell integration maps `@`-commands onto the `aiTerminal ai` CLI, which
streams into your scrollback. That keeps the terminal light, keeps AI output in your
normal copy/paste/scroll workflow, and gives every run the same guard + redaction
path.

AI is **off by default** — no vendor is assumed. Enable it by declaring a model
(see [Models](#models--pools)).

## `@ai` — natural language → a command

```text
❯ @ai list every port something is listening on
❯ press Enter to run (or edit)
❯ lsof -iTCP -sTCP:LISTEN -n -P
```

The suggestion is collected fully, run through the **command guard** *before* it can
reach the shell, then:

- `[ai] mode = "manual"` (default) — the command is preloaded into your prompt for
  review; Enter runs it.
- `[ai] mode = "auto"` — a guard-**allowed** command runs immediately. A
  guard-**confirm** command (e.g. `rm -rf`, `sudo`) always drops to review.
- A guard-**denied** command, a model refusal, or any error prints as a `#` comment —
  never silence, never execution.

`@ai` grounds on your cwd, shell, recent terminal output (redacted; see
`share_terminal_context`), and recalled memory — so `mkdir x` then `@ai go into it`
yields `cd x`.

## `@<agent>` — agentic runs

```text
❯ @explorer "how does the theme resolution work?"
❯ @coder "add a --verbose flag to the plugin subcommand"
```

An **agent** is a Markdown file with TOML frontmatter:

```markdown
---
description = "Implements changes end-to-end"
tools = ["fs.read", "fs.search", "fs.edit", "fs.write", "sys.run", "task.run", "todo.set"]
skills = ["refactoring", "testing"]
prompts = ["concise"]
max_steps = 40
---
You are a careful senior engineer. …
```

- Agents live in `~/.aiTerminal/ai/agents/<name>.md`; bundled: `coder`,
  `explorer`, `reviewer`, `tester`, `ai`.
- **Skills** (`ai/skills/*.md`) and **prompts** (`ai/prompts/*.md`) are reusable
  Markdown blocks spliced into an agent's system prompt by name.
- **`~/.aiTerminal/ai/aiTerminal.md`** is the global instructions file — prepended
  to every agent's system prompt and every `@ai` request, so your durable
  preferences shape every run.

The agent loop is provider-agnostic: the model calls tools with a `@tool <name>
<json>` line; the runner executes the tool (see [the tool catalog](#the-tool-catalog))
and feeds the result back. Every tool result is redacted before it re-enters the
loop; `sys.run` re-enters the command guard; file writes are confined to the
directory the run was invoked from (the sandbox).

### Sub-agent delegation — `task.run`

An agent holding the `task.run` tool can fan work out:

```json
@tool task.run {"tasks": [
  {"agent": "explorer", "prompt": "map the config module"},
  {"agent": "tester",   "prompt": "run the config tests and report"}
]}
```

Sub-agents run **in parallel**, are **safe-tools-only** (read/search — never write,
exec, or further delegation), and their reports fold back into the parent's loop.

## `@flow` — multi-step workflows

A **flow** is a TOML file of agent steps; each step is a full agent run and (with
`chain = true`) sees the previous steps' answers.

```toml
# ~/.aiTerminal/ai/flows/review.toml
description = "Explore the relevant code, then review it"
chain = true

[[step]]
label  = "map"
agent  = "explorer"
prompt = "Map the code relevant to: {{input}}"

[[step]]
label  = "review"
agent  = "reviewer"
prompt = "Using the map above, review: {{input}}"
```

```text
❯ @flow                          # no args — list available flows
❯ @flow review the PTY layer     # first word names a flow → run it with the rest
❯ @flow add retry logic to fetch # unknown first word → the whole line becomes
                                 # input to the DEFAULT pipeline (implement:
                                 # explore → implement → verify)
```

`{{input}}` in a step prompt is replaced by the input text. Bundled flows:
`review` (explore → review) and `implement` (explore → implement → verify — also
the default for free text).

## `@loop` — engineered agent loops

The loop-engineering discipline, built in: don't perfect a single prompt — design
the loop the agent runs inside, with a **verifiable goal** and hard stop rules.

```text
❯ @loop "make the config tests pass" --check "cargo test -p framework config::"
🔁 loop 'coder' — up to 5 iteration(s)
▶ iteration 1/5 … ▶ iteration 2/5 …
✓ goal reached after 2 iteration(s)
```

Each iteration: the maker agent works the goal → the verifier runs → its failure
output (tail-capped) feeds the next iteration as structured feedback.

- **`--check "<cmd>"`** — a deterministic verifier; exit 0 = done. It passes the
  command guard first (a denied command never runs; confirm-tier is refused).
- **No `--check`?** The maker/checker split: an independent `reviewer` agent
  (read-only tools) inspects the work and must conclude `VERDICT: PASS` or
  `VERDICT: CONTINUE` + concrete gaps — the model that did the work never grades
  its own homework.
- **Stop rules** — success · `--max N` iteration cap (default 5) · **stalled**
  (identical verifier output twice in a row = no progress) · `--budget TOKENS`
  (a hard total-token ceiling). Never an open-ended run.
- **`--agent <name>`** picks the maker (default `coder`); **`--bg`** detaches the
  whole loop as a tracked job (`@job` + `tail -f` the log).

Exit codes: `0` goal reached · `1` stalled/exhausted/budget · `2` setup error —
so loops compose with shell logic and CI.

See [examples/ai/loop.md](../examples/ai/loop.md) for recipes.

## `@job` — tracked tasks

`@job <task>` runs an agent task **as a recorded job** — flags optional,
anywhere in the line:

```text
❯ @job create a CHANGELOG from the last 10 commits            # coder, foreground,
                                                              # streamed live AND logged
❯ @job audit the deps for unused entries --agent reviewer --bg # named agent, detached
▶ background job 1753112000-4242
  monitor: aiTerminal ai job     ·  tail -f ~/.aiTerminal/ai/jobs/…/log.md

❯ @job                        # bare = list runs + their logs
background jobs (2):
  ▶ 1753112000-4242 running   audit the deps … --agent reviewer
  ✓ 1753111800-4101 done      create a CHANGELOG …
❯ @job clear                  # prune finished jobs
```

Foreground `@job` runs play with the full live chrome *and* tee their answer
into the job log, so everything you ran this way stays reviewable. `--bg` also
works on any `@<agent>`, `@flow`, or `@loop` invocation.

Each job is a folder under `~/.aiTerminal/ai/jobs/<id>/`: `job.toml` (status,
command, timestamps, exit code) + `log.md` (the full streamed output — `tail -f` it
to watch live).

A job's status is always honest: `running` · `done` · `failed` · `cancelled`
(you pressed Ctrl+C) · `died` (the process vanished — crash, kill, reboot; the
list detects a dead pid and heals the record on the spot). `@job clear` prunes
everything that is no longer running, healed zombies included.

## Exit codes & scripting

Every AI command tells the shell the truth, so `$?`, `&&`, and CI compose:

| code | meaning |
|------|---------|
| `0`  | the run completed (for `@loop`: the goal verified) |
| `1`  | the run failed — model/transport error, step limit, tool stall; loop stalled/exhausted/out of budget |
| `2`  | setup error — unknown agent/flow, AI not configured, guard-blocked check |
| `130`| interrupted — Ctrl+C cancelled the run cleanly |

**Ctrl+C** cancels the in-flight request immediately (the stream is killed
mid-token), stops the agent loop before its next turn, exits `130`, and — for a
foreground `@job` — stamps the record `cancelled`. Background jobs are fully
detached from the window (their own session), so closing the terminal never
kills them.

A `@loop --check` command is itself bounded: a verifier that hangs is killed
after 10 minutes and the loop aborts with a setup error instead of stalling
forever.

## Attachments — files, images, PDFs

Any `@<path>` token in a prompt that names an existing file becomes an attachment
— in `@ai`, `@<agent>`, `@flow`, and `@loop` alike:

```text
❯ @ai what does this diagram show? @design/arch.png
❯ @reviewer "does the implementation match the spec?" @spec.pdf @src/parser.rs
```

- **Images** (`png/jpg/jpeg/gif/webp`, ≤4 MB) ride the request as vision blocks —
  sent only to models whose catalog declares `vision` (a non-vision failover
  candidate gets the text-only request).
- **PDFs** ride as document blocks, gated on the `document` cap (Anthropic models;
  providers without a document shape never receive them malformed).
- **Text files** inline into the context as fenced blocks (≤48 KB, truncated
  beyond, binaries skipped) and pass the same redaction as everything else.
- Agents can additionally *read* any file themselves through their `fs.*` tools —
  attachments are for putting something in front of the model up-front.

## The live experience

Every run plays out in your scrollback like a first-class harness — chrome on
stderr, content on stdout, so piping stays clean:

```text
❯ @coder "fix the failing parser test"
✦ @coder · claude-opus-4-8
⠹ thinking…                          ← spinner until the first token
∴ The test expects a trailing …      ← reasoning, dim, live
  ⚙ fs.search {"q":"parse_flow"} · 18ms · 2.1KB
  ⚙ fs.edit {"path":"src/…"} · 6ms · 412B
The fix: the parser dropped the …    ← the answer, streaming
✓ 8.4s · 2 tools · 12.3k in / 1.8k out
```

- **Answers** stream token-by-token to stdout; the `@tool …` machine protocol
  never reaches the display.
- **Thinking** streams dim to stderr under a `∴` marker. Models declare the
  capability in the catalog; force it per pool entry with `thinking =
  true|false` on any `[[ai.model]]`.
- **Tools** trace live with duration + result size; failures show inline.
- **`@ai <request>`** gets the same treatment — spinner, thinking, the command
  forming dim — ending with the token footer and the command preloaded at your
  prompt for review.
- All of it is TTY-aware: piped output and `--bg` job logs get plain,
  animation-free text automatically, and the chrome colors follow your theme
  (via the live `TT_*` shell colors).

## Memory

A structured, retrieval-based memory (a from-scratch BM25 ranker — no DB, no
embeddings service). Plain Markdown files:

- Plain Markdown files in `~/.aiTerminal/ai/memory/`

Each turn, the most relevant memories are recalled into the AI context
(`[ai] memory = true`). Agents curate their own via the `memory.*` tools.

## MCP

Declare MCP servers under `~/.aiTerminal/ai/mcp/`. Agent runs launch them and
expose their tools as
`mcp.<server>.<tool>` alongside the native catalog.

## Models & pools

Models are **data**: one `ai/models/<provider>.toml` per provider (anthropic,
openai, openrouter, deepseek, groq, grok, qwen, kimi, minimax, ollama, lmstudio,
local) declaring its endpoint/key-env once, plus per-model definitions (params,
context window, pricing). Enable AI with one block in `config.toml`:

```toml
[[ai.model]]
provider = "anthropic"
id       = "claude-opus-4-8"
api_key  = ""              # or export ANTHROPIC_API_KEY
```

Declare several `[[ai.model]]` tables to load-balance
(`[ai.balance] strategy = "weighted" | round_robin | cost | failover`, per-model
`weight`, sampling overrides, and a `thinking = true|false` cap override).
Each model's catalog entry declares its capabilities — `vision`, `document`,
`thinking`, `tools` — and the engine sends each candidate only what it supports. Any model id a known provider serves works — it need
not be pre-declared. The **fast model** (NL→command) is drawn from your pool unless
`fast_model` overrides it.

## Context & privacy

- `share_terminal_context = true` — the window keeps a redacted snapshot of the
  focused pane's recent output in a 0600 temp file; the CLI grounds on it. Off → the
  file is removed and only your request is sent.
- Every egress path applies the **AI-scope redaction rules** (config + the
  `redactor` plugin): keys, tokens, and secrets are masked before leaving.
- `[ai] network = false` cuts agents off from `web.read` / `net.get` / `http.*`
  entirely.
- Keys are never read off your machine — only from config or the provider's env var.

## The tool catalog

Native tools agents may hold (each agent lists its own; unlisted tools are refused):

| Family | Tools | Danger |
| --- | --- | --- |
| `fs` | read, list, stat, home, roots, glob, search (grep), measure, write, edit, append, mkdir, delete, copy, move, open | read = safe; writes are sandbox-confined (the invocation directory) |
| `sys` | run (through the command guard) | exec |
| `web` / `net` / `http` | read (page → markdown, incl. git repos/READMEs), get, post | network (`[ai] network` + SSRF rules) |
| `memory` | search, get, add, update, forget | safe/write |
| `data` / `queue` / `store` | structured tables, queues, KV (sandboxed dir) | write |
| `todo` | set, add, done, list, clear — the live plan | safe |
| `task` | run — sub-agent delegation | safe (delegates are read-only) |
| `git` | repo browsing behind `web.read` | network |
| `diag` / `sec` / `clock` / `time` / `codec` / `files` / `clip` | diagnostics, guard checks, time, hashing/encoding, file management, clipboard | mostly safe |

## CLI reference

```text
aiTerminal ai "<prompt>"                     # Q&A (what plain @ai text falls back to)
aiTerminal ai --command "<request>"          # NL → one guarded command   (@ai)
aiTerminal ai --agent <name> "<task>"        # agent run                  (@<agent>)
aiTerminal ai --flow <name> "<input>"        # workflow                   (@flow)
aiTerminal ai --loop "<goal>" [--check …]    # engineered agent loop      (@loop)
                 [--max N] [--budget TOKENS] [--agent <name>]
aiTerminal ai --bg …                         # detach any of the above    (--bg)
aiTerminal ai job [clear]                   # job list / prune           (@job)
aiTerminal ai flow                          # flow list                  (@flow)
```
