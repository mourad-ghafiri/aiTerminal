# Examples

Working, copy-paste-able templates for everything the terminal supports, grouped
by what they extend. Each file says where to install it; nothing here is loaded
automatically. The plugin, flow, and agent examples are guarded by schema tests,
so they can never drift from the live formats.

## `plugin/` — extend the shell & status bar

| File | Shows |
| --- | --- |
| [plugin/plugin.toml](plugin/plugin.toml) | The full declarative surface: status segment, aliases, abbreviation, completion, guard + redaction rules. Install the folder to `~/.aiTerminal/plugins/hello/`. |
| [plugin/shell.zsh](plugin/shell.zsh) | An optional trusted shell snippet (ship a `shell.bash` twin for real plugins). |

## `ai/` — extend the AI

| File | Shows | Install to |
| --- | --- | --- |
| [ai/agent.md](ai/agent.md) | A custom agent (`@docs-writer …`) | `~/.aiTerminal/ai/agents/` |
| [ai/flow.toml](ai/flow.toml) | A multi-step workflow (`@flow ship "<change>"`) | `~/.aiTerminal/ai/flows/` |
| [ai/loop.md](ai/loop.md) | `@loop` recipes — engineered agent loops with verifiable goals | (usage examples, nothing to install) |

## `config/` — appearance, keys, profiles, language

| File | Shows | Install to |
| --- | --- | --- |
| [config/profile.toml](config/profile.toml) | A per-profile config overlay | `~/.aiTerminal/profiles/<id>/config.toml` |
| [config/keymap.toml](config/keymap.toml) | A user keymap override file | `~/.aiTerminal/keymaps/` |
| [config/theme.toml](config/theme.toml) | A minimal custom theme (missing tokens derive) | `~/.aiTerminal/themes/` |
| [config/locale.toml](config/locale.toml) | An i18n override (partial is fine — layers over the bundled locale) | `~/.aiTerminal/i18n/<locale>.toml` |
