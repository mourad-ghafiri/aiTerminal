# The AI — `@ai`, agents, flows, jobs

All AI in aiTerminal runs **through the terminal**. The window itself never talks to
a model: the shell integration maps `@`-commands onto the `aiTerminal ai` CLI, which
streams into your scrollback. That keeps the terminal light, keeps AI output in your
normal copy/paste/scroll workflow, and gives every run the same guard + redaction
path.

AI is **off by default** — no vendor is assumed. Enable it by declaring a model
(see [Models](#models--pools)).

## `@ai` — a command to run, or an answer

`@ai` is **dual-mode**: the model turns your request into a single shell **command** —
proposed at your prompt to review, edit, and run — or, for a question, a prose **answer**.
It never runs anything itself; you stay in control.

```text
❯ @ai list files
touch hamid                          ← the command, formed live (dim)
❯ press Enter to run (or edit)       ← preloaded — edit it or hit Enter

❯ @ai why is my docker build slow?
The most common causes are cache invalidation and copying node_modules…
✓ 2.1s · 1.2k in / 340 out · ~$0.004
```

**Command mode** — the suggestion is collected fully, run through the **command guard**
*before* it can reach the shell, then:

- `[ai] mode = "manual"` (default) — the command is preloaded into your prompt for review;
  Enter runs it (or edit it first).
- `[ai] mode = "auto"` — a guard-**allowed** command runs immediately. A guard-**confirm**
  command (e.g. `rm -rf`, `sudo`) always drops to review.
- A guard-**denied** command, a model refusal, or an error prints as a `#` comment —
  never silent, never executed.

**Answer mode** — for a question or anything needing explanation, `@ai` streams a prose
answer and preloads nothing. `@ai` grounds on your cwd, shell, recent terminal output
(redacted; see `share_terminal_context`), and recalled memory — so `mkdir x` then
`@ai go into it` yields `cd x`. (Reasoning stays hidden behind the spinner; a broken or
tool-call-shaped reply is shown as an answer, never preloaded as a command.)

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

❯ @job create a file report.md summarizing the repo in 2 minutes  # SCHEDULED
⧖ scheduled job 1753112100-4310 — fires in 2m
  list: aiTerminal ai job  ·  cancel: @job cancel 1753112100-4310

❯ @job                        # bare = list runs + their logs
background jobs (3):
  ⧖ 1753112100-4310 scheduled create a file report.md …   (fires in 2m)
  ▶ 1753112000-4242 running   audit the deps … --agent reviewer
  ✓ 1753111800-4101 done      create a CHANGELOG …
❯ @job cancel 1753112100-4310 # cancel a scheduled or running job
❯ @job clear                  # prune finished jobs
```

**Scheduling.** End a `@job` with a natural time and it defers instead of running now:
`in 2 minutes` / `after 30s` / `in 1 hour` / `at 17:30` / `at 5pm`. The phrase is stripped
from the task, and the job runs later **in the folder where you typed it**. A detached
sleeper fires it at the due time; `@job cancel <id>` stops a scheduled (or running) job.

Foreground `@job` runs play with the full live chrome *and* tee their answer
into the job log, so everything you ran this way stays reviewable. `--bg` also
works on any `@<agent>`, `@flow`, or `@loop` invocation.

Each job is a folder under `~/.aiTerminal/ai/jobs/<id>/`: `job.toml` (status,
command, timestamps, exit code) + `log.md` (the full streamed output — `tail -f` it
to watch live).

A job's status is always honest: `scheduled` (waiting to fire) · `running` · `done` ·
`failed` · `cancelled` (Ctrl+C or `@job cancel`) · `died` (the process vanished — crash,
kill, reboot) · `missed` (a scheduled job whose sleeper died before it fired). The list
detects a dead pid and heals the record on the spot. `@job clear` prunes everything that
is neither running nor scheduled.

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
⠹ thinking…                          ← animated while the model reasons
  ⚙ fs.search {"q":"parse_flow"} · 18ms · 2.1KB
  ⚙ fs.edit {"path":"src/…"} · 6ms · 412B
The fix: the parser dropped the …    ← the answer, streaming
✓ 8.4s · 2 tools · 12.3k in / 1.8k out · ~$0.014
```

A **`@flow`** shows each step announce itself and a compact recap; a **`@loop`** shows
each iteration and a footer with the iteration count:

```text
❯ @flow add a --json flag to the export command
▶ flow 'implement' — 3 step(s)
▶ 1/3 explore  …  ▶ 2/3 implement  …  ▶ 3/3 verify  …
✓ explore · ✓ implement · ✓ verify
✓ 58s · 11 tools · 32k in / 4.1k out · ~$0.21
```

- **Answers** stream token-by-token to stdout; the `@tool …` machine protocol
  never reaches the display (in any dialect — `@tool`, `<tool_call>`, `[TOOL_CALLS]`,
  `<|python_tag|>`, fenced blocks — the parser is model-agnostic).
- **Thinking** is hidden by default — you see only the animated `∴ thinking…`
  indicator, then tools and the answer. Set `[ai] show_reasoning = true` to stream the
  full dim chain-of-thought. Whether a model *reasons* at all is a per-pool-entry
  `thinking = true|false` on any `[[ai.model]]` (a separate catalog capability).
- **Tools** trace live with duration + result size; failures show inline.
- **Footers** show elapsed · tools · tokens, plus an **estimated cost** (`~$0.014`)
  whenever the model has pricing (`price_in`/`price_out`). Set `[ai] budget` (a USD
  soft-cap) and the footer also shows your spend against it (`· 14% of $0.10`), with a
  ⚠ when a run goes over — advisory only, it never blocks (the `@loop --budget` flag is
  a separate token hard-stop).
- **`@ai <request>`** gets the same treatment — spinner, the command forming dim (or a prose
  answer) — ending with the footer, and the command preloaded for review (or run in `auto`
  mode). It proposes a command or answers; it never runs a command itself.
- All of it is TTY-aware: piped output and `--bg` job logs get plain,
  animation-free text automatically, and the chrome colors follow your theme
  (via the live `TT_*` shell colors).

## Folder sessions — the AI remembers each project

Every AI feature (`@ai`, `@<agent>`, `@flow`, `@loop`, `@job`) persists a **session
for the folder it runs in**, so returning to a project restores what the AI knows
about it — automatically, no flags.

- A folder maps to its **project root**: the git top-level if you're inside a repo
  (so every subdirectory of a project shares one session), else the working
  directory itself.
- Each session lives under `~/.aiTerminal/ai/sessions/<id>/`:
  - `session.md` — a compact, rolling **digest of recent runs** (one line each:
    time, mode, request, outcome), byte-capped so it stays small; the oldest drop.
  - `memory/` — a **folder-scoped memory store** (same format as the global one).
  - `meta.toml` — the real root path, timestamps, run count.

On each run the folder's digest and its memory are folded into the AI context
(ahead of the terminal snapshot), and everything is redacted before egress. It is
**lean by design** — a run appends one digest line with *no extra model call*.

## Memory

A structured, retrieval-based memory (a from-scratch BM25 ranker — no DB, no
embeddings service), stored as plain Markdown files with TOML frontmatter.

- **Global** memory: `~/.aiTerminal/ai/memory/` — durable facts across every project.
- **Folder** memory: `~/.aiTerminal/ai/sessions/<id>/memory/` — facts about one project.

Each turn the most relevant memories are recalled into the AI context
(`[ai] memory = true`), **folder-first then global** (a folder fact shadows a global
one with the same id). Agents curate their own memory mid-run via the `memory.*`
tools — in a folder run those writes land in that folder's store, so the project
remembers them next time.

## MCP

Declare MCP servers under `~/.aiTerminal/ai/mcp/`. Agent runs launch them and
expose their tools as
`mcp.<server>.<tool>` alongside the native catalog.

## Models & pools

Models are **data**: one `ai/models/<provider>.toml` per provider (anthropic,
openai, openrouter, deepseek, groq, grok, qwen, kimi, minimax, ollama, lmstudio,
local) declaring its endpoint and key variable once, plus per-model definitions
(params, capabilities, context window, pricing).

There is **one pool and one strategy** — no separate "fast" tier, no global key.
Every request (`@ai`, `@<agent>`, `@flow`, `@loop`) draws a model from the same
pool, weighted by config. AI is **off** until you declare a model; no vendor is
assumed.

Declare models under `MODELS` at the end of the `[ai]` section. They belong last
because TOML gives every `key = value` to the table header above it — an `[ai]`
setting written after an `[[ai.model]]` block would become part of that model.

### Getting started — one model

```toml
[[ai.model]]
provider = "openrouter"                # any provider (see ai/models/*.toml)
id       = "deepseek/deepseek-chat"    # any id that provider serves
api_key  = "sk-or-v1-…"
```

That is the whole setup. `weight` is optional — a lone model serves every request.

### API keys, three ways

Keys belong to the model that needs them, so a mixed pool carries one key per
provider:

| In `config.toml` | Resolves to |
| --- | --- |
| `api_key = "sk-…"` | the key itself |
| `api_key = "$MY_KEY"` or `"${MY_KEY}"` | that environment variable's value |
| *`api_key` omitted* | the provider's own variable — `$ANTHROPIC_API_KEY`, `$OPENAI_API_KEY`, `$OPENROUTER_API_KEY`, `$DEEPSEEK_API_KEY`, … (declared by `ai/models/<provider>.toml`) |

Expansion happens **at request time**, never at parse time, so exporting or
rotating a key takes effect without touching `config.toml`. An unset variable
resolves to nothing (never the literal `"$MY_KEY"`), and you get the setup hint
naming exactly which variable to set. Local providers (`ollama`, `lmstudio`) need
no key at all:

```toml
[[ai.model]]
provider = "ollama"
id       = "llama3.1"
```

### A pool of several models

Add more blocks. `weight` is each model's share of requests; omit it and a model
gets a full 100:

```toml
[[ai.model]]                # ~10% — keep the pricey one rare
provider    = "anthropic"
id          = "claude-opus-4-8"
api_key     = "$ANTHROPIC_API_KEY"
weight      = 10
temperature = 0.3           # 0.0–1.0, lower is more deterministic
max_tokens  = 8000          # response cap (clamped 1–200000)
thinking    = true          # force extended thinking on/off

[[ai.model]]                # ~90% — the everyday workhorse
provider = "openrouter"
id       = "deepseek/deepseek-chat"
api_key  = "sk-or-v1-…"
weight   = 90
top_p    = 0.95             # nucleus sampling
top_k    = 40               # top-k sampling
```

Sampling settings are optional per model — leave them out and the model's catalog
defaults apply.

### How the pool picks

**Weighted** by default; omit `[ai.balance]` entirely unless you want another
strategy. It must sit *above* your `[[ai.model]]` blocks:

```toml
[ai.balance]
strategy = "weighted"       # weighted (default) | round_robin | cost | failover
```

- **weighted** — random, proportional to `weight`.
- **round_robin** — cycle the entries, one per request.
- **cost** — always the cheapest by `price_in + price_out`.
- **failover** — the first entry, with the rest as ordered fallbacks the agent
  path retries on a hard error.

Any model id a known provider serves works — it need not be pre-declared in
`ai/models/<provider>.toml`; just set `provider` so the engine knows the endpoint
and key variable. Each model's catalog entry declares its capabilities —
`vision`, `document`, `thinking`, `tools` — and the engine sends each candidate
only what it supports.


## Context & privacy

Each run's context is assembled in order: the global `aiTerminal.md` instructions,
**this folder's session digest**, the recalled memory (folder-first then global),
the redacted terminal snapshot, and any attached files.

- `share_terminal_context = true` — the window keeps a redacted snapshot of the
  focused pane's recent output in a 0600 temp file; the CLI grounds on it. Off → the
  file is removed and only your request is sent.
- Folder sessions and memory live entirely on your machine under
  `~/.aiTerminal/ai/sessions/`; nothing is uploaded, and the session context is
  redacted on the same egress path as everything else.
- Every egress path applies the **AI-scope redaction rules** (config + the
  `redactor` plugin): keys, tokens, and secrets are masked before leaving.
- `[ai] network = false` cuts agents off from `web.read` / `net.get` / `http.*`
  entirely.
- Keys are never read off your machine — only from config or the provider's env var.

## The tool catalog

This is the **full native catalog**. Each agent declares its own **allowlist** in its
Markdown frontmatter (`tools = [...]`); unlisted tools are refused. The bundled agents
hold a curated subset (below) — write your own `ai/agents/<name>.md` to grant more (e.g.
`http.*` for an API-calling agent, `data.*` for a scratch database).

| Family | Tools | Danger |
| --- | --- | --- |
| `fs` | read, list, stat, home, roots, glob, search (grep), measure, write, edit, append, mkdir, delete, copy, move, open | read = safe; writes are sandbox-confined (the invocation directory) |
| `sys` | run (through the command guard) | exec |
| `diag` | check — native `cargo check`/`ruff` → structured `file:line` diagnostics (workspace-confined) | safe |
| `web` / `net` / `http` | read (page → markdown, incl. git repos/READMEs), get, post | network (`[ai] network` + SSRF rules) |
| `memory` | search, get, add, update, forget, recall, list, consolidate, stats | safe/write |
| `data` / `queue` / `store` | structured tables, queues, KV — an agent's sandboxed scratch database | write |
| `todo` | set, add, done, list, clear — the live plan | safe |
| `task` | run — sub-agent delegation | safe (delegates are read-only) |
| `codec` / `time` | hash, uuid, base64/hex/url, JSON, CSV; date now/add/diff/format | safe |
| `git` / `sec` / `clock` / `clip` | repo browsing (via `web.read`), guard checks, clock, clipboard | mostly safe |

**Bundled agent allowlists:** `coder` — fs read+write, `diag.check`, `sys.run`, `web.read`,
`memory.*`, `todo.*`, `task.run`; `explorer` & `reviewer` — read-only (fs read/search,
`web.read`, `memory`); `tester` — fs read+write + `sys.run`; `ai` — the safe read-only set.

## CLI reference

```text
aiTerminal ai "<prompt>"                     # prose Q&A
aiTerminal ai --command "<request>"          # dual-mode: a guarded command OR prose  (@ai)
aiTerminal ai --agent <name> "<task>"        # agent run                  (@<agent>)
aiTerminal ai --flow <name> "<input>"        # workflow                   (@flow)
aiTerminal ai --loop "<goal>" [--check …]    # engineered agent loop      (@loop)
                 [--max N] [--budget TOKENS] [--agent <name>]
aiTerminal ai --bg …                         # detach any of the above    (--bg)
aiTerminal ai job [clear]                   # job list / prune           (@job)
aiTerminal ai flow                          # flow list                  (@flow)
```
