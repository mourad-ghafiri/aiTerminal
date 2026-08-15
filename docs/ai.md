# The AI — `@ai`, agents, flows, jobs

All AI in aiTerminal runs **through the terminal**. The window itself never talks to
a model: the shell integration maps `@`-commands onto the `aiTerminal ai` CLI, which
streams into your scrollback. That keeps the terminal light, keeps AI output in your
normal copy/paste/scroll workflow, and gives every run the same guard + redaction
path.

AI is **off by default** — no vendor is assumed. Enable it by declaring a model
(see [Models](#models--pools)).

## Without writing code

None of this needs a repo. If you never open a source file, five of these are still for
you — and the table says exactly what each one needs, so nothing here fails on your
machine for a reason you could not have known.

| You want to | Type | Needs |
| --- | --- | --- |
| understand a long PDF, or a photo of a page | `@ai summarise @~/Downloads/lease.pdf` | a model whose entry has `enable_document` (images: `enable_vision`) — otherwise the file is **dropped** |
| research a decision, with real sources | `@flow research "which e-bike for a hilly commute"` | `[ai] network = true`. Search is **keyless** — no extra account |
| tidy or rename a pile of files | `@ai move every screenshot into folders by month` | a model. `@ai` **proposes** the command and preloads it for you to read |
| read a long web page | `@researcher "what does this say — <url>"` | `[ai] network = true` |
| draft something, saved to a file | `@writer "turn @notes.md into a brief at brief.md"` | a model |
| hold writing under a word count | `@loop "cut intro.md under 200 words" --agent writer --check "test $(wc -w < intro.md) -lt 200"` | a model, and a command whose **exit status** decides |
| a weekly digest, in plain English | `@job "every Monday at 9, summarise ~/Documents/inbox"` | a model to read the schedule once |
| write and preview at the same time | `@md edit letter.md` | **nothing** — no model, no network |
| ask your terminal from your phone | `@gate telegram start` | a bot token |

Two things are worth knowing before you copy a line off this page.

**`@ai` has no tools.** It answers, or it proposes one command for you to review — it
cannot open a file, fetch a URL or read your clipboard. What it *can* see is an
**attachment**: any `@<path>` naming a real file rides along with the question. To have
something read a page or a folder, use an agent (`@researcher`, `@explorer`) — those have
tools.

**An attachment can be silently dropped.** A PDF only reaches a model whose catalogue
entry declares `enable_document`, an image one with `enable_vision`. If yours does not, the
question still goes — without the file.

These commands need **no model at all**: `@md`, `@theme`, `@profile`, `@config`,
`@plugin`, `@agent`, `@flow check`, `@flow graph`, and `@job -- <command>`.

## `@ai` — a command to run, or an answer

`@ai` is **dual-mode**: the model turns your request into a single shell **command** —
proposed at your prompt to review, edit, and run — or, for a question, a prose **answer**.
It never runs anything itself; you stay in control. It has **no tools** — see
[Without writing code](#without-writing-code) for what that means in practice.

The contract under the hood is deliberately tiny and **streamable**: either the whole reply
is one `RUN: <command>` line, or it is prose. Only the undecided first few characters are held
back, so an answer starts rendering block-by-block while it is still arriving rather than
appearing all at once at the end. The marker is matched case-insensitively and after leading
space, because models are inconsistent about both — and only the **first line** of a command is
ever taken, so a model that keeps talking cannot append a second line to your prompt.

```text
❯ @ai list files
du -sh .                             ← the proposed command
❯ press Enter to run (or edit)       ← preloaded — edit it or hit Enter

❯ @ai why is my docker build slow?
The most common causes are cache invalidation and copying node_modules…
✓ 2.1s · 1.2k in / 340 out · ~$0.004
```

**Command mode** — the suggestion is run through the **command guard**
*before* it can reach the shell, then:

- `[ai] mode = "manual"` (default) — the command is preloaded into your prompt for review;
  Enter runs it (or edit it first).
- `[ai] mode = "auto"` — a guard-**allowed** command runs immediately. A guard-**confirm**
  command (e.g. `rm -rf`, `sudo`) always drops to review.
- A guard-**denied** command, a model refusal, or an error prints as a `#` comment —
  never silent, never executed.

**Answer mode** — for a question or anything needing explanation, `@ai` shows a prose
answer and preloads nothing. `@ai` grounds on your cwd, shell, recent terminal output
(redacted; see `share_terminal_context`), and recalled memory — so `mkdir x` then
`@ai go into it` yields `cd x`. (Reasoning stays hidden behind the spinner; a reply that
doesn't parse to a command is shown as an answer, never preloaded — no misfire, no hang.)

## `@<agent>` — agentic runs

```text
❯ @explorer "how does the theme resolution work?"
❯ @coder "add a --verbose flag to the plugin subcommand"
❯ @agent                    # every agent you have
❯ @agent researcher         # one in full: tools, skills, what it returns
```

Eight ship, and each is a file you can edit:

| Agent | For |
| --- | --- |
| `ai` | the general assistant behind `@ai` |
| `planner` | turns a goal into a small plan with a concrete "done when" |
| `explorer` | read-only scout — maps the code and reports back tightly |
| `researcher` | finds sources, reads them, and reports what they say, with links |
| `coder` | the smallest correct edit, following the project's own conventions |
| `tester` | finds and runs the project's own tests, and reports what happened |
| `reviewer` | read-only review — correctness, security, tests, design |
| `writer` | documentation and reports, saved to the file rather than printed |

**Every agent ends by stating what it returns.** That is not a style rule: a flow node
chains on that text, so `{{explore.output}}` is only as good as the agent's discipline.
`@agent <name>` shows the contract without opening the file. `tester` and `reviewer` go
further and promise a final `VERDICT: PASS` / `VERDICT: FAIL` line — an agent that
*reports* a failure has still finished its run successfully, so that line is how a
workflow tells the difference.

An **agent** is a Markdown file with TOML frontmatter:

```markdown
---
description = "Implements changes end-to-end"
tools = ["fs.read", "fs.search", "fs.edit", "fs.write", "sys.run", "task.run", "todo.set"]
skills = ["refactoring", "testing"]
max_steps = 40
---
You are a careful senior engineer. …
```

- Agents live in `~/.aiTerminal/ai/agents/<name>.md`; **8 bundled**: `ai`,
  `coder`, `explorer`, `planner`, `researcher`, `reviewer`, `tester`, `writer`.

  `@agent` lists them with what each is made of and what it is *for* — the
  description in full rather than clipped at the window's edge, and the skills
  spliced into its prompt, which are half of why two agents with the same tools
  behave differently:

  ```text
  ❯ @agent
  agents (8):
    served by claude-sonnet-5

    @coder       25 tools · 8 skills · 24 steps
        Senior engineer + orchestrator — explores, makes the smallest correct
        edit, verifies, delegates.
        skills   concise · planning · orchestration · code-review · testing · …

    @explorer    7 tools · 1 skill · 12 steps
        Fast read-only scout — maps the relevant code and reports back tightly.
        skills   concise
  ```

  The model is named once, at the top, because it is a property of the `[ai]`
  pool and not of any agent — printing it on all eight rows would imply a
  per-agent setting that does not exist. A weighted pool says *one of*.

  `@agent <name>` shows one in full: the same header, then its tools **grouped by
  family**, so "it can read files and run commands but not reach the network" is
  one glance rather than twelve lines read one at a time — then the output
  contract a flow node chains on, and the file's own path.
- **Skills** (`ai/skills/*.md`) are reusable Markdown blocks spliced into an
  agent's system prompt by name, **in the order the agent declared them** — so the
  list reads as a priority, and the same agent always builds a byte-identical
  prompt. **12 ship**: `code-review`, `concise`, `debugging`, `git`,
  `orchestration`, `planning`, `refactoring`, `research`, `security-review`,
  `testing`, `verification`, `writing`. An agent declares them with
  `skills = ["testing", "git"]`.
- **`prompts = [...]`** (`ai/prompts/*.md`) is the same mechanism under a second
  name, for your own blocks. Nothing ships in it: one bundled answer to "how do I
  reuse a block of prompt" is enough, and the two registries behave identically.
- An agent file is **validated**: a tool it names must exist in the capability
  registry, a skill or prompt it names must be installed, its description must be
  non-empty and `max_steps` sane. `@agent` marks a file that fails with `⚠` and
  `@agent <name>` lists the problems — rather than running it with a silently
  weaker prompt.
- **`~/.aiTerminal/ai/aiTerminal.md`** is the global instructions file — prepended
  to every agent's system prompt and every `@ai` request, so your durable
  preferences shape every run.

The agent loop is provider-agnostic: the model calls tools with a `@tool <name>
<json>` line; the runner executes the tool (see [the tool catalog](#the-tool-catalog))
and feeds the result back. Every tool result is redacted before it re-enters the
loop; `sys.run` re-enters the command guard; file writes are confined to the
directory the run was invoked from (the sandbox).

**A turn may carry several calls** — one `@tool` line each, up to eight. They run in the
order written and every result comes back together. This is the difference between four
file reads costing four model round trips and costing one, and a round trip is the
expensive part: each one re-sends the whole transcript, which is longer than the last.
The only rule is that a batch must be *independent* — a call whose arguments come from
an earlier call's result belongs on the next turn. Other dialects are accepted for models
that will not emit ours (`<tool_call>`, `[TOOL_CALLS]`, fenced blocks, Llama pythonic, and
a bare `<tool> {json}` line with no marker at all — recognised only when the leading token
is a tool *this agent declared*, so prose that mentions one is still prose), and each is
read for **every** call it carries rather than the first.

**A run that hits a bound still answers.** When `max_steps` runs out — or the stuck-loop
breaker fires — the loop spends one more turn with the tools withdrawn, asking for the
best answer the transcript supports. The outcome is unchanged (`⚠` in the footer, exit
`1`), because the bound really did fire; what changes is that you get the findings rather
than a sentence about a counter. This matters most inside `@flow`: a node whose agent ran
out of steps used to fail, block everything downstream of it, and end the run — after
doing most of the work.

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

## `@flow` — a workflow declared as a graph

A **flow** is a TOML file of `[[node]]` entries. Each node is one unit of work — an
agent run, a shell command, or a pause for you — and `needs` names what must finish
first. That is the whole idea: a graph, not a line.

```toml
# ~/.aiTerminal/ai/flows/ship.toml
description = "Explore → implement → verify → fix until green"
input       = "required"

[bounds]
timeout = "30m"   budget = 400000   concurrency = 4

[[node]]
id     = "map"
agent  = "explorer"
prompt = "Map the code for: {{input}}"

[[node]]
id     = "build"
agent  = "coder"
needs  = ["map"]
prompt = "Implement it, following:\n{{map.output}}"

[[node]]
id    = "verify"
run   = "cargo test"          # a command node — zero tokens
needs = ["build"]

[[node]]
id     = "fix"
agent  = "coder"
needs  = ["verify"]
when   = "verify.failed"      # a conditional edge
prompt = "Fix:\n{{verify.output}}"
goto   = "verify"             # a bounded retry loop
max    = 3
```

Four things a chain could not do, and this can:

- **Nodes that need nothing from each other run at the same time.** Three reviews
  cost one review's wall clock. `[bounds] concurrency` caps how many are in flight.
- **`when` puts the decision on the edge**, as data this tool parses — not as an
  instruction a model interprets, differently, next time.
- **A `run` node costs no tokens.** It is a command through the same guard as
  everything else, and its exit status is a fact the graph branches on. The model is
  spent only where judgement is needed.
- **`goto` points one edge backwards**, bounded by `max` — so "test, fix, test again"
  is a flow rather than something you sit and supervise.

### The vocabulary

| On a node | Means |
| --- | --- |
| `id` | its name — how other nodes refer to it (required) |
| `agent` + `prompt` | a full agent run |
| `run` | a shell command, through the guard, costing nothing |
| `kind = "approve"` + `show` | stop and ask you |
| `needs = ["a","b"]` | run after these — the edges of the graph |
| `when = "a.failed"` | only run if this holds |
| `goto = "a"` · `max = 3` | after this node, run `a` again — bounded |
| `over` + `as` | fan out: one run per item of a list, in parallel |
| `retry = 1` · `timeout = "10m"` · `max_steps = 20` | this node's own bounds |
| — | *beyond* `retry`, a node gets **one** extra attempt when the failure was a transport or provider error rather than an answer. A graph should survive a blip on its first node; it should not pay twice for the same wrong answer. |
| `solo = true` | never run alongside another node |
| `optional = true` | a failure here blocks nothing and fails nothing |
| `final = true` | this node's answer is the flow's answer |

**Conditions** are deliberately not a programming language:

```text
a.passed · a.failed · a.skipped · a.ran · a.approved
a.exit == 1            (also != < >)
a.output contains "0 failed"   ·   a.output matches /(\d+) failed/
not X · X and Y · X or Y · ( … )
```

**References** — `{{input}}` · `{{<node>.output}}` · `{{<node>.exit}}` · `{{flow.name}}`
· the item name inside an `over` node. There is no implicit "everything so far" blob:
a node sees exactly what it asks for.

### Nothing runs until the graph is proved

`@flow check` costs nothing and needs no model. It refuses a dangling `needs`, a
reference to a node that does not run first, a condition that names a node that does
not exist, an agent that is not installed, and a command the guard would refuse — and
it warns about the rest.

```text
❯ @flow                          # the installed flows
❯ @flow check [<name>]           # verify one, or all of them
❯ @flow graph <name>             # the graph, drawn, with what each node reaches
❯ @flow build "add a --json flag to the export command"
❯ @flow build --bg "…"           # detached, tracked as a job
❯ @flow review "this branch" --dry-run --concurrency 2
❯ @flow build --view list "…"    # the dense board instead of the graph
❯ @flow runs                     # past runs
❯ @flow show <id>                # the graph again, with what each node cost
❯ @flow nodes [<id>]             # every node of a run, side by side
❯ @flow node [<id>] <node>       # one node in full: model, cost, transcript
❯ @flow watch [<id>]             # attach to a live run (Ctrl-C detaches)
❯ @flow log <id> [<node>] [-f]   # what a node actually said
❯ @flow resume <id>              # run only what did not complete
❯ @flow retry [<id>] <node>      # run one node again, and what depended on it
❯ @flow clear                    # prune finished runs
```

`--view graph|list` works on any of the drawing verbs. Without it they use `[flow]
view`, which ships as `graph`.

**What a run says is drawn as the document it is.** An agent answers in Markdown and
mermaid, so the flow's answer, `@flow node` and `@flow log` all render it — headings,
tables, bullets, and diagrams as pictures. A `run` node did not write prose, it wrote its
command's output, and that is passed through untouched. Either way a **pipe gets the
source unchanged**, so `@flow review "…" > review.md` writes the Markdown the model wrote.
See [markdown.md](markdown.md).

### Five flows ship

| Flow | What it does |
| --- | --- |
| `build` | plan → map the code and its conventions **in parallel** → implement → test → fix until green → review → summarise |
| `fix` | reproduce the failure **first** → find its cause → patch → prove it is gone |
| `review` | map once, then **three reviewers at the same time** (correctness, security, design) → one merged verdict |
| `research` | break the question into sub-questions → research each **in parallel** → compare → report, with sources |
| `document` | read the real code → write the file → check every claim against the source → revise until it holds |

None of them names a build tool, a test command or a language: `@tester` finds the
project's own runner. That is what makes them yours rather than somebody else's.

`research` is the one that is not about code at all — it answers any question, and it
needs `[ai] network = true`. Its search is **keyless** (DuckDuckGo's HTML endpoint), so
there is no second account to create.

### A goal on its own — the graph gets built

```text
❯ @flow explain how the export command works
◈ no flow named — building a graph for this goal
◈ built a 3-node graph: read the code, then explain it · @flow show 1785371201-90257
▸ explain-how-the-export · explain how the export command works
```

You do not have to already have the right flow. Name none and **one is written for this
goal** — by the model, out of the agents this machine actually has — and then held to
every check a graph you wrote by hand is held to: it goes through the same parser and the
same verifier, so an agent that does not exist, an edge that points nowhere and a command
the guard refuses are all caught before a token is spent running it. When the checker
refuses it, its own errors are handed back for **one** more attempt; a second failure
prints them and stops, having spent two small calls and no agent runs.

The graph lives in the run's own record (`ai/flow-runs/<id>/flow.toml`), so `@flow show`,
`node`, `log`, `retry` and `resume` all work on it and you can read exactly what was made
for you — and `@flow` keeps listing the five flows you meant to have rather than filling
up with one-off graphs. A build the checker refused is kept too: seeing what it tried to
make of your sentence is the fastest way to understand what it misread.

**The first word decides**, against what you have installed:

| You typed | What happens |
| --- | --- |
| `@flow document this project` | `document` is a flow you have → it runs, with `this project` as its input |
| `@flow revieew the parser` | close enough to `review` to be a typo → refused, with the suggestion |
| `@flow explain this project` | neither → the whole line is a goal, and a graph is built for it |

The typo guard is why the third row is safe. `@flow revieew the parser` must never
quietly become a different flow — and it must not become a *goal* either, because
building and running a graph for a misspelling is the same footgun in a newer coat.

### The graph is a document, not just a picture

`@flow graph` builds a **Markdown document** — a heading, the diagram as a `mermaid`
fence, and a table of the facts — and hands it to the renderer `@md` already uses. So
in aiTerminal the diagram is drawn by the same GPU renderer that draws every other
diagram, and in a pipe it degrades to box art and a plain table. Neither is a special
case; both fall out of it being ordinary Markdown.

A picture answers "what runs after what" and nothing else. The questions people arrive
with are *which agent is behind that box*, *what can it reach*, and *what is the
condition on that arrow* — so those are the columns:

```text
❯ @flow graph review

review
────────────────────────────────────────────────────────────

Map the code, review it three ways in parallel, then merge into one verdict

5 nodes · 3 parallel · 20m · 4 at a time · needs an input


                             ┌───────────────┐
                             │ map @explorer │
                             └───────────────┘
                                     │
             ┌───────────────────────┴──┬───────────────────────┐
             ▼                          ▼                       ▼
 ┌───────────┴───────────┐   ┌──────────┴─────────┐   ┌─────────┴────────┐
 │ correctness @reviewer │   │ security @reviewer │   │ design @reviewer │
 └───────────────────────┘   └────────────────────┘   └──────────────────┘
             │                          │                       │
             └───────────────────────┬──┴───────────────────────┘
                                     ▼
                            ┌────────┴─────────┐
                            │ report @reviewer │
                            └──────────────────┘

╭─────────────┬───────────┬──────┬────────────────────╮
│ node        │ runs      │ when │ reaches            │
├─────────────┼───────────┼──────┼────────────────────┤
│ map         │ @explorer │ —    │ 7 tools · 1 skill  │
│ correctness │ @reviewer │ —    │ 7 tools · 5 skills │
│ security    │ @reviewer │ —    │ 7 tools · 5 skills │
│ design      │ @reviewer │ —    │ 7 tools · 5 skills │
│ report      │ @reviewer │ —    │ 7 tools · 5 skills │
╰─────────────┴───────────┴──────┴────────────────────╯
```

`@flow show <id>` prints the same document after a run, with each node's real state,
model and cost written over it. A window too narrow to draw the diagram in gets the
outline instead — never raw diagram syntax.

### Every node is written down, so a run can be picked back up

Each node's result lands in `ai/flow-runs/<id>/` the moment it happens. **`@flow
resume` replays the finished nodes from disk and runs only what did not complete** —
a six-node flow that died at node five costs one node to finish, not six. A failed
node stops its own branch and nothing else, so the independent work is kept rather
than thrown away.

An `approve` node asks on a terminal; detached, it parks the run as `waiting` and
`@flow resume` picks it up with somebody there — it never deadlocks a background job.

### Watching it run

A chain can narrate itself; a graph cannot. Four nodes start together and finish in
whatever order they finish, so a stream of start/done lines hides the most useful thing
about the run. So you watch it as **the graph it is** — and the layout is the graph, not
the file: **a rank is a column**. What runs first is on the left, what runs at the same
time stacks in one column, and the arrows only ever point the way the work moves.

```text
▸ build · add a --json flag to the export command
  8 nodes · 6 agents · 69 tools · 24 skills · 4 at a time · slowest path plan→apply→verify→summary
  ╭──────────────────────────╮    ╭──────────────────────────╮    ╭──────────────────────────╮
  │ ✓ plan                ⚙3 │    │ ✓ explore            ⚙12 │    │ ⠻ apply               ⚙4 │
  │ @planner · sonnet-5      │───▸│ @explorer · sonnet-5     │───▸│ @coder · opus-5          │
  │ 4.2s · 9.0k              │  ╎││ 8.1s · 6.9k              │  ╎││ ⚙ fs.edit src/cli.rs     │
  ╰──────────────────────────╯  ╎│╰──────────────────────────╯  ╎│╰──────────────────────────╯
                                ╎│╭──────────────────────────╮  ╎│
                                ╎││ ✓ conventions         ⚙9 │  ╎│
                                ╎▸│ @explorer · sonnet-5     │──╎┘
                                ╎ │ 7.6s · 8.8k              │  ╎
                                ╎ ╰──────────────────────────╯  ╎
                                ╎╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╎
  ⠻ apply · @coder · claude-opus-5
    running · attempt 2 · 18.4s · 9.1k tokens · 4 tool calls · needs plan, explore
    ⚙ fs.read src/cli.rs · 9ms · 6KB
    ⚙ fs.edit src/cli.rs · 12ms · 1 replaced
    ⚙ sys.run cargo test · 2.1s · 48 lines
  3/8 done · 1 running · 21.3k tokens · 24.6s
```

Three things to read off that.

**The shape.** `explore` and `conventions` share a column because they run at the same
time; `apply` is to their right because it waits for both. Depth is horizontal position
and nothing else — it used to be reading order, so two cards side by side meant only that
they were declared next to each other. The ranking and the crossing reduction are the
same [layered-graph](https://www.yworks.com/pages/layered-graph-layout) passes the
diagram renderer uses, so `@flow graph <name>` and the board agree about the shape.

**What is not drawn.** An edge the graph already implies is left off. If `d` needs `a`,
`b` and `c` where `b` and `c` both need `a`, the direct `a → d` says nothing new, and
drawing it puts three arrows into one card where one carries the meaning. The dependency
is untouched — the scheduler still honours it; what is dropped is saying it twice. A
`goto` travels in the band under the whole board and turns in the gaps, so a loop never
runs through the cards it loops over.

**The slowest path** on the header line is the chain that decides the wall clock. On a
graph that overlaps work that is *not* the slowest node — a slow node with three fast ones
beside it costs nothing extra — and it cannot be read off the picture, so it is stated.

Under the cards, a **pane follows whichever node is working** — and when something breaks,
**the one that broke**: its agent, model, state, attempt, elapsed, cost, what it needs, and
the last few tool calls it made. A card is three lines and has to hold a name; these are
the questions asked of a run that is going wrong, which is the only time anybody watches a
board closely. There is no selection because the board does not read your keyboard — for
the whole story of one node, `@flow node <id>` and `@flow log <id>` are still the commands.

**A run that goes wrong says so, on the card and in the tally.** A settled failure's third
line is *why* it stopped rather than what it cost — the cost of a failure is the least
interesting thing about it. A node that could never run because something it needed failed
is drawn `⊘ blocked`, in amber, distinct from the `·` of a node its own condition ruled
out; nothing is left reading "waiting" on a run that has finished. And the tally names
every state, not only the two that are going well:

```text
  0/5 done · 1 failed · 2 blocked · 2 skipped · 30.0k tokens · 11.5s
```

The footer under the board then names the first node that failed and the first line of
what it said, because by the time you read a footer the board has usually scrolled.

**An edge takes the colour of the node it leaves once that node has settled**, so the path
that actually ran lights up behind the board and stops exactly where the run did — in the
theme's own green, red and amber, alongside the accent the **running card pulses in**.

**Two views.** `[flow] view = "list"` — or `--view list` for one command — puts every
node back on a single dense row in file order. It is the shortest board that can exist,
which is what a twenty-node flow in a six-line split wants. The graph view **hands over
to it by itself** when the cards will not fit the window, in *either* direction: depth
costs width now, so a nine-deep flow asks for more columns than a terminal has. A picture
drawn past the edge is worse than no picture.

Off a terminal — `--bg`, a pipe, CI — neither view applies: the same state machine
prints `[node] event` lines instead. Nothing is overwritten, and the attribution a
plain stream could never give is still there.

**Your keyboard is left alone.** While the board owns a region of the screen it asks the
terminal to stop echoing, because the board repaints by climbing back over the block it
drew and an echoed keystroke moves the cursor out from under it — which used to strand a
copy of the board on screen for every Enter pressed during a run. Ctrl-C still works, and
an `approve` node gets echo and the cursor back for as long as its question is on screen.

### Node control

`show` is the whole run and `log` is one node's text. Between them sits the question
people actually ask when an answer looks wrong — *what is this node*:

```text
❯ @flow nodes last
✗ 900-1 failed · flow 'build'
  ✓ plan         done      @planner · claude-sonnet-5 · 4.2s · 9000 tokens · 3 tool call(s)
  ✓ explore      done      @explorer · claude-sonnet-5 · 8.1s · 6900 tokens · 12 tool call(s)
  ✗ apply        failed    @coder · claude-opus-5 · ×2 · 12.3s · 4 tool call(s)
  ⊘ verify       blocked
  · review       skipped

  left to do apply, fix, summary
```

`@flow node last apply` opens one of them in full — which agent, which model, how many
attempts, its edges and its condition, then what it was asked and what it answered,
rendered as the Markdown it is. Which model served a node is a fact the record has to
keep: a pool that picks per run cannot be read backwards for it.

`@flow watch [<id>]` **attaches** to a run that is still going, from any pane. Every
node's result is written the moment it lands, so the board it paints is the same board
the running process is painting — including for a `--bg` run that has no terminal of its
own. With no id it attaches to the newest run that is *still going*, which is what you
meant; **Ctrl-C detaches and leaves the run alone**, and the board says so while you
watch. `@flow … --bg` prints the attach command when it detaches, and `@flow runs` lists
what is still running first, so a terminal attached to nothing still shows you the way
back.

`@flow retry [<id>] <node>` runs one node again **and everything built on it**, and
prints that set before anything starts. The cascade is the point: re-running `apply`
while `verify` keeps the answer it derived from the old one is not a retry, it is a
record that contradicts itself, where a downstream `{{apply.output}}` names text that
no longer exists. What came *before* is exactly what a resume keeps.

`examples/ai/flow.toml` is a commented tour of every field, including `run` and
`approve` nodes, which the bundled five deliberately leave out — a default that shells
out is a default bound to somebody's toolchain.

## `@loop` — iterate until it verifies

Loop engineering in one line: don't perfect a single prompt — design the loop the agent runs
inside, with a **verifiable** goal and hard bounds.

What it needs is a command whose **exit status** decides — nothing more. That does not have
to be a test suite: `--check "test $(wc -w < intro.md) -lt 200"` holds a piece of writing
under a word count, and `--agent writer` makes the thing doing the cutting a writer rather
than a coder.

```text
❯ @loop "make the config tests pass"
🔁 loop 'coder' — up to 5 iteration(s)
  verifier: cargo test -p framework config:: — proposed from the goal
▶ iteration 1/5 … ▶ iteration 2/5 …
✓ goal reached after 2 iteration(s)
```

**The verifier is the whole game.** `--check "<cmd>"` is a binary stop condition: exit 0 = done,
no judgement involved. If you don't give one, the AI reads the goal **once** and proposes a real
command — because the alternative, a model deciding for itself whether it is finished, is the
single most common way agent loops fail. The proposal is printed before anything runs, and the
command guard still adjudicates it: a "verifier" that would deploy, push or install is a side
effect, not a measurement, and it is refused. Only if nothing verifiable turns up does an
independent `reviewer` agent grade each iteration (`VERDICT: PASS` / `VERDICT: CONTINUE`).
`--no-check` asks for that reviewer split on purpose.

**It is proven before it costs anything.** The check runs once *before* iteration 1:

| Pre-flight | What happens |
| --- | --- |
| the guard refuses it, or it can't run at all | exit 2 — nothing spent |
| it already exits 0 | `✓ the goal is already met` — exit 0, zero iterations |
| it fails | that failure seeds iteration 1, so the maker starts on the real error |

**Bounds, all three of them.** Iterations, tokens and wall clock are three different ways for a
loop to run away, so all three are capped: `--max N` (default 5) · `--budget TOKENS` ·
`--timeout 30m`. A value that can't be read is an error, never a silent default — a bound you
asked for and didn't get is worse than no bound at all.

**No progress is detected, not endured.** The loop remembers its last few verifier
observations, so a run that repeats itself *and* one that oscillates between two bad states are
both caught. The first time it happens the maker gets **one** more iteration — told what has
already been tried and asked for a materially different approach. A second time ends the run.

**Every iteration is written down** under `~/.aiTerminal/ai/loops/<id>/`: `loop.toml` (goal,
verifier, bounds, progress) and `iterations/<n>.md` (what the maker did, what the verifier saw).
So a run is readable, reviewable and continuable:

```text
❯ @loop                              # recent runs: outcome · verifier · iterations
❯ @loop show last                    # goal, verifier, bounds, what was tried
❯ @loop log 4310 -f                  # the newest iteration, followed live
❯ @loop resume 4310 --budget 200000  # carry on with what's left — or more rope
❯ @loop clear                        # prune finished runs
```

`@loop log` draws each iteration as the document the maker wrote — and quotes the verifier's
own output verbatim inside it, because a compiler's caret belongs in its column. Piped, you
get the file.

A run stopped by Ctrl+C, a timeout or the cap resumes from where it stopped rather than paying
for the whole thing again.

```text
❯ @loop "fix the flaky auth tests" --check "npm test -- auth" --max 8 --timeout 20m --bg
❯ @loop "bump tokio and fix what breaks" --check "cargo test --workspace"
❯ @loop "make clippy clean" --check "cargo clippy -- -D warnings" && git push
❯ @loop "…" --dry-run                # the plan and the proven verifier, nothing run
```

`--agent <name>` picks the maker (default `coder`); `--bg` detaches the whole loop as a tracked
job (`@job` + `@job log`).

Exit codes: `0` goal reached · `1` a bound stopped it (stalled/exhausted/budget/timeout) ·
`2` setup error · `130` interrupted — so loops compose with shell logic and CI.

See [examples/ai/loop.md](../examples/ai/loop.md) for recipes.

## `@job` — say what to do and when

Write the request the way you'd say it. The AI reads *when* out of the sentence, and a
plain scheduler owns everything after that:

```text
❯ @job "check the logs at midnight"
⧖ every day at 00:00 — check the logs · job 1753112100-4310
  fires in 7h · list: @job · cancel: @job cancel 1753112100-4310

❯ @job "summarize the latest kafka logs into ~/reports/kafka.md every hour"
❯ @job "run ./backup.sh every weekday at 6pm"      # a COMMAND job — needs no model to run
❯ @job "remind me to stretch in 20 minutes"        # one-shot
❯ @job "deploy on the 1st of each month at 3am"

❯ @job "post the weekly update every monday morning" --dry-run
cron 0 9 * * 1 — post the weekly update
  first run in 5d                                  # …and nothing was scheduled
```

The model is asked **once, at creation**. Its answer — a cron expression, an interval, or a
single moment — is written into the record, so occurrence #47 of an hourly job costs nothing
and behaves exactly like #1. The sentence it understood is printed before you accept it, and
`--dry-run` shows it without scheduling anything.

**The request, quoted or not.** One quoted argument is taken **verbatim** — spacing,
newlines, and a `--bg` *inside* the quotes stays text. Loose words are joined with single
spaces, so `@job summarize the logs --bg` also works.

**When you already know the schedule**, say so and no model is consulted at all:

```text
❯ @job --every 15m -- ./sync.sh          # interval
❯ @job --cron "0 9 * * 1-5" -- ./standup.sh
❯ @job --at 17:30 -- ./eod.sh            # today (or tomorrow) at that clock time
❯ @job --in 2m "draft the release notes" # an agent task, two minutes from now
❯ @job -- "ls | wc -l"                   # ONE quoted word after -- = a shell line
❯ @job -- sh -c "echo hi"                # several words = argv, executed as typed
```

`--` also means AI is never needed: a command job runs with no model configured. Commands
go through the same **command guard** as everything else, and because a detached job has
nobody to answer a prompt, "ask first" is a refusal (exit `2`, recorded in the log).

**Watching them:**

```text
❯ @job                          # list: next fire, last outcome, run count
background jobs (3):
  ⧖ 1753112100-4310 scheduled check the logs                (fires in 7h)
      cron 0 0 * * * · 12 run(s) · last ok
  ▶ 1753112000-4242 running   audit the deps … --agent reviewer
  ✓ 1753111800-4101 done      create a CHANGELOG …
❯ @job show last                # the full record: plan, schedule, folder, next fire
❯ @job log 4310 -f              # the newest run, followed live (id, prefix, or `last`)
❯ @job cancel 4310              # stop all future occurrences
❯ @job clear                    # prune finished jobs — never a live or recurring one
```

**It survives the machine.** The sleeper that waits for a fire-time is a detached process,
so a reboot takes it with it. On the next launch (and on any `@job`) the supervisor re-arms
anything still ahead and runs anything overdue **once** — an hourly job that missed six hours
runs once, not six times. A one-shot whose moment passed with nothing watching is `missed`,
honestly. `[jobs] max_concurrent` bounds how much a due fleet can start at a time.

Foreground `@job` runs play with the full live chrome *and* tee their output into the run
log. `--bg` detaches; it also works on any `@<agent>`, `@flow`, or `@loop` invocation.

`@job log` knows which of the two it is holding: an **agent's** log is drawn as the document
it is, and a **command's** output is passed through untouched — a `#` line printed by a
program is a comment, not a heading. Piped, both give you the file.

Each job is a folder under `~/.aiTerminal/ai/jobs/<id>/` — `job.toml` (what to run, when,
and how the last run went) and `runs/<n>.md`, one log per occurrence. Plain TOML you can
read, edit, or delete; deleting the folder is a valid way to cancel. Logs are pruned to
`[jobs] keep_runs` and each is capped at `[jobs] max_log_bytes`, so an hourly job can't fill
the disk.

A job's status is always honest: `scheduled` (waiting to fire) · `running` · `done` ·
`failed` · `cancelled` (Ctrl+C or `@job cancel`) · `died` (the process vanished — crash,
kill, reboot) · `missed` (its moment passed with nothing watching). The list detects a dead
pid and heals the record on the spot.

With **no model configured**, `@job` still reads `in 5m`, `at 17:30`, `every hour` and
`every day at 9am` itself — the planner is an upgrade, never a dependency.

## `@workspace` — the folder as a conversation

The workspace is a **native surface of the app itself** — drawn by the same
engine that draws every pane, not an ANSI TUI fighting the terminal from inside
one. Press **⌘J** (action `workspace`, rebindable), or type `@workspace` in any
pane: the CLI stages a private OSC the host answers by opening the surface over
that pane's folder. It opens on its home screen — the mark, the folder, what the
overlay adds, the pool and its strategy — with the input panel centered beneath.
⌘J (or `Esc` on an empty idle panel) puts your terminal back exactly as it was; the
sitting keeps running, and the same key returns to the conversation where it
stood. A model is NOT required to open: browsing, `/help`, `!` and `/mcp` all
work; only a prompt answers with the setup hint. (Outside aiTerminal,
`ai workspace` says where the feature lives and exits clean.)

Ask, follow up, and the conversation remembers — one transcript carries the whole sitting, compacted
automatically when the window demands it. Every answer renders live as Markdown with
native diagrams, every model turn draws from your configured pool under its strategy,
and the product's whole `@` language works mid-conversation.

### Input, first match wins

| You type | What happens |
| --- | --- |
| `/command` | a slash command (below) — plus every prompt file as `/<name>` |
| `!cmd` | ONE shell command, judged by the guard, its output shown and folded into the next turn |
| `@flow …` / `@job …` / `@loop …` / `@agent` / `@mcp` | the real command, run inline — its output streams into the conversation between dim rules, Esc stops it, and its exit feeds the model's next turn |
| `@<agent> task` | one run of that agent (project overlay first), its answer folded in |
| `@<path>` | attach a file — images and PDFs ride the turn as real media; the band completes project files (`explain @src/ma` → `@src/main.rs`), even mid-sentence |
| anything else | a conversation turn |

Slash surface: `/help` `/init` `/clear` `/compact` `/model` `/agent` `/agents` `/mcp`
`/memory` `/cost` `/readonly` `/status` `/retry` `/save` `/files` `/skills` `/keys`
`/trust` `/sessions` `/resume [n]` `/undo` `/redo` `/export [path]` `/thinking`
`/learn` `/changes` `/exit`. `/learn` asks the sitting to distill itself — a
reusable method becomes a skill file under `.aiTerminal/skills/`, durable facts
become memories (both through the guarded tools); `/changes` shows what the
sitting changed (`git status` + `diff --stat`, judged like any `!` command). `/sessions` lists this folder's conversations and `/resume <n>` folds the
one you pick into your next message; `/undo` takes the last exchange out of the
conversation (one `/redo` restores it); `/export` writes the whole redacted
conversation through the guarded write path; `/thinking` toggles reasoning
display for the sitting. A partly-typed command with the band open submits the
**highlighted** match — Enter selects, so `/st⏎` is `/status`. `/readonly` is plan
mode: the toolset narrows to the safe (read-only) set. `/resume` folds the folder's
last conversation back in.

### The chrome

A fresh open is a **home screen**: the mark, the folder and what its overlay
adds, centered, with the **input panel** floating beneath — your first message
anchors the conversation and pins the panel to the bottom for the sitting.

The panel is ONE shape for every state: a left accent bar whose color states the
mode (accent = build, amber = plan and the guard's ask), an elevated surface,
your draft as real lines with a true caret (it grows with Shift+Enter up to a
third of the surface), and a **meta row inside the panel's bottom** — root ·
build/plan · persona · serving model on the left, tokens · cost · overlay dot on
the right. While a turn runs the meta row carries the spinner and the muse's
aside (`esc interrupts · enter steers`) and anything you type becomes the
**draft** of your next message; a guard `confirm` turns the panel amber and asks
in place.

Typing `/` or `@` opens the **completion popup** floating above the panel
(fuzzy-ranked: prefix matches first, then subsequences, and what this folder
actually uses rises — frecency; ↑/↓ select, Tab accepts, Enter runs the
highlighted match; an `@` token completes commands, agents AND project files,
mid-sentence included), and a streaming answer's tail rides a floating card
there too. Both are overlays: **the layout is deterministic** —
nothing but a window resize or a growing draft can move the conversation. Tool
moments are marked by kind, flow-style: `⚙` native · `⌁` MCP · `✧` delegate ·
`◆` memory — and an inline `@flow`/`@job`/`@loop` run is embedded between dim
rules, its output streaming into the conversation line by line (in the app it
runs as a child of our own binary with stdin closed, so a guard `confirm`
refuses exactly as headless runs do, and Esc kills the run rather than orphaning
it).

| Key | Does |
| --- | --- |
| `Enter` | send · `Ctrl+J` / `Shift+Enter` newline (↑/↓ walk the rows) |
| `Tab` | complete `/` and `@` · accept the dropdown selection |
| `Shift+Tab` | toggle plan/build |
| `↑` / `↓` | history (or dropdown / draft rows) |
| `Esc` | close the band · clear the line · **interrupt a running turn** · on an empty idle bar, close the surface |
| `Enter` mid-run | **steer**: your note joins the run at its next step, and the MODEL decides — pivot now, or finish the current step first |
| `Ctrl+A/E/B/F/W/U/K` | emacs-style line editing |
| `⌘C` / `⌘V` | copy the mouse selection out of the conversation · paste text — or a clipboard **image**, attached as a `<#image_N>` token you can move or delete |
| `Ctrl+C` | clear; twice on an empty line leaves · `Ctrl+D` on empty leaves |

The surface is drawn by **the app's own engine** — the same pixel surface, glyph
cache and VT engine that draw every pane. It occupies the panes area only: the
app's own bottom bar (folder · cpu · memory) and the tab strip stay visible
around it, even under the trust modal. The conversation lives in a headless
terminal of its own with real scrollback (wheel and PageUp/PageDown), and every
answer goes through the product's one markdown engine, laid out at the surface's
REAL width — with **mermaid diagrams and images composited natively** (the
surface declares itself native to the renderer; no environment sniffing), inline
runs included, and every picture surviving any resize because the conversation
rebuilds from its model. Underneath,
one state machine folds every key and every streamed line from one queue into one
model (the single-model discipline opencode's Bubble Tea foundation uses) — and
because the app owns every pixel, there is no second painter left to race.
`/exit` (or Ctrl+D, or Ctrl+C twice on an empty line) ends the sitting and the
surface closes with it; the next ⌘J opens a fresh one.

### The project overlay

A repo can carry its own AI setup, exactly like the global one:

```text
<root>/aiTerminal.md         project instructions (AGENTS.md read when absent)
<root>/.aiTerminal/
  agents/  skills/  prompts/  flows/  mcp/     overlay the global dirs, project-first
  config.toml                [ai] + bounds override; [guard] rules TIGHTEN only
```

First-per-name wins: a project `coder.md` shadows the global coder for this folder.
`/init` surveys the project (read-only tools) and writes its `aiTerminal.md`.

### Trust, and the guard

The FIRST open of a folder asks — as a **native modal**, the same pattern as the
close confirmation: the question and exactly what the project would inject
(agents, skills, prompts, flows, **MCP servers — these run code as you**, config),
two buttons with the safe one — *Global only* — holding focus, ←/→/Tab to move,
Enter to choose, Esc (or a click on the backdrop) declining safely. The answer is
remembered per folder; it is asked again only when the parts that execute change
(a `git pull` that adds an MCP server re-opens the question). Declining opens the
workspace on global config alone; `/trust` re-opens the question.

The model can also **ask you** — the `ask.user` tool puts its question in the
conversation and turns the panel into an answer box (Enter answers, Esc
declines; headless runs refuse, exactly like an unanswered confirm) — and when
it works through a multi-step plan with the `todo.*` tools, the checklist
renders live in the conversation as it ticks.

And the workspace **learns**: this folder's past conversations are searchable
by the model itself (`memory.sessions`, bounded to the newest sittings), every
first request carries a compact map of the project's shape, and once per
sitting a nudge reminds the model to persist what proved durable — facts as
memories, methods as skills. What one sitting learns, the next one has.

The guard owns the boundary end to end — EVERY workspace path crosses it. Model
tool calls run through the one pipeline (`Act::Read`/`Write`/`Run` judged, every
result redacted on egress, MCP included); `!` and `/changes` are judged commands
with masked output; `/save`, `/export` and `/learn`-written skills go through the
guarded, workspace-confined write; **`@path` attachments ask the guard's path
rules before a byte is read — a denied file never enters a request, even typed
by your own hand**; turn text, grounding, steer notes and `ask.user` answers are
hidden before a model sees them; memories are scrubbed on write; the chat log is
redacted at rest (and `/export` and `memory.sessions` read only that). Inline
runs rebuild the same guard in the same root, with fixed argv and no shell.

What workspace mode ADDS is a human: a `confirm`-tier rule, spent as a refusal
in headless runs, here pauses the stream and asks you, once, for that act — and
ONLY the local keyboard can answer; no steered or remote text ever approves.
A project's `[guard]` rules can only tighten (deny/confirm/read-only/secret);
allow-tier rules from a repo are dropped and named. And the guard is
consultable: `/guard` shows your active protections, `/guard <command>` (or
`/guard read <path>`, `/guard write <path>`) answers with the guard's own
verdict — the question, never the act.

## Exit codes & scripting

Every AI command tells the shell the truth, so `$?`, `&&`, and CI compose:

| code | meaning |
|------|---------|
| `0`  | the run completed (for `@loop`: the goal verified) |
| `1`  | the run failed — model/transport error, step limit, tool stall, **blocked by the guard**; loop stalled/exhausted/out of budget |
| `2`  | setup error — unknown agent/flow, AI not configured, guard-blocked check |
| `130`| interrupted — Ctrl+C cancelled the run cleanly |

A run ends `refused` (⛔) when the guard refused three things in a row and nothing was
achieved in between: nothing broke and nothing ran out — the machine said no, and trying
again would only be refused again. A flow node that ends this way is not retried, and the
answer says what the run needed. See [security.md](security.md#when-a-run-cannot-go-on).

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
⠹ thinking…                                          ← animated while the model reasons
  ⚙ fs.search   "parse_flow" · 18ms · 6 results
  ⚙ fs.edit     src/flow/parse.rs · 6ms · 1 replaced
  ⋯ sys.run     cargo test --workspace              ← still going, replaced when it lands
The fix: the parser dropped the …                    ← the answer, streaming
✓ 8.4s · 2 tools · 12.3k in / 1.8k out · ~$0.014
```

**One thing writes to that region.** The answer repaints in place as it streams while the
tool trace has to stay where it was printed, and those used to be two writers on two
streams that knew nothing about each other: the next repaint climbed back over lines it
had never painted and ate them, so a trace came out cut in half. Everything — answer,
trace, notes, a loop's iteration headers — now goes through one sink that takes the live
tail off, writes the line, and puts the tail back. `@agent`, `@loop` and a foreground
`@job` all draw through it, so all three look alike; a tracked `@job` keeps a copy of the
text in its log, and only the text, never a repaint frame.

A call still running after a moment says so, and its line is replaced when it lands — a
forty-second `cargo test` used to print nothing at all until it was over, which on screen
is indistinguishable from a hang.

The `@tool` protocol itself is never shown. That is not a new rule, but it was only ever
enforced on the raw path: on a terminal, where the answer renders as Markdown, the filter
was skipped entirely and every tool call the model wrote was printed to the user verbatim.

A tool line says **what the call was acting on**, not the argument JSON it arrived as.
`fs.read {"path":"crates/framework/src/cli/runn` is the wire format truncated at a fixed
width — unreadable at a glance, and it reliably cuts the one part you were reading. So
the subject is picked out and a long one is elided in the *middle*, because a path's last
component is what you were looking for. What came back is read the same way, off the
JSON's shape: an array is a count of results, a listing counts its entries, multi-line
output counts its lines, and anything else is how much of it there was.

Nothing in that knows a tool by name. A table of per-tool formatters would be a second
registry to keep in step with `caps`, and wrong the day an MCP server exposes a tool this
build has never heard of — which still gets a readable line, because arguments have names
whatever the tool is.

### A wait says what it is for

Most of what a command does is instant — a job's record, its arming, its spawn are all
under a millisecond. What is not instant is a **model call**, and there are four made
outside a run's own loop: the planner reading a `@job` request, the verifier proposal
that opens a `@loop`, the graph built for a `@flow` goal, and a run folding its own
history when the window fills. Every one of them used to happen with nothing on screen —
`@job "summarise the logs every morning"` sat on a dead terminal until the model answered.

They all say what they are for now:

```text
❯ @job summarise the logs every morning
⠹ reading when to run this…
⏳ every day at 09:00 — summarise the logs · job 1785371201-90257
  fires in 14h · list: @job · cancel: @job cancel 1785371201-90257
```

Not "thinking": these happen before a run exists, and *why* you are waiting is the useful
part — `reading when to run this` tells you a schedule is being worked out, which is
exactly what a bare spinner cannot.

The spinner **holds its first frame for a moment**, so work that finishes at once draws
nothing at all. That is what makes it safe to wrap a call unconditionally: `@job -- echo
hi` and `@job --every 15m …` never consult a model and show nothing new, without anybody
having to predict in advance which paths will be slow.

**`@job` also says what it understood** — but only when that differs from what you typed.
The planner does not just pick a schedule: it strips the timing words out of the task, and
may turn a sentence into a shell command. That rewrite changes what the job *is*, and for
an immediate job it was never shown. An echo of your own sentence is noise, so there is
none. And a planner that was **asked and could not answer** says so rather than falling
silently back to the word parser — you waited for that call.

### Something to read while you wait

A run waiting on a model shows a spinner and nothing else, sometimes for a while. One dim
line keeps you company:

```text
⠹ thinking…  ·  @flow retry <node> re-runs it and everything built on it
⠹ thinking…  ·  a prompt prefix the provider already cached costs about a tenth
⠹ thinking…  ·  "Simplicity is prerequisite for reliability" — Dijkstra
```

Three things make it company rather than noise, and all three are structural rather than
a matter of taste:

- **It costs no rows.** It rides inside the spinner's own line, or one constant row under
  a flow board. Nothing scrolls, nothing accumulates, nothing survives the run.
- **It only appears while nothing else is happening.** Silent until a wait has lasted
  `after` (6s), and gone the instant the answer starts — a run that answers in three
  seconds never shows one at all.
- **It cannot reach anything but a screen.** The spinner is TTY-only and the board row is
  drawn only for a live board, so a pipe, a `--bg` job, a job log, a flow record and CI
  never see one.

The lines are written **by the model**, once, into `cache/motivation.toml` and reused —
in the background, so no run ever waits for them. Tips are drawn from this tool's own
command list rather than imagined, which is what stops a "tip" teaching you a flag that
does not exist. With no model configured there is no cache and the feature is simply
absent; there is no canned fallback, because a stock line pretending to be a fresh one is
worse than a plain spinner.

`[motivation]` in the config turns it off, picks which kinds to draw from, and sets both
timings — see [configuration.md](configuration.md#configtoml-sections).

A **`@flow`** shows a live board — the graph, drawn as cards and repainted in place (see
[Watching it run](#watching-it-run)); a **`@loop`** shows each iteration and a footer
with the iteration count:

```text
❯ @flow build "add a --json flag to the export command"
▸ build · add a --json flag to the export command
  8 nodes · 6 agents · 69 tools · 24 skills · 4 at a time
  ╭──────────────────────╮    ╭──────────────────────╮
  │ ✓ plan            ⚙3 │    │ ⠻ apply           ⚙4 │
  │ @planner             │───▸│ @coder               │
  │ 4.2s · 3.1k          │    │ fs.edit src/cli.rs   │
  ╰──────────────────────╯    ╰──────────────────────╯
  1/8 done · 1 running · 12.5k tokens · 24.6s
```

- **Answers** render as styled **Markdown** in **realtime** — headings, **bold**/*italic*, `inline
  code`, links, bullet/numbered/task lists, block quotes, boxed fenced code, aligned GFM tables,
  and diagrams — by a from-scratch engine in `corelib::md` (width-aware wrapping, theme colors).
  Rendering is **continuous**: completed blocks commit once (they scroll away untouched) while the
  single in-progress block repaints in place as it streams, so the current line/paragraph styles in
  token-by-token — never a paragraph-at-a-time "pop". It fills the **split's full width** (the true
  pane size via `TIOCGWINSZ`, not a fixed 80 columns), so tables, code boxes, and diagrams span the
  whole pane and reflow on resize. `@ai` uses a tiny streamable contract — a one-line
  `RUN: <command>` proposes a command; anything else is a teacher-style answer that renders as it
  arrives. Piped/redirected output stays **raw Markdown** (clean for `>` and `|`). The `@tool …`
  machine protocol never reaches the display in any dialect (`@tool`, `<tool_call>`,
  `[TOOL_CALLS]`, `<|python_tag|>`, fenced) — the parser is model-agnostic.
- **Diagrams** — when a visual clarifies an idea, the AI includes one and it's **drawn natively
  in the pane** — **every mermaid diagram type**: flowcharts, sequence, class, state, ER,
  gantt, pie, journey, timeline, mindmap, kanban, git graphs, quadrant, requirement, C4,
  xychart, sankey, block, packet, radar, treemap and architecture. Parsed and laid out by
  `corelib::mermaid`, rasterized by the `gfx` engine, and composited over reserved grid rows
  (via a private `OSC 1338` placement in the terminal engine). **Piped or third-party
  terminals draw the same picture in Unicode box art** — only a diagram we genuinely cannot
  read falls back to a boxed source.
  The AI is prompted as a concise teacher and never exposes any formatting/diagram syntax.
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

### One fact, one memory

`memory.add` **reinforces instead of duplicating**. An agent re-learns the same thing
constantly — it reads a config, saves what it found, and does it again next run. When
what you are saving already exists (high token overlap with a stored note), the
existing note is reinforced (salience up, tags merged) and returned, rather than a
near-copy being written. Two files saying the same thing both ranked and both got
recalled, so the model paid twice to be told once.

### Links

A memory can name related ones, and **recall follows them one hop**:

```toml
---
kind  = "decision"
tags  = ["release", "ci"]
links = ["1785371201-ship-script"]
---
Deploys go through `make ship`, never a push to main.

The reason: [[1785371201-ship-script]]
```

Both forms work and are merged — `links = [...]` in the frontmatter, and `[[id]]`
written in the body — so editing a note by hand does not mean keeping two lists in
step. `memory.link {from, to}` relates two existing notes in both directions.

This is what lexical ranking structurally cannot do on its own: a decision ranks
because it shares words with your question, while the *reason* it was made usually
shares none. Following the relation retrieves it anyway.

### Ranking

Okapi BM25 over the note's body, tags and kind, re-ranked by **salience** (reinforced
each time a memory is recalled or re-learned), **recency** (decayed per day since it
was last touched), and an **exact-tag boost** — a tag is a deliberate act, while the
same word in a body may be an aside, and flat BM25 cannot tell them apart.

`memory.consolidate` still exists for duplicates that arrive another way: a file
written by hand, or notes that predate the check.

## MCP

A real Model Context Protocol client — protocol revision **2026-07-28**, with the
spec's own *dual-era* fallback for servers that still speak the older
`initialize`-handshake revisions (`2024-11-05` … `2025-11-25`). Nothing to
configure: the era is negotiated per server, automatically. A modern server is
probed with `server/discover` and spoken to statelessly (every request carries its
protocol version and capabilities); anything else gets the classic handshake, at
whatever revision the server answers with.

### Declaring a server

One file per server under `~/.aiTerminal/ai/mcp/<name>.toml` — the file stem is the
server's name (letters, digits, `-`, `_`; no dots, they belong to tool routing).
A seeded `example.toml.disabled` documents the format in place.

```toml
# a LOCAL server: spawned, spoken to over stdio
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/it/may/touch"]
[env]
API_TOKEN = "$MY_TOKEN"        # $VAR / ${VAR} resolve at launch time

# — or — a REMOTE server: a Streamable HTTP endpoint
url = "https://mcp.example.com/mcp"
[headers]
Authorization = "Bearer $MCP_KEY"

# either way
timeout_s = 30                 # per tool call (1–600)
```

`command` and `url` are exclusive — a file declaring both is ambiguous and treated
as no server at all. Remote auth is static headers on purpose; there is no
interactive OAuth flow, and stdio servers take credentials from `[env]`, which is
what the spec prescribes for that transport.

### What an agent sees

Every agent, loop, flow node and background agent job gets the union of the declared
servers' tools as `mcp.<server>.<tool>`, **with each tool's input schema** spliced
into its description — the model calls a tool with the arguments it actually takes,
rather than guessing names. Behaviour annotations ride along as `[read-only]` /
`[destructive]` hints. A server that declares the `resources` capability also grows
`mcp.<server>.resources.list` and `mcp.<server>.resources.read`, so its data surface
is browsable the same way. The catalogue is sorted, so the prompt prefix (and the
provider cache it feeds) never depends on which server answered first — and a flow
launches **one** hub for the whole run, shared by every node, not one per node.

Results come back whole: text, resource links, embedded resources, structured
output (`structuredContent`), described media — bounded at 256 KiB, exactly like a
chatty command.

### `ai mcp` (shell: `@mcp`)

Connects to every declared server exactly as a run would and reports each one:
transport, negotiated era and revision, the server's self-reported identity, tool
and resource counts — or, for one that failed, the reason and the tail of its
stderr, which is where a dying server writes why.

### Trust

A declaration is a trust decision. A local server runs as you; a remote one sees
every argument the model writes. The guard holds the same line here as everywhere:
outgoing arguments have their secret placeholders restored (a placeholder from
another run is refused, never forwarded as literal text), every result is redacted
before it re-enters the model, and tool descriptions — foreign text headed for a
system prompt — are sanitized and bounded on the way in.

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


## Context, budget & compaction

Every run measures its own context against the window of the model **serving that
run**, and gives context back before that model would refuse the turn. The window
comes from the model's own `context_window` in `~/.aiTerminal/ai/models/*.toml` — so
a 32k local model gets a 32k budget and a 1M model gets a 1M one, from the same code.

```toml
[ai]
context_window = 0      # 0 = use the serving model's own; >0 overrides it
compact_at     = 0.75   # fraction of the usable window that triggers compaction
```

`context_window` is for the case a model file cannot know about: a local model served
with a **smaller** window than its card claims. For a mixed pool, set it on the entry
rather than globally, so each member budgets against its own window:

```toml
[[ai.model]]
id             = "my-local-model"
context_window = 16000
```

The reply's reservation is capped at **half the window**. `context_window` is a setting
you type and `max_tokens` comes from a model file, so the two routinely disagree — and a
16k reply declared against an 8k window used to leave a quarter of the window for
everything else, which meant compacting on turn one and buying a summary on every turn
after.

### A big tool result never enters the transcript

A tool result does not cost its tokens once. It is stored, and the transcript is re-sent
on **every remaining turn** — so a 200 KB build log carried inline is paid for again and
again, and it crowds out the reasoning it was fetched to support.

So a result over **8 KB** is written to `~/.aiTerminal/cache/offload/<run>/` the moment
it arrives, and the model is handed a preview plus the path:

```text
   Compiling framework v0.0.0 (/work/crates/framework)
   …
[full output saved to …/cache/offload/1785-42/003-sys-run.txt] — 4000 lines, 214 887 bytes.
Read it with fs.read when you need more.
```

Lossless in the way that matters: `fs.read` is not workspace-confined (only writes are),
so the agent can pull any of it back when it turns out to matter. The threshold is
deliberately generous — every source file you read and every short command is under it.

### The ladder

When something still grows past the line — a long conversation rather than a large
result — compaction runs **cheapest rung first** and stops as soon as the transcript
fits.

| Rung | What it does | Costs |
| --- | --- | --- |
| **offload** | Any large tool result that slipped through is written out and replaced by a preview plus its path. | nothing — no model call |
| **summarize** | The oldest turns fold into one `## Earlier work (compacted)` block, sized from the budget so the result actually fits. | one model call |

Most runs now finish **without the ladder running at all** — results never enter the
transcript at full size in the first place, and the cheapest compaction is the one that
does not have to happen. A run that does compact says so; it never shrinks its own
history silently.

### Paying for the prompt once

A turn re-sends everything before it: the agent's system prompt — its instructions, its
skills, its whole tool catalogue — and the entire conversation so far. Left unmarked,
a twelve-step run pays full price for that prompt twelve times.

So every turn **declares what is settled**: the system block is fixed for the run, and
every message but the newest has already been sent. That is a fact about the
conversation rather than a vendor feature, so it rides on the neutral request and each
provider decides what to do with it — Anthropic gets two `cache_control` breakpoints
(one static, one rolling), and OpenAI-compatible endpoints cache a matching prefix by
themselves and need only that we do not disturb the order.

The saving is a number, not a claim:

```text
✓ 8.4s · 3 tools · 12.3k in / 1.8k out (11.1k cached, 90%) · ~$0.004
```

The first turn of a run writes the cache; every turn after reads it, and the share
climbs as the run goes on. **A run showing no cached share is the signal that
something in the prompt has stopped being stable** — a tool list that changed order, a
timestamp somebody added. The transcript is append-only and the MCP tool catalogue is
sorted for exactly this reason, and both are held in place by tests.

### `ctx.*` — an agent managing its own context

Two tools every agent has, answered by the loop itself rather than by a capability
(they read and rewrite the run's transcript, which is loop state). An agent does not
declare them, and they never reach the tool runner.

| Tool | Does |
| --- | --- |
| `ctx.status {}` | `{used, window, usable, pct, turns}` — check before a big read |
| `ctx.compact {"keep": "…"}` | run the ladder now; `keep` names what must survive |

**`@ai` has no transcript** — it is a single turn — so it gets no `ctx.*` tool. What it
gets is the budget: its grounding preamble is trimmed to fit before egress, dropping
whole blocks from the least valuable end (terminal snapshot, then session digest, then
recalled memory). Your standing instructions and the files you attached are the last
things to go, and a trim is announced rather than silent.

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
- Every egress path goes through the **AI guard** (config + the `ai-guard` plugin):
  keys, tokens and secrets leave as placeholders like `«aws-key-1»` and become
  themselves again the moment the text returns to this machine — so a run can use a
  key it was never shown. See [security.md](security.md).
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
| `memory` | search, get, add, update, forget, link, recall, list, consolidate, stats | safe/write |
| `data` / `queue` / `store` | structured tables, queues, KV — an agent's sandboxed scratch database | write |
| `todo` | set, add, done, list, clear — the live plan | safe |
| `ctx` | status, compact — the run's own context (answered by the loop, granted to every agent) | safe |
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
aiTerminal ai --bg …                         # detach any of the above    (--bg)
aiTerminal ai loop "<goal>"                  # iterate to a verified goal (@loop)
                 [--check "<cmd>" | --no-check] [--agent <name>]
                 [--max N] [--budget TOKENS] [--timeout 30m] [--bg] [--dry-run]
aiTerminal ai loop [show|log|resume] <id>    # one run: record / output / carry on
aiTerminal ai loop [clear]                   # loop list / prune
aiTerminal ai job "<request>"                # a job, scheduled by the AI  (@job)
                 [--every 15m | --cron "…" | --at 17:30 | --in 2m]
                 [--agent <name>] [--bg] [--dry-run]
aiTerminal ai job -- <command>               # a command job (no model needed)
aiTerminal ai job [log|show|cancel] <id>     # one job: output / record / stop
aiTerminal ai job [clear]                    # job list / prune
aiTerminal ai agent [<name>]                 # the agents you have        (@agent)
aiTerminal ai flow <name> "<input>"          # run a workflow graph       (@flow)
aiTerminal ai flow "<goal>"                  # …or let the model route the goal
                 [--timeout 30m] [--budget TOKENS] [--concurrency N]
                 [--bg] [--dry-run]
aiTerminal ai flow check [<name>]            # verify a flow (or all) — no model needed
aiTerminal ai flow graph <name>              # draw the graph
aiTerminal ai flow runs                      # past runs
aiTerminal ai flow [show|resume] <id>        # one run: the record / carry on
aiTerminal ai flow log <id> [<node>] [-f]    # a node's output
aiTerminal ai flow [clear]                   # flow list / prune runs
```
