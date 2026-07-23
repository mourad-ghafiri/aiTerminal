# Themes

A theme is one TOML file of **semantic tokens** — `bg`, `surface`, `fg`, `muted`,
`accent`, `success`, `warn`, `error` — plus the terminal ANSI palette (`[ansi]`) and
file-type colors (`[files]`, which drive the theme-matched `ls` colors). Everything
in the window (chrome, panes, status bar, switcher) and the shell (prompt colors,
`LS_COLORS`) derives from those tokens.

## Using

```text
❯ @theme                 # list themes (● marks the active one)
❯ @theme nord            # switch the CURRENT profile's theme — the window
                         # restyles live within a second
❯ @theme export midnight # print a theme's COMPLETE normalized TOML
```

`@theme <name>` writes the active profile's config overlay, so every profile
keeps its own look (`@profile switch` restyles accordingly). The window follows
config-file changes each second — the same applies if you hand-edit
`[appearance] theme` in any config TOML.

The switch is FULLY live: the window chrome and all pane content (indexed ANSI
colors resolve through the live theme) repaint immediately, and **running
shells recolor at their next prompt** — the integration sources theme colors
from `~/.aiTerminal/shell/colors.sh` (rewritten on every switch) rather than
baking them into the environment, so the prompt, `@command` highlighting, and
subsequent `ls` output all follow. Only text already printed with truecolor
escapes keeps its original colors (it is literal output).

## The collection

`midnight` (default), `graphite`, `alpine`, `deep-purple`, `pink`, `product-red`,
`gold`, `sunset`, `cosmic-orange`, `sage`, `lavender`, `mist-blue`, `titanium`,
`solar-flare`, `nebula`, `coral`, and the light themes `sky-blue`, `light-gold`,
`starlight` — all data files under `builtin/themes/`.

## Custom themes

Copy any `*.toml` into `~/.aiTerminal/themes/` (user files win over bundled names).
Start from a full reference:

```sh
aiTerminal theme export midnight > ~/.aiTerminal/themes/mine.toml
```

Missing tokens are derived (hover/border/depth shades, nearest-ANSI mappings), so a
minimal file with just the core roles is valid.
