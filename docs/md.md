# `@md` — read & edit Markdown

Two commands turn any `.md` file into something you can actually read and edit at the prompt —
using the same from-scratch Markdown + diagram engines that render `@ai` answers.

## `@md render <file>`

Pretty-prints a Markdown file straight into the pane: headings, **bold**/*italic*, `inline code`,
links, bullet/numbered/task lists, block quotes, boxed fenced code, and aligned tables — wrapped to
the **full width of the split**. Diagrams are **drawn natively** as pixels inline (flowcharts and
sequence diagrams); in other terminals or when piped they fall back to a clean labelled box, and
piped output is plain Markdown so `@md render notes.md | less` stays clean.

```
❯ @md render README.md
```

## `@md edit <file>`

A full-screen **split editor**: the raw Markdown on the left, a **live rendered preview** on the
right that updates as you type — diagrams and all. Opening a path that doesn't exist starts an empty
buffer that's created on first save.

```
❯ @md edit notes.md
```

```
┌ notes.md ● (42L) ──────────────────────────── saved notes.md ┐
│  1 # Release plan          │ Release plan                     │
│  2                         │ ────────────                     │
│  3 Steps to ship:          │ Steps to ship:                   │
│  4                         │                                   │
│  5 - cut the branch        │  • cut the branch                │
│  6 - run the suite         │  • run the suite                 │
│  7                         │                                   │
│  8 ```mermaid              │  ╭──────── (native diagram) ────╮ │
│  9 flowchart LR            │  │  [branch] → [test] → [ship]  │ │
│ 10   A-->B-->C             │  ╰──────────────────────────────╯ │
│ ^S save  ^W focus:editor  ^Q quit  ·  scroll: ↑↓ ←→ · wheel   │
└───────────────────────────────────────────────────────────────┘
```

### Keys

| Key | Action |
| --- | --- |
| type / `Enter` / `Backspace` / `Delete` / `Tab` | edit the Markdown (Tab inserts spaces) |
| `←→↑↓` `Home` `End` `PageUp/Down` | move the caret (editor) or scroll (preview) |
| `Ctrl+W` | toggle keyboard focus between editor and preview |
| **mouse wheel** | scroll the pane under the pointer; **Shift+wheel** scrolls horizontally |
| **click** | focus a pane; in the editor, place the caret |
| `Ctrl+S` | save |
| `Ctrl+Q` / `Ctrl+C` / `Esc` | quit — if there are unsaved changes, choose **save / discard / cancel** |

Both panes scroll **vertically and horizontally**, by keyboard and by mouse — so wide tables and
long code lines are always reachable.

Inside aiTerminal the preview's diagrams render as real pixels and the mouse works throughout
(aiTerminal reports mouse events to full-screen programs, so `vim`/`less` get the mouse too). In a
third-party terminal the mouse is handled by that terminal and diagrams show as boxes.
