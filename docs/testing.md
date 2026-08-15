# Testing

aiTerminal has **1532 unit tests** and **329 scenarios**, and the whole suite runs in a
few seconds with no network, no API key, no window, and no changes to your machine.

```sh
cargo test --workspace                          # everything
cargo test -p framework scenario -- --nocapture # the scenario report, readably
```

There is no test framework dependency, because there are no dependencies at all — the
zero-crate rule applies to tests exactly as it applies to the binary. Everything below is
built from `std` and the project's own `testkit`.

---

## The two kinds, and why both exist

**A unit test proves a function. A scenario proves a product.**

That distinction is not academic. When the `@gate` feature shipped, it had a large unit
suite and users still reported bugs. Writing 35 scenarios against the same code — the
same types, the same decision layer — found **22 defects**, two of them serious: the gate
would attach to your own shell prompt and never let go, and a reply could be delivered to
the wrong chat. Neither was a broken function. Both were failures of *product behaviour*,
which is the thing a unit test is structurally unable to see.

So the suite has both, and they are aimed at different targets.

| | Unit tests | Scenarios |
| --- | --- | --- |
| Live in | `<module>/tests/` beside the code | `scenarios/<feature>/*.toml` |
| Written in | Rust | declarative TOML |
| Prove | a function's contract | a user journey end to end |
| Read like | `a_single_keystroke_is_never_wrapped_in_a_paste` | *a destructive suggestion is blocked before it can reach the shell* |
| Catch | wrong output for a given input | wrong *behaviour* across a sequence of steps |

---

## Unit tests

1532 of them (1059 in `framework`, 337 in `corelib`, 136 in `platform`), beside the code
they test. Named as sentences, so a failure reads as a statement about the product rather
than a symbol that broke:

```
✗ a_chatty_model_cannot_smuggle_a_second_line
✗ arrows_follow_the_mode_the_program_selected
✗ an_unrecognized_name_is_refused_rather_than_typed
```

They cover every pure module: the engines (`term`, `gfx`, `wire`, `re`), the AI runtime,
the `caps` tool families, plugins, security, config, profiles, i18n, the CLI, and the
pure GUI logic (panes, keymap actions, link routing, workspace persistence).

### Where they live

Each module keeps its tests in a `tests/` folder beside it, declared with one line at the
bottom of the source file:

```
crates/framework/src/config/
  mod.rs          ← ends with `#[cfg(test)] mod tests;`
  apply.rs
  paths.rs
  tests/
    mod.rs        ← `use super::*;` still sees every private item
```

A **child module**, deliberately — not a crate-level `tests/` directory. Rust's
integration tests can only see `pub` items, and most of these suites assert on private
functions (`fs_write_guard`, `apply_toml`, `shell_split`, `LiveMarkdown::feed`). Making
them public to test them would widen the API surface for the benefit of the test suite,
which is the wrong trade. A child module sees everything its parent can, so the tests
read the same as when they sat inline — they just no longer sit between you and the code.

Where a suite grows past the file-size gate it splits by the surface it exercises
(`cli/tests/{display,flow,jobs,loops,run,subcommands}.rs`), with the mocks they share in
`tests/mod.rs`.

### Cap regressions

Every bound in the performance model has a test that feeds it an over-cap input and
asserts **bounded memory and bounded time** — a 100 MB response body with no newline in
it, a 10 MB tool output, a pathological regex over 10 KB, a 500-event resize storm. These
are the tests that keep a hostile server or a runaway command from taking the window down.

### Headless render proofs

The renderer is driven through `platform::testkit` — a `MockWindow`, `MockGpu` and
`MockShaper` behind the same traits the real Metal/CoreText path implements. Layout,
glyph caching and damage tracking are asserted with no window and no GPU, so they run in
CI on Linux and Windows too. `testkit::ppm` encodes frames as uncompressed P6 for
pixel comparison (deliberately not PNG — a golden-image path should not depend on a
DEFLATE decoder).

### The OS seam

The FFI layer (`platform/src/os/`) is the one place that cannot be mocked, so it is
tested against the real thing and **skips cleanly when the thing is absent**: a Metal
upload → blit → readback round-trip on the actual GPU, a PTY that spawns `/bin/echo` and
round-trips its output, a clipboard test that saves and restores your real pasteboard.

---

## Scenarios

A scenario is a user journey written as data and played against the real code.

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

Each `[[step]]` carries one verb. An unrecognized verb **fails the suite** — a typo in a
scenario must never pass silently.

### Coverage

329 journeys across every feature. Each folder's test asserts a minimum count, so
coverage cannot silently shrink.

| Folder | # | What a journey drives |
| --- | --- | --- |
| `gate/` | 36 | pairing, remote commands, the guard, attaching to interactive CLIs |
| `markdown/` | 31 | rendering, streaming, diagrams, the pager, and the shape of every log a run leaves |
| `flow/` | 30 | the graph: verification before it spends, parallel branches, conditions, bounded loops, fan-out, approvals, retries, building a graph for a bare goal, node control, the two ways of watching a run, and the board's own geometry — depth as columns, edges the graph implies left undrawn, a block that never changes height |
| `ai/` | 34 | model reply → command or answer, the guard, the agent tool loop |
| `terminal/` | 22 | the VT engine — grid, colour, wide glyphs, scrollback, DEC modes |
| `jobs/` | 20 | scheduling, occurrence logs, and what a job reports when it stops |
| `cli/` | 26 | the `@`-command surface: profiles, themes, config, plugins, documents, jobs, gates, and every verb that reads a flow/loop/job record — with a run to read, without one, and with one whose graph exists only in its own record |
| `config/` | 16 | config parsing and the profile overlay |
| `guard/` | 41 | one policy over commands, paths and secrets — the placeholder round trip, and a value that is a second command once it is put back |
| `plugins/` | 14 | what a `plugin.toml` composes into, and what trust gates |
| `loop/` | 13 | the verifier, the bounds, and the one escalation |
| `shell/` | 10 | the init script sourced into your shell |
| `keymap/` | 10 | chords, actions, and which binding wins |
| `theme/` | 8 | tokens, round-trip, fallback |
| `workspace/` | 10 | the folder as a conversation: the trust gate and what it names, a transcript that remembers, a confirm asked and answered both ways, the project overlay shadowing an agent, plan mode, `!` feeding the next turn, an inline `@flow` run feeding the next turn, prompt files as slash commands |
| `memory/` | 8 | what an agent remembers, where, and what it forgets |

The `cli/` folder is where a **command** is proved rather than the machinery behind it.
Its steps are lines a person types, run through the same dispatch the shell calls — and
what they assert afterwards is *what changed*, never what was printed. A message is
words, and words are pinned by unit tests where they are built; "the profile exists and
is now the active one" is the behaviour.

### How it is built

The engine is a **Strategy**: one `World` per feature owns that folder's verbs and knows
nothing about discovery, parsing or reporting.

```rust
pub trait World {
    fn apply(&mut self, step: &Toml) -> Result<(), String>;
}

pub type Factory = fn(&Toml) -> Result<Box<dyn World>, String>;
```

The vocabulary is open *across* features but closed *within* each one, which is why verbs
are matched per world rather than through one shared enum — each world keeps an
exhaustive match and returns `unknown_verb` for anything else.

Adding a feature is three things: a folder under `scenarios/`, a file in
`crates/framework/src/scenario/worlds/`, and a line in `REGISTRY`. A test asserts that
every folder has a world, so a folder without one cannot sit there silently never running.

The whole engine is `#[cfg(test)]`. Nothing ships.

### The report

```
  ai scenarios
  ✓ a destructive suggestion is blocked before it can reach the shell
  ✓ a chatty model cannot smuggle a second command into your shell
  ✗ an empty reply does not preload anything
      028-an-empty-reply-does-not-preload-anything.toml · step 3 · expect_marker [1 items]
      expected "# the AI did not suggest a command" in the line the shell reads; got "#TT-ANSWER#"

  22/23 passed
```

A failure names the file, the step number, the verb, what was expected and what actually
happened — so it reads as a bug report, not a stack trace.

That particular failure was real. When a model returned nothing to `@ai --command`, the
terminal printed nothing and preloaded nothing, so the request was indistinguishable from
a no-op. The evidence was already in the code: `command_marker`'s entire "no command"
tier had exactly one caller — its own unit test. Three unit tests were asserting
behaviour production could not produce. The scenario is what made that visible.

### Writing one

Read the world's source file: each documents its own verbs at the top, and the shape is
the same everywhere — **what is set up**, **what a person or a program does**, and **what
must be true**.

Two constraints from the bundled TOML parser: no multi-line arrays (keep an array on one
line) and no `\u` escapes (write the character). Control bytes get readable names instead:
`<ESC>` `<BEL>` `<CR>` `<LF>`.

---

## Hermetic and harmless by design

The suite runs on your machine without touching your machine. This is a hard rule, not an
aspiration.

**AI is always mocked.** Every model interaction runs against `MockTransport` or
`ScriptedTransport` with canned SSE fixtures. The AI scenarios go further: the scenario
writes what the model replies, that text is encoded as the provider's *real* SSE wire
format, and it comes back through the *real* decoder — so the streaming path, both
provider dialects, and the reply classifier are all exercised with no socket and no key.
API keys in tests are dummy values; `CurlTransport` is constructed only at runtime.

**No network.** SSRF and network gating are tested through the refusal paths — `[ai]
network = false`, https-only — which fail *before* any socket is opened. The git-browsing
test builds a throwaway repo in a temp dir.

**No user state.** Anything touching `~/.aiTerminal` takes the shared `test_home` lock,
runs under a temp `$HOME`, and restores the previous value on drop. `$HOME` is
process-global, so a single mutex serializes every test that depends on it — otherwise
they would race each other and leak temp homes into unrelated tests. Filesystem tests
confine themselves to per-test scratch dirs; the clipboard test saves and restores your
real pasteboard.

**No dangerous commands.** This is what makes it safe — and meaningful — to write a test
*about* `rm -rf /`. Guard tests assert the string is **blocked**; scenarios assert it
**went nowhere**. No world spawns a process, opens a socket, or touches a PTY:

- **gate** has no PTY at all. "Bytes written to the terminal" land in a `Vec<u8>`, so
  there is no mechanism by which a command could execute.
- **ai** cannot execute tools. The runner is a lookup table the scenario wrote; a
  scenario about `sys.run rm -rf /` is a scenario about a string being refused.
- **plugins** never calls `evaluate`, which would spawn a process to read the clock.

The only commands the suite actually runs are inert: `echo`, `true`, `sleep`, `git init`
in a temp dir, and two shell invocations described next.

### The one deliberate exception

The shell world runs `zsh -n` and `bash -n` on the generated init script. That is
**parse-only** — the shell reads the file and exits without executing a single command.

It earns its exception. A quoting slip in that script breaks every new pane, and no
amount of substring matching would catch it. See
`scenarios/shell/018-a-hostile-alias-value-cannot-break-out.toml`, where a plugin's alias
value is chosen specifically to terminate the quoting and append `touch /tmp/pwned`. The
assertion is that the shell still parses it as one alias.

One unit test goes slightly further and runs `zsh -c` to *source* a generated alias index
and print a value back, because a text assertion alone missed a real regression: the
index stored `a['git']=…` with the quotes included, so `${a[git]}` was always empty and
no alias hint ever fired. Both skip cleanly where the shell isn't installed.

---

## Architectural gates

Five scripts guard the project's structural promises. They are not tests of behaviour —
they fail the build when an invariant is broken.

```sh
bash tools/check_no_crates.sh      # 🚫 no third-party crate may enter the build
bash tools/check_layers.sh         # 🧱 every cross-layer edge is the immediately-lower facade
bash tools/check_unsafe.sh         # 🔒 all unsafe is confined to platform/src/os/
bash tools/check_file_size.sh      # 📏 no source file over 1000 lines
bash tools/check_verb_coverage.sh  # ⌨️ every documented verb is run by a scenario
```

The zero-crate gate reads `Cargo.lock` and rejects any source that is not a workspace
member — direct or transitive. The layer gate enforces `corelib < platform < framework <
app`. The unsafe gate keeps every `unsafe` block inside the FFI seam, which is why
`framework` can carry `#![forbid(unsafe_code)]`. The file-size gate is the newest, and
the one aimed at contributors rather than at the binary: a file nobody reads end to end
is a file nobody can change safely, so the split has to happen while it is still cheap.

The verb gate is the newest, and the only one that guards *coverage* rather than
structure. It reads each command's own usage text, extracts the verbs it advertises, and
fails naming any that no scenario ever types. Run against the tree it was added to, it
named five: `@flow watch`, and all four `@loop` reading verbs — documented, implemented,
unit-tested at the engine, and never once routed through the dispatch a person uses. That
gap is invisible in a coverage report, because the code *is* covered; what was missing was
the path to it. Note what a pass does **not** mean: `@flow show last` on an empty store
satisfies the gate while only proving the refusal. Reading a real record back is a
scenario's job, and no script can check that a scenario was worth writing.

`.github/workflows/ci.yml` builds and tests on macOS, Linux and Windows on every push and
pull request, then runs all five gates. They must be run with `bash`, not `sh`.

---

## What is not covered, and why

Honest gaps are more useful than an overstated number.

- **The window event loop and the FFI seam** are OS-bound by nature. They are covered by
  the headless render proofs and by running the app, not by unit tests.
- **The AI CLI wrappers** (`run_agent_cli`, `run_flow_cli`, `run_loop_cli`) each
  hard-code `CurlTransport`, resolve agents from `$HOME`, and launch MCP subprocesses.
  Making them drivable is a genuine refactor rather than a seam. The layers *beneath*
  them — the client, the agent loop, orchestration, the reply classifier — are fully
  covered, including through scenarios.
- **GUI layout and rendering** need a window for a true end-to-end proof. The pure logic
  is unit-tested and the renderer has headless proofs; the composition is verified by
  running it.

## Related

- [architecture.md](architecture.md) — the four-layer workspace and the invariants these
  gates enforce.
- [security.md](security.md) — the AI guard the `guard/` scenarios drive.
- `scenarios/README.md` — the scenario suite's own guide, next to the scenarios.
