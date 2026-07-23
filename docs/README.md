# aiTerminal — documentation

A light, AI-first terminal. Everything is a terminal command (`@ai`, `@<agent>`,
`@flow`, `@loop`, `@job`, `@profile`, `@config`, `@theme`, `@plugin`) and every
setting is a TOML file — there is no settings UI.

| Doc | What it covers |
| --- | --- |
| [getting-started.md](getting-started.md) | Build, first run, enabling AI, your first `@`-commands. |
| [features.md](features.md) | The terminal itself: tabs, splits, switcher, selection, ⌘-click, status bar, profiles. |
| [ai.md](ai.md) | The whole AI surface: `@ai` / agents / flows / loops / background jobs, memory, MCP, models & pools, the tool catalog. |
| [configuration.md](configuration.md) | `config.toml` reference, the `~/.aiTerminal/` layout, profiles, i18n. |
| [keybindings.md](keybindings.md) | Default chords, the action list, custom keymaps. |
| [plugins.md](plugins.md) | The declarative plugin system (`plugin.toml` + shell snippets). |
| [shell.md](shell.md) | Shell integration: how snippets/aliases/colors are injected. |
| [theme.md](theme.md) | Themes: the token model, the collection, custom themes. |
| [security.md](security.md) | The command guard, redaction, agent tool gating, SSRF rules. |
| [logging.md](logging.md) | The diagnostic logger. |
| [packaging.md](packaging.md) | Bundling the macOS .app. |
| [architecture.md](architecture.md) | The four-layer workspace, module map, invariants, and how to develop/test. |
