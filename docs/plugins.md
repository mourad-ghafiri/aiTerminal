# Plugins

A plugin is a **folder of data**: `plugins/<name>/plugin.toml` plus optional shell
snippets. No plugin runs code inside the terminal process — shell snippets run in
*your shell*, and only for **trusted** plugins (builtin, or installed by you).

```text
~/.aiTerminal/plugins/<name>/
  plugin.toml        # the manifest (everything below)
  shell.zsh          # optional zsh snippet (sourced by the integration)
  shell.bash         # optional bash snippet
```

Builtin plugins load straight from the bundled `builtin/plugins/` (single source of
truth — they always match the binary); your dir holds third-party installs and
disables. Manage with `@plugin list|install|enable|disable|remove|info`, or
`[plugins] disabled = [...]` in config.

## What a manifest can declare

| Table | What it does |
| --- | --- |
| `[aliases]` / `[[abbr]]` | Shell aliases (a `name = "expansion"` table) and expand-on-space abbreviations. |
| `[[completion]]` | Declarative tab-completion for a command (subcommands + flags). |
| `[[var]]` + `[[segment]]` | Status-bar data: probed/derived variables rendered as left/right segments with theme colors. |
| `[[keybinding]]` | Chord → action contributions to the keymap. |
| `shell.zsh` + `shell.bash` | Feature snippets (trusted plugins only). Ship BOTH dialects — zsh-only is acceptable solely for zsh-specific features (ZLE widgets, global aliases); CI enforces the parity. |
| `[[allow_command]]` / `[[deny_command]]` / `[[confirm_command]]` / `[[safe_command]]` | Command-guard rules (plugins can only ADD restrictions/safety data — deny wins). |
| `[[redact]]` | Redaction rules (pattern → replacement, scoped terminal/ai/all). |

## The builtin set

- **`ai-terminal`** — the `@`-command family (`@ai`, `@<agent>`, `@flow`, `@job`,
  `@profile`, `@config`, `@theme`, `@plugin`).
- **Shell UX**: `prompt`, `autosuggest`, `syntax-highlight`, `completion`,
  `alias-hints`, `history`, `lineedit` (macOS-style navigation + ⇧-selection on
  the command line), `sudo`, `common`, `dir`, `jump`, `term-cwd`.
- **Security**: `command-guard` (the default deny/confirm/safe rules), `redactor`
  (the default secret-masking rules).
- **Tooling**: `git`, `github`, `docker`, `kubernetes`, `node`, `python`, `rust`,
  `colored-man`, `extract`, `encode`, `clipboard`.
- **Info**: `sysinfo`, `weather`, `world-clock`, `web-search`, `notes`.

## Writing one

See [examples/plugin/](../examples/plugin/) for a complete, tested template
(segment, aliases, abbreviation, completion, guard + redaction rules, shell
snippet). Drop the folder in `~/.aiTerminal/plugins/`, restart (or
`@plugin enable hello`). Segments re-evaluate on `cd`/tab-switch and refresh the
status bar instantly.

The engine is **UI-independent**: the same `plugin::load_registry` serves the
window, the CLI, and headless renders — a plugin never touches the render layer;
it only contributes data (and shell snippets that run in *your shell*).
