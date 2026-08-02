/* ============================================================================
   scenes.js — the scene builders shared by the site's showcase and the film:
   the Telegram phone that stands beside the terminal (@gate), the @md split
   editor, and the redactor's before/after view.

   These live here rather than in showcase.js because video.html tells the same
   story with the same pieces. One implementation, two stages — a claim the
   film makes is a claim the site makes, by construction.

   Depends on replica.js for `el`, `spanEl`, `sleep` and the span helpers.
   ========================================================================== */
/* Build the `@md edit` split view directly as DOM (a real two-column layout, so it stays
   pixel-aligned at any window width — no box-drawing to drift). Left = Markdown source with a
   line-number gutter and the divider column `mdedit/editor.rs` draws; right = the live rendered
   preview.

   The two halves must agree: the preview renders THIS source, boxed `A ──▶ B ──▶ C` from the
   `flowchart LR` beside it, exactly as `@md render` prints it. A preview that drew something
   its own Markdown does not describe would be a drawing of a feature, not the feature.

   The chrome is the production chrome (`mdedit/editor.rs`): a full-width status bar
   ` <path> ●  (<n>L)` with the last status message right-aligned, `~` for rows past the end of
   the buffer, and the real help line. */
function buildMdEditor(w) {
  const pane = w.pane();
  pane.innerHTML = "";
  const root = el("div", "rw-md");

  // Status bar: " release.md ●  (21L)" … "saved release.md"
  const bar = el("div", "rw-md-bar");
  bar.append(spanEl(S("t-fg", " release.md")), spanEl(S("t-error", " ●")),
    spanEl(S("t-muted", "  (21L)")));
  bar.appendChild(el("span", "rw-md-bar-r", "saved release.md"));

  const bodyRow = el("div", "rw-md-body");

  // Left: the raw Markdown source, with the gutter, then `~` past the end of the buffer.
  const ed = el("div", "rw-md-ed");
  /* The head of the very file `@md render` just printed — the same document,
     which is what makes the preview a preview rather than a second demo. */
  const src = [
    ["# Release 0.9 — the graph you can watch", "h1"], ["", ""],
    ["The exporter now speaks JSON, and `@flow`", "text"],
    ["draws itself while it runs.", "text"], ["", ""],
    ["## What changed", "h1"], ["", ""],
    ["- **Breaking**: `--format=csv` is now", "li"],
    ["  `--format csv`", "li"], ["", ""],
    ["```mermaid", "fence"], ["flowchart LR", "code"],
    ["  cut --> test --> tag --> publish", "code"], ["```", "fence"],
  ];
  src.forEach(([t, k], i) => {
    const row = el("div", "rw-md-row");
    row.append(el("span", "rw-md-gutter", String(i + 1)),
      el("span", "rw-md-" + (k || "code"), t || " "));
    ed.appendChild(row);
  });
  const tilde = el("div", "rw-md-row");
  tilde.append(el("span", "rw-md-gutter", ""), el("span", "rw-md-tilde", "~"));
  ed.appendChild(tilde);

  // Right: the rendered preview — the same document, drawn.
  const pv = el("div", "rw-md-pv");
  pv.appendChild(el("div", "rw-md-title", "Release 0.9 — the graph you can watch"));
  pv.appendChild(el("div", "rw-md-rule", "──────────────────────"));
  pv.appendChild(el("div", "rw-md-gap", ""));
  const para = el("div", "rw-md-para");
  para.append(el("span", null, "The exporter now speaks JSON, and "),
    el("span", "rw-md-inline", "@flow"), el("span", null, " draws itself while it runs."));
  pv.appendChild(para);
  pv.appendChild(el("div", "rw-md-gap", ""));
  pv.appendChild(el("div", "rw-md-title", "What changed"));
  pv.appendChild(el("div", "rw-md-rule", "──────────────────────"));
  pv.appendChild(el("div", "rw-md-gap", ""));
  const li = el("div", "rw-md-bullet");
  li.append(el("span", "mk", "\u2022"), el("span", null, " "), el("b", null, "Breaking"),
    el("span", null, ": "), el("span", "rw-md-inline", "--format=csv"),
    el("span", null, " is now "), el("span", "rw-md-inline", "--format csv"));
  pv.appendChild(li);
  pv.appendChild(el("div", "rw-md-gap", ""));
  /* The fence on the left, drawn on the right — a preview that showed something
     its own source does not describe would be a drawing of a feature. */
  const dg = el("div", "rw-md-diagram");
  ["cut", "test", "tag", "publish"].forEach((n, i) => {
    if (i) dg.appendChild(el("span", "rw-md-arrow", "\u2500\u2500\u25b6"));
    dg.appendChild(el("span", "rw-md-node", n));
  });
  pv.appendChild(dg);

  bodyRow.append(ed, pv);

  // The real help line, verbatim.
  const help = el("div", "rw-md-help");
  [["^S", "save"], ["^W", "focus:editor"], ["^Q", "quit"]].forEach(([k, v]) => {
    const g = el("span", "rw-md-key");
    g.append(el("b", null, k), el("span", null, " " + v));
    help.appendChild(g);
  });
  help.appendChild(el("span", "rw-md-hint", "\u00b7 scroll: \u2191\u2193 \u2190\u2192 \u00b7 wheel \u00b7 shift+wheel = horizontal"));

  root.append(bar, bodyRow, help);
  pane.appendChild(root);
}


/* ── the phone standing beside the terminal (@gate) ────────────────────────
   A scripted Telegram mock, not a chat client: the point of `@gate` is that the
   chat lives somewhere ELSE while the terminal keeps running on your desk, so
   the phone is built outside the terminal window and driven beat by beat from
   the demo script.

   The `/shot` reply is not a drawing of a terminal — it CLONES the live pane,
   carrying its theme variables across, so the photo in the chat is pixel-for-
   pixel whatever the terminal was showing at that moment. That is exactly what
   the real command does, and it keeps the two halves honest. */

const SVG_NS = "http://www.w3.org/2000/svg";
/* A monochrome line icon from one or more path strings. Built as real SVG nodes
   rather than markup, matching this file's no-innerHTML-for-content rule. */
function icon(paths, cls, size) {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", size || 15);
  svg.setAttribute("height", size || 15);
  if (cls) svg.setAttribute("class", cls);
  (Array.isArray(paths) ? paths : [paths]).forEach((d) => {
    const p = document.createElementNS(SVG_NS, "path");
    p.setAttribute("d", d);
    svg.appendChild(p);
  });
  return svg;
}

const ICONS = {
  clip: "M16 7v9.2a4.2 4.2 0 0 1-8.4 0V6.3a2.6 2.6 0 0 1 5.2 0v9.4a1 1 0 0 1-2 0V7.2H9.3v8.5a2.4 2.4 0 0 0 4.8 0V6.3a4 4 0 0 0-8 0v9.9a5.8 5.8 0 0 0 11.6 0V7H16z",
  mic: ["M12 3.2a2.5 2.5 0 0 1 2.5 2.5v5.6a2.5 2.5 0 0 1-5 0V5.7A2.5 2.5 0 0 1 12 3.2z",
        "M6.6 11.3a5.4 5.4 0 0 0 10.8 0h1.7a7.1 7.1 0 0 1-6.2 6.9v2.6h-1.8v-2.6a7.1 7.1 0 0 1-6.2-6.9h1.7z"],
  send: "M2.4 20.8 22 12 2.4 3.2l0 6.9 13.4 1.9-13.4 1.9z",
  smile: ["M12 2.4A9.6 9.6 0 1 0 12 21.6 9.6 9.6 0 0 0 12 2.4zm0 1.9a7.7 7.7 0 1 1 0 15.4 7.7 7.7 0 0 1 0-15.4z",
          "M8.7 9a1.3 1.3 0 1 0 0 2.6 1.3 1.3 0 0 0 0-2.6zm6.6 0a1.3 1.3 0 1 0 0 2.6 1.3 1.3 0 0 0 0-2.6z",
          "M7.4 13.6a5 5 0 0 0 9.2 0H7.4z"],
  wifi: ["M12 17.6l2.3-2.7a3.5 3.5 0 0 0-4.6 0L12 17.6z",
         "M12 10.3a8.8 8.8 0 0 0-6.2 2.5l1.6 1.7a6.6 6.6 0 0 1 9.2 0l1.6-1.7A8.8 8.8 0 0 0 12 10.3z"],
  back: "M14.7 5.3 8 12l6.7 6.7 1.4-1.4L10.8 12l5.3-5.3z",
};

/* Telegram renders a bot's `<b>` and `<code>` — and the real gate sends exactly those
   (see `gate/driver.rs`), so the demo's message text is the production text verbatim.
   `el` sets textContent, which is correct for this file's no-innerHTML-for-content rule
   but would print the tags literally, so the markers are parsed into real nodes here. */
const TAG_RE = /<(b|code)>([\s\S]*?)<\/\1>/g;
function rich(parent, text) {
  TAG_RE.lastIndex = 0; // a shared /g/ regex carries state between calls
  let at = 0;
  for (let m; (m = TAG_RE.exec(text)); ) {
    if (m.index > at) parent.appendChild(document.createTextNode(text.slice(at, m.index)));
    parent.appendChild(el(m[1], m[1] === "code" ? "ph-code" : null, m[2]));
    at = m.index + m[0].length;
  }
  if (at < text.length) parent.appendChild(document.createTextNode(text.slice(at)));
  return parent;
}

/* the 13 theme variables `setTheme` writes inline on the window root — copied
   onto a screenshot so the clone renders in the terminal's live colors */
const THEME_VARS = ["--t-bg", "--t-surface", "--t-fg", "--t-muted", "--t-accent",
  "--t-accent2", "--t-success", "--t-warn", "--t-error", "--t-cursor",
  "--t-selection", "--t-border", "--t-hover"];

function makePhone(host) {
  // No phone host on the page → the terminal half of the story still plays.
  if (!host) {
    const noop = () => {};
    return { type: async () => {}, send: noop, typing: noop, reply: noop, photo: noop, menu: noop,
      live: () => ({ update: noop }) };
  }
  host.hidden = false;
  host.innerHTML = "";

  const root = el("div", "ph");
  ["up", "dn", "pwr"].forEach((k) => root.appendChild(el("span", "ph-side " + k)));

  const screen = el("div", "ph-screen");

  /* iOS status bar: clock left, island centred, radios right */
  const sys = el("div", "ph-sys");
  const sigs = el("div", "ph-sig");
  [0, 1, 2, 3].forEach(() => sigs.appendChild(el("i")));
  const batt = el("div", "ph-batt");
  batt.appendChild(el("i"));
  const island = el("div", "ph-island");
  island.appendChild(el("span", "ph-cam"));
  const radios = el("div", "ph-sys-ic");
  radios.append(sigs, icon(ICONS.wifi, "ph-wifi", 12), batt);
  sys.append(el("span", "ph-sys-time", "9:41"), island, radios);

  /* the chat header */
  const head = el("div", "ph-head");
  const who = el("div", "ph-who");
  const status = el("div", "ph-status", "bot");
  who.append(el("div", "ph-name", "aiTerminal gate"), status);
  const dots = el("div", "ph-menu");
  dots.append(el("i"), el("i"), el("i"));
  head.append(icon(ICONS.back, "ph-back", 17), el("div", "ph-avatar", "A"), who, dots);

  const feed = el("div", "ph-feed");
  feed.appendChild(el("div", "ph-day", "today"));

  /* the composer */
  const bar = el("div", "ph-bar");
  const field = el("div", "ph-field");
  const input = el("div", "ph-input empty", "Message");
  field.append(icon(ICONS.smile, "ph-ic", 16), input, icon(ICONS.clip, "ph-ic", 16));
  const action = el("button", "ph-action");
  action.setAttribute("tabindex", "-1");
  action.setAttribute("aria-hidden", "true");
  action.appendChild(icon(ICONS.mic, "ph-ic", 16));
  bar.append(field, action);

  screen.append(sys, head, feed, bar, el("div", "ph-home"));
  root.append(screen, el("div", "ph-glare"));
  host.appendChild(root);

  const scroll = () => { feed.scrollTop = feed.scrollHeight; };
  /* A fixed wall clock, ticking a minute per exchange — a live clock would date
     any screenshot of this page. */
  let minute = 41;
  const stamp = () => `09:${String(minute++).padStart(2, "0")}`;

  /* swap the mic for a send button while there is text to send */
  function armed(on) {
    action.classList.toggle("send", on);
    action.replaceChildren(icon(on ? ICONS.send : ICONS.mic, "ph-ic", 16));
  }

  /* the shared bubble: body, an optional attachment, then time (+ read ticks) */
  function bubble(side, text, extra, cls) {
    const b = el("div", "ph-msg " + side + (cls ? " " + cls : ""));
    if (extra) b.appendChild(extra);
    if (text) b.appendChild(rich(el("div", "ph-body"), text));
    const meta = el("div", "ph-meta");
    meta.appendChild(el("span", null, stamp()));
    if (side === "me") meta.appendChild(el("span", "ph-tick", "✓✓"));
    b.appendChild(meta);
    feed.appendChild(b); scroll();
    return b;
  }

  return {
    /* type into the composer, the way a thumb would */
    async type(text) {
      input.classList.remove("empty");
      input.textContent = "";
      const caret = el("span", "ph-caret");
      input.appendChild(caret);
      armed(true);
      for (const ch of text) {
        caret.insertAdjacentText("beforebegin", ch);
        await sleep(36 + Math.random() * 26);
      }
      await sleep(240);
    },
    /* commit whatever was typed as an outgoing message */
    send(text) {
      input.replaceChildren();
      input.textContent = "Message";
      input.classList.add("empty");
      armed(false);
      return bubble("me", text);
    },
    /* the bot is composing — the line every chat app shows under the name */
    typing(on) {
      status.textContent = on ? "typing…" : "bot";
      status.classList.toggle("live", !!on);
    },
    reply(text, pre) {
      return bubble("bot", text, pre ? el("div", "ph-pre", pre) : null);
    },
    /* a bot's inline keyboard — the command buttons Telegram renders under a
       message, and the same list `@gate` publishes with setMyCommands */
    menu(text, keys) {
      const b = bubble("bot", text);
      const pad = el("div", "ph-keys");
      keys.forEach((k) => pad.appendChild(el("span", "ph-key", k)));
      b.appendChild(pad);
      scroll();
      return b;
    },
    /* The live screen of an attached program: ONE bubble, edited in place, with the
       program's current choices as buttons beneath it. Returns a handle so the demo
       updates that same bubble instead of posting another — which is exactly what the
       real gate does, and the reason a long session never buries the chat. */
    live(title, lines, buttons) {
      const b = el("div", "ph-msg bot live");
      const card = el("div", "ph-live");
      card.appendChild(el("div", "ph-live-title", title));
      const body = el("div", "ph-live-body");
      const pad = el("div", "ph-keys live");
      const paint = (ls, bs) => {
        body.replaceChildren();
        ls.forEach(([kind, text]) => body.appendChild(rich(el("div", "ph-live-line " + kind), text)));
        pad.replaceChildren();
        (bs || []).forEach((row) => {
          const r = el("div", "ph-keyrow");
          row.forEach((t) => r.appendChild(el("span", "ph-key", t)));
          pad.appendChild(r);
        });
      };
      paint(lines, buttons);
      card.appendChild(body);
      b.append(card, pad);
      const meta = el("div", "ph-meta");
      meta.appendChild(el("span", null, stamp()));
      b.appendChild(meta);
      feed.appendChild(b);
      scroll();
      return {
        update(nextLines, nextButtons) {
          paint(nextLines, nextButtons);
          // restart the highlight so an in-place edit is visibly an edit
          b.classList.remove("pulse");
          void b.offsetWidth;
          b.classList.add("pulse");
          scroll();
        },
      };
    },
    /* `/shot` — a real clone of the live pane, not an impression of one */
    photo(caption, w) {
      const shot = el("div", "ph-shot");
      const chrome = el("div", "ph-shot-bar");
      chrome.append(el("i"), el("i"), el("i"), el("span", "ph-shot-name", "aiTerminal"));

      const frame = el("div", "ph-shot-frame");
      const stage = el("div", "ph-shot-stage");
      // The pane renders with variables set inline on the window root; the clone
      // lives outside that subtree, so they travel with it.
      THEME_VARS.forEach((v) => stage.style.setProperty(v, w.root.style.getPropertyValue(v)));

      const pane = w.pane();
      const rect = pane.getBoundingClientRect();
      const clone = pane.cloneNode(true);
      clone.classList.remove("focused", "enter");
      // pin the clone to the pane's real box, so the miniature is a true
      // reduction rather than a reflow at a different width
      clone.style.width = (rect.width || 500) + "px";
      clone.style.height = (rect.height || 380) + "px";
      stage.appendChild(clone);
      frame.appendChild(stage);
      shot.append(chrome, frame);

      const b = bubble(null, caption, shot, "photo");

      // Scale the full-size clone down to the bubble, measured after layout so
      // it is right at any window width.
      const avail = frame.clientWidth || 172;
      const k = rect.width ? Math.min(1, avail / rect.width) : 0.34;
      stage.style.width = (rect.width || 500) + "px";
      stage.style.height = (rect.height || 380) + "px";
      stage.style.transform = `scale(${k})`;
      frame.style.height = Math.round((rect.height || 380) * k) + "px";
      scroll();
      return b;
    },
  };
}

/* ── the redactor: the same lines, before and after they leave ─────────────
   Two aligned columns so the eye can pair them line for line. The right-hand
   side is not invented: these are the exact strings the shipped `redactor`
   rules produce, including the cases where the `KEY=value` rule takes the key
   name with it (`ANTHROPIC_API_KEY=sk-…` → `ANTHROPIC_«redacted»`).

   The left column shows each secret as its PREFIX only. Nothing on this site is
   shaped like a real credential — a page about not leaking secrets should not be
   the thing a secret scanner has to think about. The third column carries the
   rule that matched, which is the part that actually documents anything. */
const REDACT_ROWS = [
  ["DATABASE_URL=postgres://db.internal/prod", "DATABASE_URL=postgres://db.internal/prod", ""],
  ["AWS_ACCESS_KEY_ID=AKIA…", "AWS_ACCESS_KEY_ID=«redacted»", "AKIA[0-9A-Z]{16}"],
  ["ANTHROPIC_API_KEY=sk-ant-…", "ANTHROPIC_«redacted»", "sk-[A-Za-z0-9_-]{16,}"],
  ["GITHUB_TOKEN=ghp_…", "GITHUB_«redacted»", "gh[pousr]_[A-Za-z0-9]{20,}"],
  ["LOG_LEVEL=debug", "LOG_LEVEL=debug", ""],
];

function buildRedactView(w) {
  const root = el("div", "rw-red");

  const grid = el("div", "rw-red-grid");
  grid.append(
    el("div", "rw-red-head", "on your screen"),
    el("div", "rw-red-head mid", ""),
    el("div", "rw-red-head out", "what leaves your machine")
  );

  REDACT_ROWS.forEach(([before, after, rule]) => {
    const hit = before !== after;
    const l = el("div", "rw-red-cell" + (hit ? " secret" : " plain"));
    l.textContent = before;
    const mid = el("div", "rw-red-arrow", hit ? "→" : "");
    const r = el("div", "rw-red-cell" + (hit ? " safe" : " plain"));
    r.textContent = after;
    if (hit) r.title = "matched " + rule;
    grid.append(l, mid, r);
  });

  const foot = el("div", "rw-red-foot");
  foot.append(
    el("span", "rw-red-lock", "🔒"),
    el("span", null, "9 rules · redactor plugin · scope "),
    el("b", null, "ai"),
    el("span", "rw-red-note", " — the values stay intact on your own screen")
  );

  root.append(grid, foot);
  w.pane().appendChild(root);
}
