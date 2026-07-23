# Keybindings

The default keymap is data — `builtin/keymaps/default.toml` — embedded in the
binary as the base layer. On top of it compose (later wins): plugin keybindings →
your `~/.aiTerminal/keymaps/*.toml` files → `[[keybinding]]` tables in
`config.toml`. Unbound chords fall through to the shell.

## Defaults (macOS-style, Cmd-led)

| Chord | Action |
| --- | --- |
| `Cmd+T` / `Cmd+W` | new tab / close tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | next / previous tab |
| `Cmd+1..9` | jump to tab N |
| `Cmd+P` / `Cmd+K` | tab quick-switcher |
| `Cmd+D` / `Cmd+Shift+D` | split right / split down |
| `Cmd+Shift+W` | close pane |
| `Cmd+Alt+←/→/↑/↓` | focus pane in direction |
| `Cmd+Enter` | zoom pane (toggle full) |
| `Cmd+C` / `Cmd+V` | copy / paste |
| `Enter` (with a mouse selection) | copy the selection instead of running the command |
| `Cmd+=` / `Cmd+-` / `Cmd+0` | pane font zoom in / out / reset |
| `Cmd+,` | reload config live |
| `Shift+PageUp/PageDown` | scroll page up / down |
| `Shift+↑/↓` | scroll line up / down |
| `Shift+Home/End` | scroll to top / bottom |

## Command-line editing (the lineedit plugin)

Modified navigation keys that are NOT bound above fall through to the shell as
standard xterm sequences, where the builtin **lineedit** plugin (zsh) makes
editing the command feel native:

| Keys | On the command line |
| --- | --- |
| `←/→` | move by character |
| `⌥←/→` (or `Ctrl+←/→`) | jump by word |
| `⌘←/→` | jump to line start / end |
| `⇧←/→` | select by character |
| `⇧⌥←/→` | select by word |
| `⇧⌘←/→` | select to line start / end |
| type / `⌫` / `⌦` over a selection | replace / delete it |
| `Esc` | cancel the selection |
| `⌘C` | copy the selection to the clipboard (OSC 52) |
| `⌥⌫` / `⌘⌫` | delete word / to line start |
| `Ctrl+Alt+←/→`, `⌥↑` | dir plugin: back / forward / parent dir |

The selection draws as a light neutral band with the text's own colors on
top, and any other key drops it, macOS-style. Because it's a plugin,
`@plugin disable lineedit` turns all of it off.

## Action names

For custom bindings (`snake_case`, case/sep-insensitive):

`new_tab` `close_tab` `next_tab` `prev_tab` `go_to_tab_1..9` `tab_switcher`
(alias `command_palette`) `split_right` `split_down` `close_pane` `focus_left`
`focus_right` `focus_up` `focus_down` `focus_next` `zoom_pane` `zoom_in_pane`
`zoom_out_pane` `reset_zoom` `cycle_tab_bar` `copy` `paste` `scroll_line_up`
`scroll_line_down` `scroll_page_up` `scroll_page_down` `scroll_top`
`scroll_bottom` `reload_config`

## Customizing

In `config.toml` (wins over everything):

```toml
[[keybinding]]
key    = "cmd+shift+enter"
action = "zoom_pane"
```

Or drop a keymap file in `~/.aiTerminal/keymaps/` (sorted, composed in order):

```toml
# ~/.aiTerminal/keymaps/mine.toml
name = "Mine"
[[keybinding]]
key    = "ctrl+alt+t"
action = "cycle_tab_bar"
```

Chords are `cmd`/`ctrl`/`alt`/`shift` + a key name (`a`-`z`, `0`-`9`, `enter`,
`tab`, `comma`, `equal`, `minus`, arrows, `pageup`, `home`, …) and match the keycap
on any layout.
