# `@gate` — drive your terminal from a chat app

`@gate telegram start` hands a tab or split to a chat. Your shell keeps running right
there — you carry on typing — and a **paired** chat drives *the same shell*: same
working directory, same history, same running program. Ask it to run something, send a
keystroke, or pull a screenshot when text isn't enough.

Every remote action prints a dim line in the pane before it happens, so nothing anyone
does from the chat is invisible to whoever is at the keyboard.

```
❯ @gate telegram start

  ⬤ telegram gate live · @mourad_term_bot
  pair from the chat: /pair 418-207   (nothing runs until you do)
  this pane is a shell you share with the chat · `exit` or `@gate stop` ends it

❯ ls                          ← you, typing normally
README.md  crates  docs

  ▸ Mourad: cargo build       ← arrived from the chat
❯ cargo build
   Compiling aiTerminal…
  ◂ sent 12 lines

❯ █                           ← your prompt, right where you left it
```

> **Read the [security model](#security-model) before you enable this.** A gate is
> remote code execution over a chat app. It ships off, and it stays off until you turn
> it on.

## Setup

**1 — Create a bot.** Message [@BotFather](https://t.me/BotFather) on Telegram and send
`/newbot`. It asks for a name, then replies with a token like `123456:AAH…`.

**2 — Keep the token out of your config file.**

```sh
export TELEGRAM_BOT_TOKEN='123456:AAH…'     # add it to your shell profile
```

**3 — Turn gates on** in `~/.aiTerminal/config.toml`:

```toml
[gates]
enabled = true

[gates.telegram]
token = "$TELEGRAM_BOT_TOKEN"
```

**4 — Start it**, in any tab or split:

```
❯ @gate telegram start
```

**5 — Pair.** Open a chat with your bot and send the six-digit code shown in the pane:

```
/pair 418-207
```

That's it. Send `git status` and it runs.

## Using it

In a paired chat, **a plain message is a command** — send `git status`, get the output
back in a code block with its exit status and how long it took. Set
`[gates] plain_text = "ignore"` if you would rather type `/run` every time.

| Command | What it does |
| --- | --- |
| `<anything>` | run it in the shared shell (subject to the command guard) |
| `/run <cmd>` | the same, explicitly |
| `/sh <cmd>` | run it **out-of-band**, in its own shell — works even while `vim` owns the shared one |
| `/shot` | a PNG of the terminal, exactly as it looks |
| `/key <name>` | press a key: `enter tab esc up down left right home end pgup pgdn ctrl-c ctrl-d ctrl-l q` — or any single character |
| `/keys <text>` | type text *without* pressing enter |
| `/cancel` | Ctrl-C the running command |
| `/ai <prompt>` | ask this terminal's own AI (`@ai`), from the chat |
| `/full` | resend the last output as a file, untruncated |
| `/status` | what the gate and the shell are doing |
| `/help` | the list above |
| `/stop` | end the gate |

The same list is published to Telegram, so the chat's own `/` menu offers it.

**When it's a full-screen program.** Open `htop` or `vim` and text capture stops
meaning anything, so the gate answers with a **screenshot** instead, and a plain
message becomes keystrokes rather than a command. `/key enter` submits, `/shot` gets a
fresh frame.

**When something is waiting for input.** If a command goes quiet — `sudo` asking for a
password, an `ssh` host prompt — the gate sends you what it has so far, and your next
message goes to that command's **stdin** rather than starting a new one.

**When something takes a while.** A long build sends progress notes (at two minutes,
then less often) and always finishes with its real exit status. It is never abandoned
part-way.

**When you're both typing at once.** If you have a half-finished line at the prompt, a
command from the chat **waits** rather than being spliced into it — otherwise
`git comm` plus `ls -la` would run `git commls -la`. The chat is told it's queued, and
it runs the moment the line is clear. Your typing is never cleared to make room.

## Stopping

Any of these:

- type `exit` in the pane (it *is* a shell)
- `@gate stop` — from that pane or any other
- `/stop` from the chat
- `[gates] idle_timeout_minutes` to stop automatically after a quiet period

Stopping always restores the terminal: raw mode off, cursor back, mouse reporting off,
and out of the alternate screen if a program left it there.

`@gate status` lists what is running.

## Configuration

```toml
[gates]
enabled              = false      # master switch
require_pairing      = true       # a chat must send the code before anything runs
plain_text           = "run"      # "run" (guarded) | "ignore" (require /run)
screenshot           = "document" # "document" keeps the PNG exact; "photo" lets the
                                  # chat app recompress it (small text goes blurry)
max_reply_messages   = 3          # before output is trimmed with a pointer to /full
idle_timeout_minutes = 0          # 0 = never

[gates.telegram]
token = "$TELEGRAM_BOT_TOKEN"     # the token, or "$VAR" / "${VAR}"
allow = []                        # chat ids that skip pairing; empty = everyone pairs
```

Keep the `[gates]` keys **above** any `[gates.<channel>]` table — in TOML a bare key
joins whichever table was declared above it.

## Security model

A Telegram bot accepts messages from anyone who learns its @name. So a chat id in a
config file is an **address, not a credential**. The protections, in the order they
apply:

1. **Off by default.** `[gates] enabled = false` until you change it.
2. **Pairing.** The six-digit code is printed *in your terminal*. Whoever can see your
   screen can pair; nobody else can. Until someone does, an unknown chat gets **no
   reply at all** — not even a hint that the bot is live. Five wrong codes close
   pairing for the session, and while nobody is paired the code rotates every ten
   minutes so a gate left running overnight isn't still advertising last night's.
3. **One chat at a time.** Once paired, a second chat is refused.
4. **The command guard.** Every remote command goes through the same `[security]`
   rules as an AI suggestion: `denied_commands` blocks it outright, `confirm_commands`
   holds it until you reply `/yes`. See [security.md](security.md).
5. **Redaction.** Your `[[redact]]` rules are applied to everything leaving the
   machine — **both** the `terminal` and `ai` scopes, because a chat app is off-machine
   either way.
6. **Visibility.** Every remote command, blocked attempt, and reply is echoed in the
   pane.
7. **A restart never replays.** On start the gate acknowledges the whole message
   backlog, so commands sent while it was off do not execute when it comes back. If it
   can't establish that, it refuses to start rather than guess.

### What this does not protect against

Being straight about the limits:

- **The command guard is a speed bump, not a sandbox.** It matches the text of a line.
  `l=rm; $l -rf /` defeats any pattern you can write. Pairing is the real control; the
  guard is there to catch mistakes, not a determined attacker who is already paired.
- **A paired chat has your shell.** That is the feature. It can read your files, use
  your SSH keys, and spend your money. Pair only from a device you control, and stop
  the gate when you're done.
- **Telegram sees your output.** Command output and screenshots travel through their
  servers. Your redaction rules apply first, but they only catch what you told them to.
- **The bot token appears in the process list.** It is passed to `curl` as part of the
  URL, so another user on the same machine could read it via `ps`. This matters on
  shared machines.
- **`require_pairing = false` with an empty `allow` list is refused** — it would mean
  anyone who found the bot owned the machine.

## How it works

The gate spawns your shell in its own PTY and sits between it and the pane, mirroring
every byte into an in-memory terminal — which is what makes `/shot` possible without a
screen-capture permission or a GPU.

To know exactly which output belongs to a command it was asked to run, the gated shell
emits two escape sequences the gate strips back out: one when a command starts, one
when it ends (with its exit status). This only happens inside a gated shell, and only
with `[shell] integration = true`. Without it the gate falls back to detecting pauses
in output, which is less precise — and `/status` says so rather than pretending.

## Other chat apps

Telegram ships today. Discord is next: the adapter boundary and the
`[gates.<channel>]` config shape are already generic, so it slots in without touching
anything above it. (Discord's gateway needs a WebSocket, which needs a TLS stack this
zero-dependency build doesn't have, so it will poll the REST API instead — a second or
two of extra latency.)
