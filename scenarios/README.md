# Scenarios

Real user journeys, written as data, played against the real code.

A unit test proves a function. A scenario proves a **product**: it reads as a sequence of
things a person does and a program does, and it runs the actual decision layer, the
actual mirror terminal, and the actual relay ordering that ships in the binary.

```sh
cargo test -p framework scenario -- --nocapture
```

```
  gate scenarios
  ✓ a stranger who finds the bot learns nothing
  ✓ an idle shell prompt does not look like a program
  ✗ a tap shows its result promptly
      064-a-tap-is-answered-promptly.toml · step 7 · expect_frame_within_ms 1200
      no live frame within 1200ms of the last action — a tap that shows nothing reads as broken

  34/35 passed
```

## Layout

One folder per feature. The runner walks whatever is there, so adding a feature is
adding a folder and a test that points at it.

```
scenarios/
  gate/     @gate — pairing, shell commands, the guard, attaching to programs
```

Filenames are numbered by theme so the report reads in the order the journeys were
designed: `01x` pairing, `02x` shell, `03x` the guard, `04x` attach detection,
`05x` driving a program, `06x` refusals and lifecycle.

## Writing one

```toml
name = "a numbered question becomes buttons and a tap answers it"
tags = ["attach", "buttons"]

[setup]
paired = true

[[step]]
run_local = "claude"        # started at the keyboard, so the shell reports it
[[step]]
app_modes = "bracketed"     # an inline CLI — it never touches the alternate screen
[[step]]
expect_attached = true

[[step]]
screen = ["Do you want to make this edit?", "❯ 1. Yes", "  2. No"]
[[step]]
wait_ms = 2500
[[step]]
expect_buttons = ["k:1", "k:2"]

[[step]]
tap = "k:1"
[[step]]
expect_pty = "1"
```

Each `[[step]]` carries exactly one verb. An unrecognized verb fails the suite — a typo
in a scenario must never pass silently.

### Setup

| Key | Meaning |
| --- | --- |
| `paired` | start already paired (skips retyping the handshake) |
| `allow` | chat ids pre-authorized in config |
| `plain_text_runs` | `[gates] plain_text` |
| `attach` | `[gates] attach` |
| `deny` · `confirm` | command-guard patterns |
| `redact` | redaction patterns |
| `cols` | terminal width |

### What people and programs do

| Verb | Meaning |
| --- | --- |
| `chat` (+ `from`) | a message from the paired user, or from another chat id |
| `tap` (+ `from`) | a button tap, carrying its callback value (`k:1`) |
| `pty` | bytes the shell printed |
| `screen` | paint the mirror, as a program repainting |
| `local` | keys typed at the local keyboard |
| `run_local` | the local user runs a command |
| `shell_start` · `shell_end` | the shell's preexec/precmd marks |
| `shell_prompt` | the shell arms its line editor, as zsh and bash really do |
| `app_modes` | a program declares itself: `alt`, `mouse`, `bracketed`, `app_cursor` |
| `app_release` | it hands the terminal back |
| `wait_ms` | advance the clock |

Control bytes get readable names: `<ESC>` `<BEL>` `<CR>` `<LF>`.

### What must be true

| Verb | Meaning |
| --- | --- |
| `expect_says` · `expect_not_says` | fragments that must (not) appear in what was sent |
| `expect_pty` · `expect_no_pty` | exactly what reached the terminal since the last check |
| `expect_attached` | is a program being driven |
| `expect_buttons` | callback values the live screen offers |
| `expect_local` | what was printed in the pane |
| `expect_chat_id` | everything went to this chat — the disclosure check |
| `expect_frame_within_ms` | a user action produced a frame this promptly |
| `expect_live_reposted` | the live screen was re-posted rather than edited in place |
| `expect_nothing_queued` | nothing is waiting to run later |

## Nothing dangerous ever runs

The world has **no PTY and no process spawning**. "Bytes written to the terminal" land in
a `Vec<u8>`; there is no mechanism by which a command could execute.

That is what makes it safe — and meaningful — to write a scenario about `rm -rf /`:
the string appears only as text to be matched by a deny pattern, and `expect_no_pty`
proves it went nowhere. See `030-a-denied-command-never-reaches-the-terminal.toml`.
