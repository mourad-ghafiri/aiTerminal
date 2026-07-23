# Shell integration

The engine injects shell integration **non-destructively** when a pane spawns
(`[shell] integration = true`): for zsh via a generated `ZDOTDIR` that sources your
real config first; for bash via `--rcfile` that replicates login sourcing. Your own
rc always wins; disable everything with one switch.

What rides in — all of it **plugin data**, regenerated per spawn:

- **Aliases + abbreviations** from enabled plugins (git, dir, docker, kubernetes, …).
- **Declarative tab-completions** (`[[completion]]` specs → compdef data).
- **Theme context**: `TT_ACCENT`-style color vars for prompt/hint snippets, plus
  theme-derived `LS_COLORS`/`LSCOLORS` so `ls` matches your theme.
- **Trusted plugin snippets** (`shell.zsh` / `shell.bash`): the prompt,
  autosuggestions, history tuning, alias-hints, syntax highlighting, and the
  `@`-command handlers (the `ai-terminal` plugin).
- `TT_BIN` (the absolute binary path, so `@`-commands work from a .app bundle) and
  `TT_SESSION_LOG` (the redacted session-context file for `@ai`, only when
  `share_terminal_context` is on).

The `@`-command family hooks `command_not_found` — it fires **only** for
`@`-prefixed words (which are never real commands), so normal typing, git, vim, etc.
are untouched. `@ai`'s suggested command is dispatched by a prompt hook *in the real
shell*, so `cd`/`export`/`source` suggestions actually take effect in your session.

Features are added/removed by enabling/disabling **plugins** — the engine itself is
feature-agnostic. See [plugins.md](plugins.md).
