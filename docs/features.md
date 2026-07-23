# Features

The window is a pure, fast terminal; the AI lives behind `@`-commands (see
[ai.md](ai.md)).

## Tabs & splits

- `Cmd+T` new tab · `Cmd+W` close tab · `Cmd+1..9` jump · `Ctrl+Tab` /
  `Ctrl+Shift+Tab` cycle · drag tabs to reorder.
- `Cmd+D` split right · `Cmd+Shift+D` split down · `Cmd+Alt+arrows` move focus ·
  `Cmd+Enter` zoom a pane to full · `Cmd+Shift+W` close pane.
- The tab bar sits top/bottom/left/right (`[behavior] tab_bar`, or cycle live) and
  appears only with 2+ tabs. Tabs are numbered and named by program or folder
  (`3 - 🖥 Terminal [my-project][zsh]`).

## The quick-switcher (`Cmd+P` / `Cmd+K`)

An overlay listing every tab with its folder (and remote host over SSH). Type a
number or filter text, arrows + Enter to jump.

## Terminal ergonomics

- Scrollback (configurable depth): wheel, `Shift+PageUp/Down`, `Shift+Home/End`;
  typing snaps back to live.
- Multi-click selection: click/word/line escalation; drag to extend; `Cmd+C` copies,
  right-click copies, middle-click pastes. `Enter` with a live mouse selection
  copies it instead of running the command.
- **Command-line editing like a text field** (the `lineedit` plugin): `⌥/⌘ arrows`
  jump by word / to the line ends, `⇧`+arrows select (light neutral band, text
  keeps its colors), typing replaces the selection, `⌫`/`⌦` delete it, `Esc`
  cancels, `⌘C` copies it — see [keybindings.md](keybindings.md).
- Cursor styles: `block` (default), `bar`, `underline` — `[appearance]
  cursor_style`. Steady (no blink), ghost-free rendering with burst-settled
  presents (a mid-repaint frame is never shown).
- **OSC 52 clipboard**: any program (tmux, remote vim over SSH, the lineedit
  plugin) can SET the system clipboard via `OSC 52`; clipboard *reads* are
  refused, so nothing can spy on it.
- **⌘-click to open**: hold ⌘ and hover — URLs and existing file/folder paths under
  the pointer underline; click opens them with the OS (browser / default app /
  Finder). Handles paths with spaces and non-ASCII names, and uses the shell's live
  cwd for relative paths.
- Per-pane font zoom (`Cmd +/-/0`), window-level scale, any monospace font.
- A shell that exits closes its pane/tab cleanly; the last one closes the window.

## Status bar

Plugin-driven segments (git branch + state, cwd, user@host, clock, …) along the
bottom, updating instantly on tab switch / `cd` (OSC 7), plus the active profile
chip. Over SSH it shows the remote path + host.

## Themes

`@theme` lists, `@theme <name>` switches the current profile\x27s theme — the
window restyles live (see [theme.md](theme.md)).

## Profiles

Named terminal identities — per-profile config overlay + saved workspace, managed
with `@profile` and applied live. See [configuration.md](configuration.md#profiles).

## Session persistence

The active profile's whole window state — tabs, splits, focus, per-pane zoom and
folder, tab-bar orientation, window size, **and each pane's terminal content
with its colors** (scrollback tail + screen, styling preserved as ANSI, up to
1000 lines) — saves on quit (plus a periodic autosave), then restores silently
on launch and on profile switch: the reopened pane shows exactly the session
you left, colors included, with a fresh prompt right below. Processes
themselves are not resumed (the tmux-resurrect model); shells relaunch in
their saved directories and the window reopens at its saved size.

## i18n

The terminal's own strings (chrome + CLI) are localizable TOML
(`builtin/i18n/<locale>.toml`, en + fr shipped; `[appearance] locale`, per-profile
overridable). See [configuration.md](configuration.md#i18n).

## Shell integration

Non-destructive zsh/bash integration (your rc always loads first) injecting: plugin
aliases + abbreviations, declarative tab-completions, theme-matched `ls` colors and
prompt colors, and the `@`-command handlers. One switch: `[shell] integration`.
See [shell.md](shell.md).

## Security

A command guard (allow/confirm/deny) over AI-suggested commands and agent `sys.run`,
plus scoped secret redaction (terminal display / AI egress). See
[security.md](security.md).
