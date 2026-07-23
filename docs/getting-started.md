# Getting started

## Build

```sh
git clone <repo> && cd the-terminal
cargo build --release        # zero third-party crates; single binary
./target/release/aiTerminal
```

First launch seeds `~/.aiTerminal/` (config, themes, keymaps, agents, flows,
models) and opens your login shell. The `builtin/` bundle stays the source of truth
for builtin plugins — your dir holds only your edits and third-party installs.

### A real macOS app

One script builds a self-contained `aiTerminal.app` (release binary + the
`builtin/` data + icon, plus a distributable zip):

```sh
./tools/bundle-macos.sh                  # → dist/aiTerminal.app + dist/aiTerminal.zip
open dist/aiTerminal.app                 # run it — or install it:
cp -R dist/aiTerminal.app /Applications/ # then launch from Spotlight / the Dock
```

Details in [packaging.md](packaging.md). Or just put `target/release/aiTerminal`
on your PATH and run it directly.

## Enable AI

AI is off until you declare a model. Open `~/.aiTerminal/config.toml`, uncomment one
`[[ai.model]]`, and either set its `api_key` or export the provider's env var:

```toml
[[ai.model]]
provider = "anthropic"
id       = "claude-opus-4-8"
api_key  = ""                 # or: export ANTHROPIC_API_KEY=…
```

Any provider in `ai/models/*.toml` works (openai, openrouter, deepseek, groq,
ollama, lmstudio, …) — including local servers.

## First commands

```text
@ai show the 10 largest files here          # NL → a reviewed shell command
@explorer "what does this project do?"      # read-only agent, streams its report
@coder "rename Config::dir to home_dir"     # a coding agent (writes confined to the cwd)
@flow review the parser                     # a multi-step workflow (bare @flow lists)
@loop make tests pass --check "cargo test"  # iterate until verified
@job tidy the README --bg  then  @job       # a tracked task + monitoring
@coder --bg "big refactor…"  then  @job    # background + monitoring
@profile create "Work" 💼  ·  @profile switch work
@theme            # list themes    ·  @config   # show the config
```

## Daily driving

- `Cmd+T/D` tabs & splits · `Cmd+P` switcher · `Cmd+,` reload config.
- Everything configurable is a TOML file under `~/.aiTerminal/` —
  [configuration.md](configuration.md) maps the layout.
- `~/.aiTerminal/ai/aiTerminal.md` is the global AI instructions file — put your
  durable preferences there; every `@ai`/agent/flow/loop run is grounded on it.
