/* ============================================================================
   showcase.js — one complete aiTerminal window, driven by the feature menu.
   Click a feature → the window performs it ONCE and rests (themes restyle it,
   the tab bar docks to another edge, panes split, the switcher drops over it,
   profiles swap its whole identity…). Nothing loops, nothing is scroll-
   triggered; ↻ replays the current feature.
   ========================================================================== */

document.addEventListener("DOMContentLoaded", () => {
  const host = document.getElementById("showcase-window");
  const captionEl = document.getElementById("showcase-caption");
  const phoneEl = document.getElementById("showcase-phone");
  if (!host || !captionEl) return;

  let current = null;
  let epoch = 0; // a rebuilt window invalidates any in-flight script

  function fresh(opts) {
    epoch++;
    // `makeWindow` wipes the window host, but the phone sits OUTSIDE it, so it
    // has to be torn down here or it would linger over the next feature.
    if (phoneEl) {
      phoneEl.hidden = true;
      phoneEl.innerHTML = "";
    }
    return makeWindow(host, opts);
  }

  async function run(w, myEpoch, steps) {
    for (const st of steps) {
      if (myEpoch !== epoch) return;
      switch (st.do) {
        case "cmd": await typeCmd(w, st.text, st); break;
        case "out": w.line(st.spans, st.paneIdx); await sleep(st.ms || 120); break;
        case "stream": await streamLine(w, st.spans, st); break;
        case "think":
          await streamLine(w, [DIM(st.text)], { paneIdx: st.paneIdx, speed: 9, prefix: [DIM("∴ ")] });
          break;
        case "tool":
          w.line([DIM(`  ⚙ ${st.name} ${st.args} · ${st.ms}ms · ${st.size}`)], st.paneIdx);
          await sleep(st.wait || 300);
          break;
        case "spin": await spinner(w, st.label || "thinking…", st.ms || 900, st); break;
        case "footer":
          w.line([S("t-success", st.glyph || "✓"), DIM(" " + st.text)], st.paneIdx);
          break;
        case "pause": await sleep(st.ms); break;
        case "call": await st.fn(w); break;
      }
    }
  }

  const resting = (w, cwd = "~/project") => {
    w.line([ACC(cwd + " "), ACC2("❯ "), FG("git status -sb")]);
    w.line([OK("## main...origin/main")]);
  };

  const caption = (title, text, extra = "") => {
    captionEl.innerHTML =
      `<h3>${title}</h3><p>${text}</p>${extra ? `<div class="cap-extra">${extra}</div>` : ""}`;
  };

  /* Build the `@md edit` split view directly as DOM (a real two-column layout, so it stays
     pixel-aligned at any window width — no box-drawing to drift). Left = Markdown source with a
     line-number gutter; right = the live rendered preview, native diagram included. */
  function buildMdEditor(w) {
    const pane = w.pane();
    pane.innerHTML = "";
    const root = el("div", "rw-md");

    const bar = el("div", "rw-md-bar");
    bar.append(spanEl(S("t-fg", "release.md")), spanEl(S("t-error", " ●")),
      spanEl(S("t-muted", "  (10L)")));
    const barR = el("span", "rw-md-bar-r", "saved ✓");
    bar.appendChild(barR);

    const bodyRow = el("div", "rw-md-body");

    // Left: raw Markdown source with a gutter.
    const ed = el("div", "rw-md-ed");
    const src = [
      ["# Release plan", "h1"], ["", ""],
      ["- cut the branch", "li"], ["- run the suite", "li"], ["", ""],
      ["```mermaid", "fence"], ["flowchart LR", "code"], ["  A --> B --> C", "code"], ["```", "fence"],
    ];
    src.forEach(([t, k], i) => {
      const row = el("div", "rw-md-row");
      row.append(el("span", "rw-md-gutter", String(i + 1)),
        el("span", "rw-md-" + (k || "code"), t || " "));
      ed.appendChild(row);
    });

    // Right: the rendered preview.
    const pv = el("div", "rw-md-pv");
    pv.appendChild(el("div", "rw-md-title", "Release plan"));
    pv.appendChild(el("div", "rw-md-rule", "────────────"));
    pv.appendChild(el("div", "rw-md-gap", ""));
    ["cut the branch", "run the suite"].forEach((s) => {
      const li = el("div", "rw-md-bullet");
      li.append(el("span", "mk", "•"), el("span", null, " " + s));
      pv.appendChild(li);
    });
    const dg = el("div", "rw-md-diagram");
    ["branch", "test", "ship"].forEach((n, i) => {
      if (i) dg.appendChild(el("span", "rw-md-arrow", "→"));
      dg.appendChild(el("span", "rw-md-node", n));
    });
    pv.appendChild(dg);

    bodyRow.append(ed, pv);

    const help = el("div", "rw-md-help");
    [["^S", "save"], ["^W", "focus"], ["^Q", "quit"]].forEach(([k, v]) => {
      const g = el("span", "rw-md-key");
      g.append(el("b", null, k), el("span", null, " " + v));
      help.appendChild(g);
    });
    help.appendChild(el("span", "rw-md-hint", "· scroll ↑↓ ←→ · mouse wheel"));

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
      if (text) b.appendChild(el("div", "ph-body", text));
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
          ls.forEach(([kind, text]) => body.appendChild(el("div", "ph-live-line " + kind, text)));
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
     name with it (`ANTHROPIC_API_KEY=sk-…` → `ANTHROPIC_«redacted»`). */
  const REDACT_ROWS = [
    ["DATABASE_URL=postgres://db.internal/prod", "DATABASE_URL=postgres://db.internal/prod", ""],
    ["AWS_ACCESS_KEY_ID=AKIA3RJHF2P9QLXMZB4T", "AWS_ACCESS_KEY_ID=«redacted»", "AKIA[0-9A-Z]{16}"],
    ["ANTHROPIC_API_KEY=sk-ant-api03-9Fk2LmQ7xTvB", "ANTHROPIC_«redacted»", "sk-[A-Za-z0-9_-]{16,}"],
    ["GITHUB_TOKEN=ghp_8sK2mVx91QpLzR4tYnB7wDe3Fg", "GITHUB_«redacted»", "gh[pousr]_[A-Za-z0-9]{20,}"],
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


  /* ---------------- features ---------------- */
  const FEATURES = {

    themes: {
      caption() {
        const chips = THEME_ORDER.map((k) => {
          const t = THEMES[k];
          return `<button class="swdot${k === "midnight" ? " active" : ""}" data-chip-theme="${k}"
            style="--chip:${t.accent}" title="${t.name}"></button>`;
        }).join("");
        caption("Themes switch <em>everything</em>, live",
          "One TOML per theme — chrome, pane content, status bar and your <b>running shells</b> restyle instantly. Click a swatch:",
          chips);
      },
      demo(w, myEpoch) {
        resting(w);
        run(w, myEpoch, [
          { do: "pause", ms: 400 },
          { do: "cmd", text: "@theme nebula" },
          { do: "call", fn: async (t) => t.setTheme("nebula") },
          { do: "out", spans: [DIM("theme 'nebula' applied — running shells restyle too")] },
        ]);
        captionEl.querySelectorAll("[data-chip-theme]").forEach((chip) => {
          chip.onclick = () => {
            if (myEpoch !== epoch) return;
            w.setTheme(chip.dataset.chipTheme);
            w.line([ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), S("t-accent-b", "@theme"), ACC2(" " + chip.dataset.chipTheme)]);
            captionEl.querySelectorAll("[data-chip-theme]").forEach((c) => c.classList.toggle("active", c === chip));
          };
        });
      },
    },

    splits: {
      caption: () => caption("Split, focus, zoom",
        "<kbd>⌘D</kbd> right · <kbd>⌘⇧D</kbd> down — every pane its own shell and scrollback; the focused one wears the accent frame."),
      opts: { title: "aiTerminal", tabs: [{ title: "Terminal [web][zsh]", active: true }, { title: "Terminal [api][zsh]" }] },
      demo(w, myEpoch) {
        w.line([ACC("~/web"), OK(" ⎇ main"), ACC2(" ❯ "), FG("npm run dev")]);
        w.line([OK("  ➜  ready "), FG("on http://localhost:3000")]);
        run(w, myEpoch, [
          { do: "pause", ms: 600 },
          { do: "call", fn: async (t) => { t.note("⌘D — split right"); await sleep(600); t.split(false); } },
          { do: "cmd", text: "cargo watch -x test", paneIdx: 1 },
          { do: "out", spans: [OK("    Finished"), FG(" test — 507 passed")], paneIdx: 1 },
          { do: "pause", ms: 700 },
          { do: "call", fn: async (t) => { t.note("⌘⇧D — split down"); await sleep(600); t.split(true); } },
          { do: "cmd", text: "tail -f api.log", paneIdx: 2 },
          { do: "out", spans: [MUT("GET /health 200 · 2ms")], paneIdx: 2 },
        ]);
      },
    },

    tabdock: {
      caption: () => caption("The tab bar docks anywhere",
        "<code>[behavior] tab_bar</code> — move <b>this window's</b> tab bar:",
        `<button class="chip active" data-dock="top">top</button>
         <button class="chip" data-dock="bottom">bottom</button>
         <button class="chip" data-dock="left">left</button>
         <button class="chip" data-dock="right">right</button>`),
      opts: { tabs: [{ title: "Terminal [web][zsh]", active: true }, { title: "Terminal [api][zsh]" }, { title: "vim · parser.rs" }], tabpos: "top" },
      demo(w, myEpoch) {
        resting(w);
        run(w, myEpoch, [
          { do: "pause", ms: 500 },
          { do: "out", spans: [DIM("tab_bar = top (the default) — click a side above: a running window re-docks live")] },
        ]);
        captionEl.querySelectorAll("[data-dock]").forEach((b) => {
          b.onclick = () => {
            if (myEpoch !== epoch) return;
            w.setTabpos(b.dataset.dock);
            w.line([ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), S("t-accent-b", "@config"), ACC2(" set behavior.tab_bar " + b.dataset.dock)]);
            captionEl.querySelectorAll("[data-dock]").forEach((c) => c.classList.toggle("active", c === b));
          };
        });
      },
    },

    switcher: {
      caption: () => caption("<kbd>⌘P</kbd> — every tab, one overlay",
        "Type a number or a few letters of a title <b>or path</b> and jump."),
      opts: { tabs: [{ title: "Terminal [web][zsh]", active: true }, { title: "Terminal [api][zsh]" }, { title: "vim · parser.rs" }, { title: "Terminal [infra][zsh]" }] },
      demo(w, myEpoch) {
        resting(w, "~/web");
        const rows = [
          { title: "Terminal [web][zsh]", detail: "~/web" },
          { title: "Terminal [api][zsh]", detail: "~/work/api" },
          { title: "vim · parser.rs", detail: "~/web" },
          { title: "Terminal [infra][zsh]", detail: "deploy@prod ~/infra" },
        ];
        run(w, myEpoch, [
          { do: "pause", ms: 500 },
          { do: "call", fn: async (t) => { t.note("⌘P"); await sleep(550); t.openSwitcher("", rows, 0); } },
          { do: "pause", ms: 1000 },
          { do: "call", fn: async (t) => t.openSwitcher("ap", rows, 1) },
          { do: "pause", ms: 1100 },
          { do: "call", fn: async (t) => {
              t.closeSwitcher();
              t.setTabs([{ title: "Terminal [web][zsh]" }, { title: "Terminal [api][zsh]", active: true }, { title: "vim · parser.rs" }, { title: "Terminal [infra][zsh]" }]);
              t.defaultStatus({ cwd: "~/work/api" });
              t.line([ACC("~/work/api"), OK(" ⎇ main"), ACC2(" ❯ "), S("t-cursor", "")]);
            } },
        ]);
      },
    },

    profiles: {
      caption: () => caption("Whole identities, one command",
        "<code>@profile work</code> swaps theme, AI models, plugins <b>and your saved tabs</b> — live, in a second."),
      demo(w, myEpoch) {
        resting(w);
        run(w, myEpoch, [
          { do: "pause", ms: 400 },
          { do: "cmd", text: "@profile" },
          { do: "out", spans: [FG("profiles in ~/.aiTerminal/profiles (3):")] },
          { do: "out", spans: [ACC("  ● 🚀 Default          (default)")] },
          { do: "out", spans: [MUT("  ○ 💼 Work             (work)")] },
          { do: "out", spans: [MUT("  ○ 🌙 Night Ops        (night-ops)")] },
          { do: "pause", ms: 900 },
          { do: "cmd", text: "@profile work" },
          { do: "call", fn: async (t) => {
              t.setTheme("graphite");
              t.setTitle("aiTerminal");
              t.defaultStatus({ profile: "💼 Work", cwd: "~/work/api" });
              t.setTabs([{ title: "Terminal [api][zsh]", active: true }, { title: "Terminal [infra][zsh]" }]);
            } },
          { do: "out", spans: [FG("switched to profile 'work' — theme, config, models and your saved tabs, in one second")] },
        ]);
      },
    },

    ai: {
      caption: () => caption("<code>@ai</code> — ask, review, run",
        "One guarded command per request, preloaded at your prompt. Risky → ⚠ confirm. Catastrophic → blocked."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "cmd", text: "@ai list every port something is listening on" },
          { do: "spin", label: "thinking…", ms: 1000 },
          { do: "out", spans: [ACC("❯ "), DIM("press Enter to run (or edit)")] },
          { do: "out", spans: [ACC("❯ "), ACC2("lsof -iTCP -sTCP:LISTEN -n -P"), S("t-cursor", "")] },
          { do: "pause", ms: 1200 },
          { do: "cmd", text: "@ai what does this diagram show? @design/arch.png" },
          { do: "out", spans: [DIM("  📎 arch.png → vision block (1.2 MB)")] },
          { do: "spin", label: "thinking…", ms: 1000 },
          { do: "stream", spans: [FG("A four-layer architecture: corelib → platform → framework → app.")], speed: 11 },
        ]);
      },
    },

    agent: {
      caption: () => caption("<code>@coder</code> — a full agent at your prompt",
        "Live reasoning, tool traces, streaming answer, token footer — a complete harness, no app switch."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "cmd", text: "@coder \"fix the failing parser test\"" },
          { do: "out", spans: [ACC("✦ @coder"), MUT(" · claude-opus-4-8")] },
          { do: "spin", label: "thinking…", ms: 1100 },
          { do: "think", text: "The test expects a trailing newline — the parser drops it on the last line…" },
          { do: "tool", name: "fs.search", args: '{"q":"parse_flow"}', ms: 18, size: "2.1KB" },
          { do: "tool", name: "fs.edit", args: '{"path":"src/parser.rs"}', ms: 6, size: "412B" },
          { do: "tool", name: "sys.run", args: '{"cmd":"cargo test parser"}', ms: 2100, size: "1.4KB" },
          { do: "stream", spans: [FG("The fix: the parser dropped the final line — added the flush in "), ACC2("parse_flow()"), FG(".")], speed: 11 },
          { do: "footer", text: "8.4s · 3 tools · 12.3k in / 1.8k out" },
        ]);
      },
    },

    flow: {
      caption: () => caption("<code>@flow</code> — pipelines of specialists",
        "Explore → implement → verify, each step its own agent, chained. Free text runs the default pipeline."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "cmd", text: "@flow add retry logic to the fetch helper" },
          { do: "out", spans: [FG("▶ flow 'implement' — 3 step(s)")] },
          { do: "out", spans: [MUT("  1. explore (@explorer)   2. implement (@coder)   3. verify (@tester)")] },
          { do: "spin", label: "explore…", ms: 1000 },
          { do: "out", spans: [OK("✓"), DIM(" explore · 1.2k in / 340 out")] },
          { do: "spin", label: "implement…", ms: 1200 },
          { do: "out", spans: [OK("✓"), DIM(" implement · 6.4k in / 1.1k out")] },
          { do: "spin", label: "verify…", ms: 900 },
          { do: "out", spans: [OK("✓"), DIM(" verify · 2.1k in / 280 out")] },
          { do: "stream", spans: [FG("Added exponential backoff (3 attempts) — tests green.")], speed: 11 },
        ]);
      },
    },

    loop: {
      caption: () => caption("<code>@loop</code> — until it's <em>actually</em> done",
        "An independent verifier grades every iteration — the model never grades its own work."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "cmd", text: "@loop \"make the config tests pass\" --check \"cargo test config\"" },
          { do: "out", spans: [FG("🔁 loop 'coder' — up to 5 iteration(s)")] },
          { do: "out", spans: [FG("▶ iteration 1/5")] },
          { do: "tool", name: "fs.edit", args: '{"path":"src/config.rs"}', ms: 7, size: "610B" },
          { do: "out", spans: [DIM("  check: exit=1 · assertion failed: default theme")], ms: 500 },
          { do: "out", spans: [FG("▶ iteration 2/5")] },
          { do: "tool", name: "fs.edit", args: '{"path":"src/config.rs"}', ms: 5, size: "188B" },
          { do: "out", spans: [DIM("  check: exit=0")], ms: 400 },
          { do: "out", spans: [OK("✓ goal reached after 2 iteration(s)")] },
        ]);
      },
    },

    job: {
      caption: () => caption("<code>@job</code> — background work that survives you",
        "<code>--bg</code> detaches fully, statuses never lie, and every job keeps a log you can <code>tail -f</code>."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "cmd", text: "@job audit the deps --agent reviewer --bg" },
          { do: "out", spans: [FG("▶ background job 1753112000-4242")] },
          { do: "out", spans: [DIM("  monitor: @job  ·  tail -f ~/.aiTerminal/ai/jobs/…/log.md")] },
          { do: "pause", ms: 1000 },
          { do: "cmd", text: "@job" },
          { do: "out", spans: [FG("background jobs (3):")] },
          { do: "out", spans: [FG("  ▶ 1753112000-4242 running   audit the deps … "), DIM("(2m ago · 2m)")] },
          { do: "out", spans: [OK("  ✓ "), FG("1753111800-4101 done      create a CHANGELOG … "), DIM("(9m ago · 45s)")] },
          { do: "out", spans: [MUT("  ⏹ "), FG("1753110900-3980 cancelled refactor the CLI … "), DIM("(24m ago · 12s)")] },
        ]);
      },
    },

    md: {
      opts: { title: "release.md — @md edit", tabs: [{ title: "release.md [@md edit]", active: true }] },
      caption: () => caption("<code>@md</code> — read &amp; live-edit Markdown",
        "<code>@md render</code> pretty-prints a file (long files open a scrollable pager); <code>@md edit</code> is a split editor — Markdown source on the left, a live rendered preview on the right, with native diagrams and keyboard + mouse scroll."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 250 },
          { do: "cmd", text: "@md edit release.md" },
          { do: "pause", ms: 350 },
          { do: "call", fn: async (t) => buildMdEditor(t) },
        ]);
      },
    },

    gate: {
      opts: { title: "aiTerminal — @gate telegram", tabs: [{ title: "Terminal [project][@gate]", active: true }] },
      caption: () => caption("<code>@gate</code> — your terminal, from your phone",
        "<code>@gate telegram start</code> hands this pane to a chat: you keep typing here while a <b>paired</b> chat drives the same shell. Start <b>Claude Code, Codex, vim or a REPL</b> and it <b>attaches</b> — the chat becomes that program's live screen, with buttons for whatever it's asking. Detected from the terminal protocol itself, so it works for any program, with no code for any of them."),
      demo(w, myEpoch) {
        const g = makePhone(phoneEl);
        let live = null;
        /* Every beat is its own step, so `run` can abandon the story the moment
           another feature is selected — the phone lives outside the window and
           would otherwise keep animating on screen. */
        run(w, myEpoch, [
          { do: "pause", ms: 350 },

          /* 1 — the gate opens, and prints a code only this screen can see */
          { do: "cmd", text: "@gate telegram start" },
          { do: "out", spans: [ACC("  ⬤ telegram gate live"), DIM(" · @mourad_term_bot")], ms: 320 },
          { do: "out", spans: [MUT("  pair from the chat: "), ACC2("/pair 418-207"), MUT("   (nothing runs until you do)")], ms: 700 },

          /* 2 — pairing, from the phone */
          { do: "call", fn: async () => { await g.type("/pair 418-207"); g.send("/pair 418-207"); } },
          { do: "call", fn: async () => { g.typing(true); await sleep(650); g.typing(false); } },
          { do: "call", fn: async () => { g.reply("✓ paired — you are driving mourad-mbp"); await sleep(500); } },
          { do: "call", fn: async () => {
            g.menu("Send a command and I'll run it in your terminal.",
              ["/shot", "/status", "/full", "/help", "/stop"]);
          } },
          { do: "out", spans: [MUT("  ▸ Mourad paired from telegram")], ms: 800 },

          /* 3 — the pane is still yours */
          { do: "cmd", text: "ls" },
          { do: "out", spans: [FG("README.md  crates  docs  website")], ms: 900 },

          /* 4 — …and the chat drives the very same shell */
          { do: "call", fn: async () => { await g.type("git status"); g.send("git status"); await sleep(300); } },
          { do: "out", spans: [ACC("  ▸ Mourad: "), FG("git status")], ms: 260 },
          { do: "cmd", text: "git status", speed: 12 },
          { do: "out", spans: [FG("On branch "), OK("main")], ms: 110 },
          { do: "out", spans: [FG("nothing to commit, working tree clean")], ms: 260 },
          { do: "out", spans: [MUT("  ◂ sent 2 lines to telegram")], ms: 260 },
          { do: "call", fn: async () => {
            g.reply("❯ git status · ✓ 0 · 0.3s", "On branch main\nnothing to commit, working tree clean");
            await sleep(850);
          } },

          /* 5 — an interactive program takes over, and the chat becomes its screen */
          { do: "call", fn: async () => { await g.type("claude"); g.send("claude"); await sleep(280); } },
          { do: "out", spans: [ACC("  ▸ Mourad: "), FG("claude")], ms: 240 },
          { do: "cmd", text: "claude", speed: 14 },
          { do: "out", spans: [MUT("  ⬤ attached — the chat is driving claude")], ms: 500 },
          { do: "call", fn: async () => {
            g.reply("▶ <b>attached to claude</b> — send text to type into it.");
            await sleep(600);
          } },
          { do: "call", fn: async () => {
            await g.type("add a --json flag to export");
            g.send("add a --json flag to export");
            await sleep(400);
          } },
          { do: "call", fn: async () => {
            // ONE bubble from here on — it is edited, never re-posted.
            live = g.live("claude", [
              ["", "> add a --json flag to export"],
              ["", ""],
              ["dim", "✦ Thinking… (esc to interrupt)"],
            ], [["↑", "↓", "⏎", "esc", "^C", "📷"]]);
            await sleep(1100);
          } },
          { do: "call", fn: async () => {
            live.update([
              ["", "Edit src/export.rs"],
              ["ok", "  + #[arg(long)] json: bool"],
              ["", ""],
              ["", "Do you want to make this edit?"],
              ["sel", "❯ 1. Yes"],
              ["", "  2. Yes, and don't ask again"],
              ["", "  3. No, tell Claude what to do"],
            ], [["1 · Yes", "2 · Yes, and…", "3 · No, tell…"], ["↑", "↓", "⏎", "esc", "^C", "📷"]]);
            await sleep(1400);
          } },
          { do: "call", fn: async () => {
            // A tap sends no message — the live screen stays put and updates.
            live.update([
              ["ok", "✓ Edited src/export.rs"],
              ["", ""],
              ["dim", "✦ Running tests…"],
            ], [["↑", "↓", "⏎", "esc", "^C", "📷"]]);
            await sleep(900);
          } },
          { do: "out", spans: [MUT("  ▸ Mourad tapped "), ACC2("1 · Yes")], ms: 700 },

          /* 6 — when text isn't enough, ask for the screen */
          { do: "call", fn: async () => { await g.type("/shot"); g.send("/shot"); await sleep(300); } },
          // capture first, THEN report it — so the photo shows the terminal as it
          // was asked for, not the line announcing itself
          { do: "call", fn: async () => {
            // a clone of the live pane: same lines, same theme, same geometry
            g.photo("terminal.png · 68 KB", w);
            await sleep(240);
          } },
          { do: "out", spans: [MUT("  ◂ sent a screenshot (68 KB)")], ms: 1100 },

          /* 7 — and the person at the keyboard takes it back */
          { do: "cmd", text: "@gate stop" },
          { do: "out", spans: [ACC("  ⬤ gate closed"), DIM(" — stopped from this pane")], ms: 340 },
          { do: "call", fn: async () => { g.reply("gate closed · this terminal is no longer reachable"); } },
          { do: "out", spans: [MUT(" ")], ms: 200 },
          // the shell, back to being just yours
          { do: "out", spans: [ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), S("t-cursor", " ")] },
        ]);
      },
    },

    guard: {
      caption: () => caption("The AI proposes. <em>The guard disposes.</em>",
        "allow · ⚠ confirm · ✗ deny — deny always wins, and secrets are redacted before anything leaves your machine."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "cmd", text: "@ai clean the build artifacts" },
          { do: "spin", label: "thinking…", ms: 800 },
          { do: "out", spans: [ACC("❯ "), DIM("press Enter to run (or edit)")] },
          { do: "out", spans: [ACC("❯ "), ACC2("cargo clean")] },
          { do: "pause", ms: 1000 },
          { do: "cmd", text: "@ai force push my branch" },
          { do: "spin", label: "thinking…", ms: 700 },
          { do: "out", spans: [WARN("⚠ "), DIM("review before running — matches a confirm rule  /git push --force/")] },
          { do: "out", spans: [ACC("❯ "), ACC2("git push --force-with-lease origin fix/parser")] },
          { do: "pause", ms: 1100 },
          { do: "cmd", text: "@ai wipe the whole disk" },
          { do: "spin", label: "thinking…", ms: 700 },
          { do: "out", spans: [ERR("# blocked by guard: matches a deny rule  /rm -rf \\//")] },
        ]);
      },
    },

    redact: {
      caption: () => caption("Your secrets <em>never leave the room</em>",
        "Nine regex rules — AWS · OpenAI · Anthropic · GitHub · Slack · Google keys, bearer tokens, JWTs, PEM blocks and any <code>KEY=value</code> that looks sensitive — rewrite text on its way out. The AI still gets the shape of your config; it never gets the secret. Add <code>scope = \"terminal\"</code> and the masking applies to your screen too."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "cmd", text: "cat .env" },
          { do: "out", spans: [ACC2("DATABASE_URL"), MUT("="), FG("postgres://db.internal/prod")], ms: 70 },
          { do: "out", spans: [ACC2("AWS_ACCESS_KEY_ID"), MUT("="), WARN("AKIA3RJHF2P9QLXMZB4T")], ms: 70 },
          { do: "out", spans: [ACC2("ANTHROPIC_API_KEY"), MUT("="), WARN("sk-ant-api03-9Fk2LmQ7xTvB")], ms: 70 },
          { do: "out", spans: [ACC2("GITHUB_TOKEN"), MUT("="), WARN("ghp_8sK2mVx91QpLzR4tYnB7wDe3Fg")], ms: 70 },
          { do: "out", spans: [ACC2("LOG_LEVEL"), MUT("="), FG("debug")], ms: 900 },

          // your own machine shows you everything — the boundary is egress, not display
          { do: "cmd", text: "@ai why can't the app reach the database?" },
          { do: "spin", label: "reading your terminal…", ms: 1000 },
          { do: "call", fn: async (t) => buildRedactView(t) },
          { do: "pause", ms: 1400 },
          { do: "out", spans: [FG("db.internal has no port — try "), ACC2("db.internal:5432"), FG(" in DATABASE_URL")] },
        ]);
      },
    },


  };

  /* ---------------- wiring ---------------- */
  function select(id, replay = false) {
    const f = FEATURES[id];
    if (!f || (!replay && current === id)) return;
    current = id;
    document.querySelectorAll("[data-feature]").forEach((r) =>
      r.classList.toggle("active", r.dataset.feature === id));
    const w = fresh(Object.assign(
      { title: "aiTerminal", tabs: [{ title: "Terminal [project][zsh]", active: true }] },
      f.opts || {}));
    f.caption();
    f.demo(w, epoch);
  }

  document.querySelectorAll("[data-feature]").forEach((row) =>
    row.addEventListener("click", () => select(row.dataset.feature)));

  select("themes");
});
