# Configuration

Everything is TOML — there is no settings UI. Inspect with `@config`, edit the file,
reload live with `Cmd+,` (or restart).

```text
❯ @config       # the EFFECTIVE settings — after the active profile's overlay
❯ @config path  # the config file's path:  $EDITOR "$(@config path)"
```

`@config` prints the effective value, so it is the quickest way to find out whether
a profile overlay is overriding what you think it is.

## `~/.aiTerminal/` layout

```text
config.toml            the global config (seeded, fully documented — read it!)
profiles/<id>/         per-profile: profile.toml, config.toml (overlay), workspace.toml
plugins/<name>/        installed plugins (builtin ones load from the bundle)
themes/*.toml          themes (seeded from the bundle; add your own)
i18n/<locale>.toml     locale overrides (layer over the bundled builtin/i18n)
keymaps/*.toml         your keymap override files
ai/                    everything AI:
  aiTerminal.md          the global AI instructions (the system-prompt base)
  agents/*.md            agents (@<name>)     skills/*.md    skills
  prompts/*.md           prompt blocks        flows/*.toml   workflow graphs (@flow)
  mcp/                   MCP declarations     memory/*.md    global durable memory
  models/*.toml          the provider/model catalog
  jobs/<id>/             background job records (@job)
  flow-runs/<id>/        @flow run records: run.toml + nodes/<id>.md
  loops/<id>/            @loop run records: loop.toml + iterations/<n>.md
  sessions/<id>/         per-folder AI memory: session.md (recent-run digest),
                         memory/ (folder-scoped), meta.toml — restored on return
cache/                 regenerable caches (cloned repos for web.read)
logs/                  daily diagnostic logs
shell/                 the generated shell integration + live theme colors
```

One rule: everything AI lives under `ai/`, regenerable artifacts under
`cache/`/`logs/`/`shell/`, and `ai/aiTerminal.md` is the single global instructions
file every `@ai`/agent/flow/loop run is grounded on — edit it to shape how the
AI works for you.

## `config.toml` sections

| Section | Keys |
| --- | --- |
| `[appearance]` | `theme`, `locale` (i18n — see below), `font_family`, `font_size`, `cursor_style` (`block` \| `bar` \| `underline`) |
| `[behavior]` | `zoom`, `tab_bar` (top/bottom/left/right), `shell`, `scrollback` |
| `[ai]` | `share_terminal_context`, `memory`, `show_reasoning` (show the model's thinking text; default off), `mode` (manual/auto), `network`, `budget` (USD cost soft-cap, advisory); then `[ai.balance] strategy` and the `[[ai.model]]` pool blocks — see [ai.md](ai.md#models--pools) |
| `[gates]` | `enabled`, `require_pairing`, `plain_text` (run/ignore), `screenshot` (document/photo), `max_reply_messages`, `idle_timeout_minutes`; then a `[gates.<channel>]` table per gateway — see [gate.md](gate.md) |
| `[md]` | `syntax` (highlight code fences), `image_max_rows`, `remote_images` (fetch `https://` images; off by default) — see [markdown.md](markdown.md) |
| `[jobs]` | `max_concurrent` (jobs running at once), `keep_runs` (per-occurrence logs kept), `max_log_bytes` (cap per run log) — see [ai.md](ai.md#job--say-what-to-do-and-when) |
| `[loop]` | `max`, `timeout`, `check_timeout`, `keep_runs`, `propose_check` (let the AI infer a verifier command) — see [ai.md](ai.md#loop--iterate-until-it-verifies) |
| `[flow]` | `concurrency` (nodes in flight at once), `timeout`, `node_timeout`, `keep_runs`, `max_map` (the ceiling on a fan-out) — see [ai.md](ai.md#flow--a-workflow-declared-as-a-graph) |
| `[plugins]` | `enabled`, `disabled = ["name", …]` |
| `[shell]` | `integration` (master switch for injected aliases/snippets/colors) |
| `[registry]` | `dir` — where the bundled `builtin/` lives (empty = auto-resolve) |
| `[logging]` | `level` (off/error/warn/info/debug/trace), `retention_days` |
| `[security]` | `allowed_commands`, `denied_commands`, `confirm_commands`, `auto_safe_commands` (regex lists) |
| `[[keybinding]]` | `key = "cmd+shift+x"`, `action = "split_down"` (see keybindings.md) |
| `[[redact]]` | `pattern`, `replacement`, `scope` (terminal/ai/all), `literal` |

The seeded `config.toml` documents every key inline — it is the reference.

### ⚠️ Table order matters

TOML assigns every bare `key = value` to the table header **above** it. Array-of-table
blocks — `[[ai.model]]`, `[[keybinding]]`, `[[redact]]` — open a new table, so any
plain section key written after one joins *that* block instead of the section:

```toml
[ai]
[[ai.model]]        # ❌ opens a new table here
id = "…"
api_key = "sk-…"

memory = true       # ← lands inside [[ai.model]], NOT [ai]
```

Write a section's plain keys first and its `[[…]]` blocks last:

```toml
[ai]
memory = true       # ✅ [ai]'s own keys first

[[ai.model]]        # ✅ blocks last
id = "…"
api_key = "sk-…"
```

The seeded file is already laid out this way, and aiTerminal warns at startup if an
`[[ai.model]]` block has swallowed `[ai]` settings — it never drops them silently.

## Profiles

A **profile** is a named terminal identity: its own config overlay + its own saved
tabs/splits. Manage them entirely from the prompt:

```text
❯ @profile                           # list — ● marks the active one
❯ @profile current                   # the active id, one word (for scripts)
❯ @profile create "Work" 💼
❯ @profile work                      # switch directly (by id or name) — live in ~1 s
❯ @profile edit [work]               # open its config overlay in $EDITOR;
                                     # saving applies live (no settings UI, ever)
❯ @profile rename work "Work Stuff"
❯ @profile delete work
```

- Every profile (including the seeded `default`) owns
  `profiles/<id>/config.toml` — an all-commented template; only keys you uncomment
  override the global config. Different theme, different AI pool, different
  keybindings per profile — all TOML.
- Each profile's open tabs/splits (and each pane's working directory + zoom) persist
  to `profiles/<id>/workspace.toml` on quit and autosave, and restore on launch and
  on switch.
- The active profile shows as a chip in the status bar. `@profile switch` from *any*
  shell moves the on-disk pointer; the window polls it and follows live — saving the
  outgoing profile's workspace first.
- The saved workspace is the **whole window state**: tabs, splits, focus, per-pane
  zoom and working directory, each pane's terminal content with its styling
  (scrollback tail + screen as ANSI), the tab-bar orientation, and the window
  size — closing and reopening the terminal (or switching profiles) silently
  restores exactly what you left, colors included, with a fresh prompt under
  the restored text.

## i18n

Every user-facing string (window chrome + CLI output) resolves through the locale
catalog. `en` and `fr` ship in `builtin/i18n/`; select with `[appearance] locale`
(globally or per profile). Drop a partial `~/.aiTerminal/i18n/<locale>.toml` to
override individual keys, or a full file to add a language — missing keys fall
back to `en`, then to the key itself (never a blank UI). CI enforces that shipped
locales define exactly the same key set.
