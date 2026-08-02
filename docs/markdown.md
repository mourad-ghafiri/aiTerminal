# Markdown

Everything that renders Markdown in aiTerminal — `@md render`, the `@md edit` preview, and
every AI answer — goes through one engine in `corelib::md`. It reads GitHub-flavored
Markdown *and* the HTML subset GitHub allows, so a real README renders as a document rather
than as syntax.

"Every AI answer" includes the ones you read back later: a flow's answer, `@flow node`,
and `@flow log` / `@loop log` / `@job log`. Those files are documents on disk, and one
rule decides how each is shown — **Markdown is what an agent writes**. A command's output
is not, so it is passed through untouched rather than re-wrapped, and a `#` line printed by
a program stays a comment. Rendering is for the person at the terminal: **a pipe always
gets the source unchanged**, which is what keeps `@flow review "…" > review.md` honest.

```sh
@md render README.md      # the whole file, drawn
@md edit notes.md         # source left, live preview right
```

## What is supported

| Group | Elements |
| --- | --- |
| Headings | `#`…`######`, and the setext spellings (`===` / `---` underlines) |
| Text | bold, italic, strikethrough, inline code, hard line breaks (two trailing spaces or `\`), backslash escapes, HTML entities (`&amp;` `&#8212;` `&#x1F600;`), `:emoji:` shortcodes |
| Links | inline, reference (`[t][id]`, `[t][]`, `[id]`), bare URLs (`https://…`, `www.…`), `<autolinks>`, `mailto:` — clickable via `OSC 8` where the terminal supports it |
| Images | `![alt](src "title")`, reference images, `<img>` — **drawn as pixels** (see below) |
| Lists | bullet, ordered (with `start`), task lists (`- [x]`), nesting, loose/tight spacing |
| Quotes | `>` with lazy continuation, and the five GFM alerts: `> [!NOTE]` `[!TIP]` `[!IMPORTANT]` `[!WARNING]` `[!CAUTION]` |
| Code | fenced (with an info string) and indented; **syntax highlighted** for ~30 languages |
| Tables | GFM pipe tables with alignment and escaped `\|`; cells wrap instead of being cut |
| Footnotes | `[^1]` references and their definitions, collected into a section at the end |
| Math | `$inline$`, `$$display$$` and ```` ```math ```` blocks, kept verbatim |
| Other | thematic breaks, YAML/TOML front matter (stripped), `%%`-free HTML comments (dropped) |

## The HTML subset

Tags are read by a front-end that maps them onto the *same* tree the Markdown parser
produces — the renderer never sees a tag.

| | Tags |
| --- | --- |
| Inline | `b` `strong` `i` `em` `del` `s` `u` `ins` `mark` `code` `tt` `samp` `kbd` `sub` `sup` `a` `img` `br` `span` `small` `abbr` `cite` `q` |
| Block | `details`/`summary`, `div`/`p` (with `align`), `center`, `table`/`tr`/`th`/`td`, `ul`/`ol`/`li`, `dl`/`dt`/`dd`, `h1`–`h6`, `pre`, `blockquote`, `hr`, `picture`, `section` `article` `header` `footer` `main` `aside` `figure` |

Two rules keep this predictable:

- **Containers are transparent.** A `<div>`'s contents go back through the Markdown
  scanner, so the near-universal README opening — a centered `<div>` wrapped around a
  logo, badges and headings — renders as centered Markdown.
- **Unknown tags degrade to their text**, and `<script>`, `<style>`, `<iframe>` and their
  kin are dropped whole, content included.

## Images

An image that stands alone in its block — a logo, a screenshot, a row of badges — is
**drawn as real pixels** inside aiTerminal, over reserved grid rows (the same mechanism
diagrams use). Everywhere else it renders as `▣ alt` with its source.

- **Local files** always draw. A relative path resolves against the document's own
  directory, the way the document meant it.
- **Remote images** (`https://…`) are fetched only when you opt in:

  ```toml
  [md]
  remote_images  = true   # default false — rendering a document never reaches the network on its own
  image_max_rows = 20     # the tallest an image may be drawn
  syntax         = true   # highlight fenced code
  ```

  Downloads are size-capped and cached under `~/.aiTerminal/cache/images/`.

## How it works

Three stages, mirroring the diagram engine:

1. **Parse** — `md/parse.rs` (blocks and inlines) and `md/html.rs` (the tag subset) both
   produce one `Block`/`Inline` tree. A prepass lifts link reference definitions and
   footnote bodies out of the document, so a reference resolves wherever it is defined —
   including below the text that uses it.
2. **Render** — `md/render.rs` turns the tree into styled ANSI, wrapped to the pane's real
   width. With `Style::enabled = false` (a pipe) it emits clean plain text, no escapes.
   `md/code.rs` colors code fences from the same theme tokens.
3. **Draw** — the streaming renderer hands images to the host as their own chunk, so the
   app can draw pixels while every other terminal prints the placeholder.

Rendering is **streaming**: `md/stream.rs` emits each block the moment it is complete, which
is what makes an AI answer appear as it is written instead of after it is finished. Fenced
blocks and multi-line HTML elements are held together until they close.

## Limits

- Colors declared in HTML (`style="color: …"`) are ignored; a document always wears the
  active terminal theme, so it stays readable in light and dark and follows `@theme`.
- Math is displayed verbatim rather than typeset.
- Nested block structures are bounded (32 levels), and every list is size-capped: a hostile
  document can't exhaust memory or the stack.
