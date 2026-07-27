# Scenarios

Real user journeys, written as data, played against the real code.

A unit test proves a function. A scenario proves a **product**: it reads as a sequence of
things a person does and a program does, and it runs the actual decision layers, the
actual mirror terminal, the actual generated shell script and the actual model-reply
classifier that ship in the binary.

```sh
cargo test -p framework scenario -- --nocapture
```

```
  ai scenarios
  ✓ a destructive suggestion is blocked before it can reach the shell
  ✓ a chatty model cannot smuggle a second command into your shell
  ✗ an empty reply does not preload anything
      028-an-empty-reply-does-not-preload-anything.toml · step 3 · expect_marker [1 items]
      expected "# the AI did not suggest a command" in the line the shell reads; got "#TT-ANSWER#"

  22/23 passed
```

That failure is real: it is the defect this suite found in `@ai --command`, where a model
that returned nothing printed nothing and preloaded nothing, so the request was
indistinguishable from a no-op.

## Layout

One folder per feature, one **world** per folder. The world gives that folder's verbs
meaning; the engine owns discovery, parsing, ordering and the failure report. Adding a
feature is a folder, a file in `crates/framework/src/scenario/worlds/`, and a line in
`REGISTRY`.

```
scenarios/
  ai/         the model's reply → a command or an answer, the guard, the agent tool loop
  config/     config parsing and the profile overlay
  gate/       @gate — pairing, shell commands, the guard, attaching to programs
  keymap/     chords, actions, and which binding wins
  markdown/   rendering, streaming, diagrams, the pager
  plugins/    what a plugin.toml composes into, and what trust gates
  security/   the command guard and the redactor
  shell/      the init script that gets sourced into your shell
  terminal/   the VT engine — what a program prints, what you end up looking at
  theme/      theme tokens, round-trip, fallback
```

Filenames are numbered by theme so the report reads in the order the journeys were
designed. Each folder's test asserts a minimum count, so coverage cannot silently shrink.

## Writing one

```toml
name = "a destructive suggestion is blocked before it can reach the shell"
tags = ["ai", "security"]

[setup]
command_mode = "auto"
deny = ["rm -rf /"]

[[step]]
model_says = "RUN: rm -rf /"
[[step]]
command = "clean up everything"

[[step]]
expect_marker = ["# blocked by guard"]
[[step]]
expect_marker_missing = ["#TT-RUN#", "#TT-EDIT#"]
```

Each `[[step]]` carries exactly one verb, plus any modifiers that verb reads. An
unrecognized verb fails the suite — a typo in a scenario must never pass silently.

Control bytes get readable names: `<ESC>` `<BEL>` `<CR>` `<LF>`. The bundled TOML parser
takes no multi-line arrays and no `\u` escapes; keep an array on one line and write the
character itself.

Each world's verbs are documented at the top of its own file. The shape is the same
everywhere: **what is set up**, **what a person or a program does**, and **what must be
true**.

## Nothing dangerous ever runs

No world spawns a process, opens a socket, or touches a PTY.

- **gate** has no PTY. "Bytes written to the terminal" land in a `Vec<u8>`.
- **ai** never reaches the network. The model is a scripted transport: the scenario
  writes what the model replies, that text is encoded as the provider's real SSE wire
  format, and it comes back through the real decoder. Tools are scripted too — the
  runner cannot execute anything, so a scenario about `sys.run rm -rf /` is a scenario
  about a string being refused.
- **plugins** never calls `evaluate`, which would spawn a process to read the clock.
- **shell** is the one place anything is executed, and it is `zsh -n` / `bash -n`:
  the shell *parses* the generated script and exits without running a single command.
  It is worth it — a quoting slip there breaks every new pane, and no amount of
  substring matching would catch it. See
  `shell/018-a-hostile-alias-value-cannot-break-out.toml`, where a plugin's alias value
  is chosen specifically to terminate the quoting and append a command.

That is what makes it safe — and meaningful — to write a scenario about `rm -rf /`: the
string appears only as text to be matched by a deny pattern, and the assertion is that it
went nowhere. See `gate/030-a-denied-command-never-reaches-the-terminal.toml` and
`ai/022-a-destructive-suggestion-is-blocked.toml`.
