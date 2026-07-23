# Architecture

Four layers, four crates — `corelib < platform < framework < app` — with **zero
third-party crates** (every dependency in `Cargo.lock` is a workspace member).

```text
crates/app        the `aiTerminal` binary: argv → framework. Nothing else.
crates/framework  the product: gui (window runtime), ai (the AI engine), caps (agent
                  tools), plugin, security, config, profile, shell, theme, keymap,
                  render (headless proofs), cli (the subcommands)
crates/platform   the OS seam: os/ (macOS FFI — the ONLY unsafe), term (VT engine),
                  transport (HTTP/SSE via system curl), log, orchestrator
crates/corelib    pure + OS-free: wire (JSON/TOML/frontmatter), gfx (CPU rasterizer),
                  types, theme tokens, unicode, datetime, brand
```

## Invariants (CI gates)

- `tools/check_no_crates.sh` — no registry/git source in `Cargo.lock`.
- `tools/check_layers.sh` — a crate depends only on strictly-lower layers; `app`
  depends on `framework` only.
- `tools/check_unsafe.sh` — `unsafe` exists only under `crates/platform/src/os/`;
  every other crate root is `#![forbid(unsafe_code)]`.

There are no facade crates: callers name real paths (`framework::ai::Client`,
`platform::term::Term`, `corelib::wire::Json`).

## Performance model

The rules that keep an idle terminal at ~0% CPU and every input bounded:

- **The event loop idles.** A clean window blocks in the OS event wait and ticks
  ~1×/s (for the coarse timers: autosave, config poll). Producers (PTY readers,
  the status worker, input) mark a shared `DirtyFlag`; the clean→dirty edge posts
  ONE wake event, so a flooding producer coalesces. Renders are paced to ~60 Hz.
- **Per-frame work sits behind generation counters.** `Term::generation()` (bumped
  on feed/resize) and `cwd_seq` gate the session-context rebuild (≥500 ms apart),
  the periodic autosave, and the incremental render — a frame where nothing moved
  costs a few atomic loads.
- **Present is damage-tracked.** Unchanged chrome → only panes whose content stamp
  moved are re-rendered, and `Gpu::present` uploads only the damaged row span
  (`Surface::take_damage` → Metal `replaceRegion`).
- **Every read is bounded, every deadline kills.** Named per-site caps, each with
  an over-cap regression test:

  | Cap | Where | Bound |
  | --- | --- | --- |
  | `MAX_SSE_LINE` / `MAX_SSE_EVENT` | AI stream transport | 4 MiB line / 8 MiB event, abort + kill curl |
  | `ERROR_SNIFF_CAP` | AI stream transport | 64 KiB retained head (never the whole stream) |
  | `SYS_RUN_CAP` + `SYS_RUN_DEADLINE` | `sys.run` tool | 256 KiB output, 60 s then kill |
  | `FS_READ_MAX` | `fs.read` tool | 1 MiB, model's `max` arg clamped |
  | `GIT_OUT_CAP` + `GIT_TIMEOUT` | git tools | 4 MiB/stream, 60 s then kill |
  | `TOOL_RESULT_MAX` / `TRANSCRIPT_SOFT_MAX` | agent loop | 48 KiB/result clip, 512 KiB transcript with oldest-result elision |
  | `CHECK_TAIL` + `CHECK_DEADLINE` | `@loop --check` | 64 KiB rolling tail, 600 s then kill |
  | `MAX_ATTACHMENTS` / `MEDIA_ATTACH_MAX` | `@<path>` attachments | 16 files, 4 MB each |

  Subprocess capture goes through `framework::procio::run_bounded` (cap + drain +
  kill-at-deadline) — a timeout that leaves the child running is a bug.
- **The regex engine can't hang.** One step budget per operation (scaled linearly
  with input), a literal-prefix prefilter for secret rules, and redaction leaves
  text untouched (with a logged warning) rather than stall or half-apply.
- **Resize never rewrites history.** Scrollback rows keep their captured width;
  readers clamp (`row.get(x)`), so a window drag is O(screen) per event.

## The framework modules

| Module | Role |
| --- | --- |
| `gui` | The window: tabs/splits of PTY panes, input routing, tab switcher, status bar, ⌘-click links, per-profile state persistence (layout + pane content + window size), live follow of profile switches and config edits. No AI code — the window is a pure terminal. |
| `ai` | The engine: streaming `Client` over a `Transport` seam, the agent loop (`run_agent` + `ToolRunner`/`AgentObserver`), flows (`run_orchestration`), the `aiTerminal.md` instruction base, BM25 memory, MCP hub, model catalog + weighted pools (vision/document/thinking caps), redaction-aware context capture. Fully offline-testable (`MockTransport`/`ScriptedTransport`). |
| `caps` | The native tool catalog agents call (fs/sys/web/memory/data/todo/task/…): a registry of `NativeObject`s, pure over `CapCtx` (policy + data dirs + the write sandbox = the invocation directory). |
| `cli` | The headless subcommands: `ai` (Q&A / command / agent / flow / loop / job), `profile`, `plugin`, `config`, `theme`. This is what the `@`-commands invoke. |
| `plugin` | Declarative plugins: manifest parsing, the registry (aliases/abbr/completions/segments/security rules/snippets), the store. |
| `security` | The command guard (allow/confirm/deny, regex over the in-house `re` engine) + scoped redaction. Deny wins. |
| `config` | `~/.aiTerminal/config.toml` load/bootstrap/seeding + the profile overlay layering. |
| `profile` | Named profiles: per-profile `config.toml` overlay + saved window state + the active pointer. |
| `i18n` | The locale catalog behind every user-facing string (bundled en/fr + user overrides, `[appearance] locale`). |
| `shell` | Shell integration: generates the zsh/bash init (aliases, completions, trusted plugin snippets, non-destructive via ZDOTDIR / --rcfile) and the live `colors.sh` running shells re-source on theme switches. |
| `render` | Headless renderers (terminal frame, chrome, switcher, icon) for visual proofs without a GUI session. |

## How the AI stays out of the window

The window's only AI involvement is writing a redacted session-context file
(`$TT_SESSION_LOG`) for grounding. `@ai` / `@<agent>` / `@flow` run in the *shell*
via the `ai-terminal` plugin → `aiTerminal ai …` → stdout into the PTY. Background
runs are plain detached processes recorded under `ai/jobs/`. This keeps the render
loop simple and every AI interaction scriptable.

## Developing

```sh
cargo build --workspace --all-targets
cargo test  --workspace                 # hermetic: mock transports, temp HOMEs
bash tools/check_no_crates.sh && bash tools/check_layers.sh && bash tools/check_unsafe.sh
```

Useful headless proofs (no window needed):

```sh
aiTerminal --render-ppm /tmp/t.ppm --cols 80 --rows 24   # a rendered terminal frame
aiTerminal --render-chrome top                            # tab bar + status bar
aiTerminal --render-switcher                              # the Cmd+P overlay
aiTerminal --render-icon /tmp/icon.png                    # the app icon
```

## Testing policy

The suite (450+ tests) is **hermetic and harmless by design** — it can run on a
developer's machine without touching their real state:

- **AI is always mocked.** Every model interaction runs against
  `MockTransport`/`ScriptedTransport` with canned SSE fixtures — the client,
  the agent loop, flows (`run_orchestration`), and the `@loop` engine
  (`drive_loop` is transport-generic precisely so tests can script the maker's
  answers and the verifier's verdicts). API keys in tests are dummy values behind
  test-only env vars; `CurlTransport` is constructed only at runtime.
- **No network.** SSRF/network gating is tested through the refusal paths (`[ai]
  network = false`, https-only) that fail *before* any socket; the git-browsing
  test builds a throwaway local repo in a temp dir.
- **No user state.** Anything touching `~/.aiTerminal` takes the shared
  `test_home` lock and runs under a temp `$HOME`; filesystem tests confine
  themselves to per-test scratch dirs; the macOS clipboard round-trip saves and
  restores the user's pasteboard.
- **No dangerous commands.** Guard tests assert that `rm -rf`/fork-bomb strings
  are *blocked* — they are never executed. The only commands tests actually run
  are inert (`echo`, `true`, `sleep`, `zsh -n`/`bash -n` syntax parses,
  `git init` in a temp dir).
- **Every cap has a regression test.** Each bound in the performance model is
  exercised with an over-cap input (a 100 MB no-newline stream, a 10 MB tool
  output, a pathological regex on 10 KB, a 500-event resize storm) asserting
  bounded memory AND bounded time.
- **Coverage shape.** Every pure module — the engines (`term`, `gfx`, `wire`,
  `re`), the AI runtime, `caps` tool families, plugins, security, config,
  profiles, i18n, the CLI (flows/jobs/loop/delegation), and the pure GUI logic
  (panes, keymap actions, link routing, workspace persistence) — has direct
  unit tests. The thin remainder is OS-bound by nature (the FFI seam, the
  window event loop) and is covered by the live app + headless render proofs
  rather than unit tests.
