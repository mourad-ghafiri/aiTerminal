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
        case "out": {
          /* `dense` marks a line that is part of a PICTURE — a diagram, a table,
             a board. Box-drawing glyphs only close into boxes when the line box
             is the cell, so those rows get the terminal's own tight metrics. */
          const line = w.line(st.spans, st.paneIdx);
          if (st.dense) line.classList.add("board");
          await sleep(st.ms || 120);
          break;
        }
        case "stream": await streamLine(w, st.spans, st); break;
        case "think":
          await streamLine(w, [DIM(st.text)], { paneIdx: st.paneIdx, speed: 9, prefix: [DIM("∴ ")] });
          break;
        case "tool": {
          /* The real trace shows WHAT a call is acting on, never the argument JSON it
             arrived as, and a duration in the unit a person would have used. The name
             is padded into a COLUMN so a run of calls reads as a table rather than as
             ragged prose — which is what the shipped trace does. */
          const dur = st.ms < 1000 ? `${st.ms}ms` : `${(st.ms / 1000).toFixed(1)}s`;
          const name = st.name.padEnd(11);
          /* A call still running after a moment says so, and its line is replaced by
             the finished one — otherwise a long command is indistinguishable from a
             hang. `slow` marks the calls worth showing that way. */
          if (st.slow) {
            const pending = w.line([DIM(`  ⋯ ${name} ${st.args}`)], st.paneIdx);
            await sleep(st.slow);
            pending.remove();
          }
          w.line([DIM(`  ⚙ ${name} ${st.args} · ${dur} · ${st.size}`)], st.paneIdx);
          await sleep(st.wait || 300);
          break;
        }
        case "spin": await spinner(w, st.label || "thinking…", st.ms || 900, st); break;
        case "footer":
          w.line([S("t-success", st.glyph || "✓"), DIM(" " + st.text)], st.paneIdx);
          break;
        case "typing": {
          // A line typed but NOT submitted — it stays on screen with its cursor,
          // which is the state that makes a chat command queue instead of splice.
          w._typing = await typeCmd(w, st.text, st);
          w._typing.appendChild(el("span", "t-cursor"));
          break;
        }
        case "submit": {
          // …and finishing that line is what releases the queue.
          const line = w._typing;
          w._typing = null;
          if (!line) break;
          const rest = spanEl(FG(""));
          line.insertBefore(rest, line.lastChild); // before the cursor
          for (const ch of st.text || "") {
            rest.textContent += ch;
            await sleep(st.speed || 34);
          }
          await sleep(220);
          line.lastChild.remove(); // the cursor: the line is sent
          break;
        }
        case "board": await board(w, st, () => myEpoch === epoch); break;
        case "pause": await sleep(st.ms); break;
        case "call": await st.fn(w); break;
      }
    }
  }

  const resting = (w, cwd = "~/project") => {
    w.line([ACC(cwd + " "), ACC2("❯ "), FG("git status -sb")]);
    w.line([OK("## main...origin/main")]);
  };

  /* One agent's three lines, as `@agent` prints them: a blank line, the padded
     name with its counts, the description WRAPPED and indented under it, and the
     skills spliced into its prompt. The wrap width is the binary's — a roster
     that reflowed differently here would be a roster describing another tool. */
  function agentRows(agents) {
    const WRAP = 88;
    const out = [];
    agents.forEach(([name, counts, desc, skills]) => {
      out.push({ do: "out", spans: [] });
      out.push({ do: "out", spans: [ACC("  @" + name.padEnd(12)), DIM(counts)], ms: 50 });
      let line = "";
      desc.split(" ").forEach((word) => {
        if ((line + " " + word).trim().length > WRAP) { out.push({ do: "out", spans: [FG("      " + line)], ms: 30 }); line = word; }
        else line = (line ? line + " " : "") + word;
      });
      if (line) out.push({ do: "out", spans: [FG("      " + line)], ms: 30 });
      if (skills) out.push({ do: "out", spans: [DIM("      skills   " + skills)], ms: 40 });
    });
    return out;
  }

  /* ---------------- the live flow board ----------------
     The board is not typed out here — it is LAID OUT by js/board.js, the port of
     the terminal's own geometry, and painted into the pane through the same span
     classes everything else uses. So a frame here is the frame the binary paints
     at that width: cards reflow, edges rejoin, and when the window is too narrow
     for cards the dense list takes over, exactly as it does in the product. */

  const INK = { g: OK, e: ERR, w: WARN, a: ACC, d: MUT, null: FG };

  /* The pane in the replica's own cells — columns AND rows, measured rather than
     assumed, because the board reflows on both and the showcase is read at
     1440px and on a phone. Below a certain size the terminal stops drawing cards
     and prints the dense list instead; giving `board.js` the real numbers is
     what lets the demo do the same rather than draw past its own edge. */
  function paneSize(w) {
    const pane = w.pane();
    const probe = el("div", "rw-line board");
    probe.appendChild(spanEl(FG("M".repeat(80))));
    probe.style.cssText = "position:absolute;visibility:hidden;white-space:pre";
    pane.appendChild(probe);
    const box = probe.getBoundingClientRect();
    probe.remove();
    const cell = box.width / 80;
    const width = pane.clientWidth || pane.getBoundingClientRect().width;
    const height = pane.clientHeight || pane.getBoundingClientRect().height;
    return {
      cols: cell > 0 ? Math.max(Math.floor(width / cell) - 1, 24) : 100,
      rows: box.height > 0 ? Math.max(Math.floor(height / box.height), 8) : 24,
    };
  }

  /* One beat of a run: fold this step's states into the model, then hold the
     frame while the spinner turns — redrawing in place, which is what makes it
     read as a run rather than as a screenshot. */
  async function board(w, st, alive) {
    const m = st.model;
    /* A scene declares a GRAPH; what has happened to it accumulates here, so a
       beat only has to name what changed. */
    ["state", "note", "calls", "attempts", "ms", "tokens", "trace"].forEach((k) => { m[k] = m[k] || {}; });
    Object.assign(m.state, st.state || st.states || {});
    Object.assign(m.calls, st.calls || {});
    Object.assign(m.attempts, st.attempts || {});
    /* A node that has just finished stops being live and becomes a cost, so its
       in-flight note goes — before this beat's own notes are applied, because a
       failure that says WHY must survive settling. */
    Object.entries(st.settle || {}).forEach(([id, got]) => {
      m.ms[id] = got.ms;
      m.tokens[id] = got.tokens;
      delete m.note[id];
    });
    Object.assign(m.note, st.note || {});
    if (st.elapsed) m.elapsed = st.elapsed;

    const { cols, rows: rowsAvailable } = paneSize(w);
    m.rowsAvailable = rowsAvailable;
    /* ONE region per run, repainted — the same rule the terminal follows. The
       rows are kept on the model, so the next beat redraws these lines rather
       than printing a second board under the first; a pane that was cleared (or
       scrolled its top away) drops them and starts a fresh region. */
    if (!m.rows || !m.rows.length || !m.rows[0].isConnected) m.rows = [];
    const held = m.rows;
    const FRAME = 90; // the terminal's own spinner period
    /* The last frame sleeps only the remainder, so a beat lasts exactly what it
       was budgeted — the film's running time is a sum, not an estimate. */
    const hold = Math.max(st.ms || 1200, FRAME);
    for (let t = 0; t < hold; t += FRAME) {
      if (!alive()) return;
      const rows = drawBoard(m, cols, Math.floor(t / FRAME));
      rows.forEach((runs, i) => {
        const spans = runs.map((r) => (INK[r.cls] || FG)(r.text));
        if (held[i]) {
          held[i].innerHTML = "";
          spans.forEach((s) => held[i].appendChild(spanEl(s)));
        } else { held[i] = w.line(spans); held[i].classList.add("board"); }
      });
      /* A frame with fewer rows than the last one must not leave a tail. */
      held.splice(rows.length).forEach((l) => l.remove());
      await sleep(Math.min(FRAME, hold - t));
    }
  }

  /* `@flow graph document` — transcribed from the binary's own output, because
     the diagram renderer draws this, not the board: the shape, the conditions on
     the edges, the bounded `up to 2x` loop back to `check`, and a table of what
     every node can reach. It costs nothing and asks no model. */
  const GRAPH_DOCUMENT = [
    { do: "out", spans: [ACC("document")], ms: 45, dense: true },
    { do: "out", spans: [MUT("────────────────────────────────────────────────────────────")], ms: 45, dense: true },
    { do: "out", spans: [], ms: 45, dense: true },
    { do: "out", spans: [FG("Read the real code, write the doc, and check every claim against the source")], ms: 45, dense: true },
    { do: "out", spans: [], ms: 45, dense: true },
    { do: "out", spans: [FG("5 nodes · loops · 30m · 400000 tokens · needs an input")], ms: 45, dense: true },
    { do: "out", spans: [], ms: 45, dense: true },
    { do: "out", spans: [], ms: 45, dense: true },
    { do: "out", spans: [FG("           ┌────────────────┐")], ms: 45, dense: true },
    { do: "out", spans: [FG("           │ read @explorer │")], ms: 45, dense: true },
    { do: "out", spans: [FG("           └────────────────┘")], ms: 45, dense: true },
    { do: "out", spans: [FG("                    │")], ms: 45, dense: true },
    { do: "out", spans: [FG("                    │")], ms: 45, dense: true },
    { do: "out", spans: [FG("                    ▼")], ms: 45, dense: true },
    { do: "out", spans: [FG("            ┌───────┴───────┐")], ms: 45, dense: true },
    { do: "out", spans: [FG("            │ draft @writer │")], ms: 45, dense: true },
    { do: "out", spans: [FG("            └───────────────┘")], ms: 45, dense: true },
    { do: "out", spans: [FG("                    │")], ms: 45, dense: true },
    { do: "out", spans: [FG("                    │")], ms: 45, dense: true },
    { do: "out", spans: [FG("                    ▼")], ms: 45, dense: true },
    { do: "out", spans: [FG("           ┌────────┴────────┐up to 2x")], ms: 45, dense: true },
    { do: "out", spans: [FG("           │ check @reviewer │◀╎")], ms: 45, dense: true },
    { do: "out", spans: [FG("           └─────────────────┘ ╎")], ms: 45, dense: true },
    { do: "out", spans: [FG("       VERDICT: FAIL│          ╎")], ms: 45, dense: true },
    { do: "out", spans: [FG("          ┌─────────┴─────────┐╎")], ms: 45, dense: true },
    { do: "out", spans: [FG("          ▼      VERDICT: PASS▼╎")], ms: 45, dense: true },
    { do: "out", spans: [FG(" ┌────────┴───────┐   ┌───────┴───────┐")], ms: 45, dense: true },
    { do: "out", spans: [FG(" │ revise @writer │╌╌╌│╌final @writer │")], ms: 45, dense: true },
    { do: "out", spans: [FG(" └────────────────┘   └───────────────┘")], ms: 45, dense: true },
    { do: "out", spans: [], ms: 45, dense: true },
    { do: "out", spans: [MUT("╭────────┬───────────┬────────────────────────────────┬─────────────────────╮")], ms: 45, dense: true },
    { do: "out", spans: [MUT("│"), FG(" node   "), MUT("│"), FG(" runs      "), MUT("│"), FG(" when                           "), MUT("│"), FG(" reaches             "), MUT("│")], ms: 45, dense: true },
    { do: "out", spans: [MUT("├────────┼───────────┼────────────────────────────────┼─────────────────────┤")], ms: 45, dense: true },
    { do: "out", spans: [MUT("│"), FG(" read   "), MUT("│"), FG(" @explorer "), MUT("│"), FG(" —                              "), MUT("│"), FG(" 7 tools · 1 skill   "), MUT("│")], ms: 45, dense: true },
    { do: "out", spans: [MUT("│"), FG(" draft  "), MUT("│"), FG(" @writer   "), MUT("│"), FG(" —                              "), MUT("│"), FG(" 10 tools · 2 skills "), MUT("│")], ms: 45, dense: true },
    { do: "out", spans: [MUT("│"), FG(" check  "), MUT("│"), FG(" @reviewer "), MUT("│"), FG(" —                              "), MUT("│"), FG(" 7 tools · 5 skills  "), MUT("│")], ms: 45, dense: true },
    { do: "out", spans: [MUT("│"), FG(" revise "), MUT("│"), FG(" @writer   "), MUT("│"), FG(" check = VERDICT: FAIL · ↺ che… "), MUT("│"), FG(" 10 tools · 2 skills "), MUT("│")], ms: 45, dense: true },
    { do: "out", spans: [MUT("│"), FG(" final  "), MUT("│"), FG(" @writer   "), MUT("│"), FG(" check = VERDICT: PASS          "), MUT("│"), FG(" 10 tools · 2 skills "), MUT("│")], ms: 45, dense: true },
    { do: "out", spans: [MUT("╰────────┴───────────┴────────────────────────────────┴─────────────────────╯")], ms: 45, dense: true },
  ];

  const caption = (title, text, extra = "") => {
    captionEl.innerHTML =
      `<h3>${title}</h3><p>${text}</p>${extra ? `<div class="cap-extra">${extra}</div>` : ""}`;
  };

  /* The phone (`makePhone`), the `@md` split editor (`buildMdEditor`) and the
     secrets view (`buildRedactView`) come from js/scenes.js — video.html
     stages the same scenes, so they belong to neither page alone. */

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
        "One guarded command per request, <b>preloaded at your prompt</b> rather than run behind your back: press Enter, or edit it first. Risky → <b>⚠ review before running</b>. Catastrophic → blocked outright. Ask a question instead and it answers as live Markdown. And while you wait, one dim line rides <em>inside</em> the spinner — a tip, a fact, a quote — costing no rows and gone the moment the answer starts."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },

          /* The everyday case: a command you would have had to look up. */
          { do: "cmd", text: "@ai which ports are listening, and what is holding them" },
          { do: "spin", label: "thinking…", ms: 1400 },
          { do: "out", spans: [ACC("❯ "), DIM("press Enter to run (or edit)")] },
          { do: "out", spans: [ACC("❯ "), ACC2("lsof -iTCP -sTCP:LISTEN -n -P"), S("t-cursor", "")] },
          { do: "pause", ms: 1400 },

          /* Something with teeth: the guard does not block it, it makes you look. */
          { do: "cmd", text: "@ai drop every stopped container and its volumes" },
          { do: "spin", label: "thinking…", ms: 2600, aside: "a prompt prefix the provider already cached costs about a tenth as much" },
          { do: "out", spans: [WARN("⚠ "), DIM("review before running (or edit)")] },
          { do: "out", spans: [ACC("❯ "), ACC2("docker container prune -f && docker volume prune -f"), S("t-cursor", "")] },
          { do: "out", spans: [MUT("  ← the guard classified it: not blocked, but not run for you either")] },
          { do: "pause", ms: 1500 },

          /* A question, not a command — the answer streams as live Markdown. */
          { do: "cmd", text: "@ai why would a container restart-loop with exit 137?" },
          { do: "spin", label: "thinking…", ms: 1000 },
          { do: "stream", spans: [FG("137 is 128+9 — the kernel sent SIGKILL, almost always the OOM killer. Check "), ACC2("docker inspect --format '{{.State.OOMKilled}}'"), FG(" first; if it is true, the memory limit is the bug, not the app.")], speed: 10 },
          { do: "footer", text: "3.1s · 1.4k in / 310 out (1.1k cached, 79%)" },
          { do: "pause", ms: 1300 },

          /* Files ride the request: an image goes to a vision model as an image. */
          { do: "cmd", text: "@ai what does this diagram show? @design/arch.png" },
          { do: "out", spans: [DIM("  📎 arch.png → vision block (1.2 MB)")] },
          { do: "spin", label: "thinking…", ms: 1000 },
          { do: "stream", spans: [FG("A four-layer architecture: corelib → platform → framework → app. Nothing points back up a layer.")], speed: 11 },
        ]);
      },
    },

    agent: {
      caption: () => caption("<code>@agent</code> — eight specialists at your prompt",
        "<code>@agent</code> lists what you have, <code>@agent &lt;name&gt;</code> shows one in full, and <code>@&lt;name&gt;</code> runs it. They are not all for writing code: <b>researcher</b> searches the web and reads the pages (keyless — no extra account, just <code>[ai] network = true</code>), <b>writer</b> drafts and saves the file, <b>planner</b> turns a goal into a plan. Each is an editable Markdown file."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "cmd", text: "@agent" },
          { do: "out", spans: [FG("agents (8):")] },
          { do: "out", spans: [MUT("  served by claude-sonnet-5")], ms: 200 },
          /* The roster as the binary prints it: a blank line, the name with its
             counts, the description wrapped and indented UNDER it, then the
             skills spliced into its prompt. Counts read out of the frontmatter
             in builtin/ai/agents/ — they are not decoration, they are what the
             agent may reach. */
          ...agentRows([
            ["ai", "19 tools · 6 steps", "General assistant — concise Markdown answers, with commands to review.", ""],
            ["coder", "25 tools · 8 skills · 24 steps", "Senior engineer + orchestrator — explores, makes the smallest correct edit, verifies, delegates.", "concise · planning · orchestration · code-review · testing · verification · debugging · git"],
            ["explorer", "7 tools · 1 skill · 12 steps", "Fast read-only scout — maps the relevant code and reports back tightly.", "concise"],
            ["planner", "7 tools · 4 skills · 10 steps", "Turns a goal into a short plan with acceptance criteria — reads, never writes.", "concise · planning · research · orchestration"],
            ["researcher", "12 tools · 3 skills · 16 steps", "Finds sources, reads them, and reports what they actually say — with links.", "concise · research · writing"],
            ["reviewer", "7 tools · 5 skills · 12 steps", "Read-only code review — correctness, security, tests, design.", "concise · code-review · security-review · verification · writing"],
            ["tester", "13 tools · 4 skills · 18 steps", "Writes and runs tests; reproduces a failure, then fixes it.", "concise · verification · testing · debugging"],
            ["writer", "10 tools · 2 skills · 14 steps", "Writes documentation and reports for the person who will read them — and saves the file.", "concise · writing"],
          ]),
          { do: "out", spans: [] },
          { do: "out", spans: [MUT("one in full:  @agent <name>   ·  run one:  @<name> \"<task>\"")] },
          { do: "pause", ms: 1000 },

          /* A read-only specialist on the change you are about to push. */
          { do: "cmd", text: "@reviewer \"review the changes on this branch\"" },
          { do: "out", spans: [ACC("✦ @reviewer"), MUT(" · claude-sonnet-5")] },
          { do: "tool", name: "sys.run", args: "git diff main...HEAD", ms: 42, size: "214 lines" },
          { do: "tool", name: "fs.read", args: "src/export.rs", ms: 5, size: "8.2KB" },
          { do: "tool", name: "fs.search", args: '"write_all"', ms: 16, size: "4 results" },
          { do: "stream", spans: [FG("Two must-fix. export.rs:88 writes before it checks the flag, so --json emits a header twice; cache.rs:41 drops the lock before the read. The rest is fine.")], speed: 10 },
          { do: "footer", text: "9.4s · 3 tools · 11.2k in / 1.1k out (8.6k cached, 77%)" },
          { do: "pause", ms: 1100 },

          /* One that reads the web rather than the repo — and cites what it read. */
          { do: "cmd", text: "@researcher \"what actually changed in the tokio 1.42 release\"" },
          { do: "out", spans: [ACC("✦ @researcher"), MUT(" · claude-sonnet-5")] },
          { do: "tool", name: "web.search", args: '"tokio 1.42 changelog"', ms: 340, size: "8 results" },
          { do: "tool", name: "web.read", args: "https://…/tokio/releases", ms: 980, size: "31KB" },
          { do: "stream", spans: [FG("Three changes matter to you: the runtime metrics are stable, task::block_in_place is cheaper, and one io_uring path is gated. Links below; the fourth item I could not confirm.")], speed: 10 },
          { do: "pause", ms: 1100 },

          /* and the same harness on code */
          { do: "cmd", text: "@coder \"fix the failing parser test\"" },
          { do: "out", spans: [ACC("✦ @coder"), MUT(" · claude-sonnet-5")] },
          { do: "spin", label: "thinking…", ms: 900 },
          { do: "think", text: "The test expects a trailing newline — the parser drops it on the last line…" },
          { do: "tool", name: "todo.set", args: "find it", ms: 3, size: "3 entries" },
          { do: "tool", name: "fs.search", args: '"parse_line"', ms: 18, size: "6 results" },
          { do: "tool", name: "fs.edit", args: "src/parser.rs", ms: 6, size: "1 replaced" },
          { do: "tool", name: "sys.run", args: "cargo test parser", ms: 2100, size: "48 lines", slow: 700 },
          { do: "stream", spans: [FG("The fix: the parser dropped the final line — added the flush in "), ACC2("parse_line()"), FG(".")], speed: 11 },
          { do: "footer", text: "8.4s · 4 tools · 12.3k in / 1.8k out (11.1k cached, 90%)" },
        ]);
      },
    },

    flow: {
      /* A graph wants rows. A real terminal has forty of them; this window is
         given enough that the cards fit at a desktop width — and when they do
         not (a phone), `board.js` falls back to the dense list, exactly as the
         terminal does. */
      opts: { tall: true },
      caption: () => caption("<code>@flow</code> — a <em>graph</em> of agents",
        "Nodes that need nothing from each other run at the same time, and a failing check loops back through a fixer — <b>bounded</b>. Nothing runs until the graph is proved. Five ship: <b>build · fix · review · document · research</b> — or <b>name none, and one is written for your goal</b>, checked the same way."),
      demo(w, myEpoch) {
        /* The board below is not typed out — it is LAID OUT, by the port of the
           terminal's own geometry in js/board.js, and diffed byte for byte
           against what the binary paints. So it animates the way a run does:
           states change, the board is redrawn, the trail goes green behind it. */
        const built = {
          nodes: [
            { id: "read",    what: "@explorer", model: "claude-sonnet-5", needs: [] },
            { id: "draft",   what: "@writer",   model: "claude-sonnet-5", needs: ["read"] },
            { id: "check",   what: "@reviewer", model: "claude-sonnet-5", needs: ["draft"] },
          ],
          tools: 24, skills: 8, concurrency: 4, slowest: ["read", "draft", "check"],
        };
        /* builtin/ai/flows/review.toml, node for node: three reviewers that need
           only `map`, so they share a rank — and a report that needs all three. */
        const reviewFlow = {
          nodes: [
            { id: "map",         what: "@explorer", model: "claude-sonnet-5", needs: [] },
            { id: "correctness", what: "@reviewer", model: "claude-sonnet-5", needs: ["map"] },
            { id: "security",    what: "@reviewer", model: "claude-sonnet-5", needs: ["map"] },
            { id: "design",      what: "@reviewer", model: "claude-sonnet-5", needs: ["map"] },
            { id: "report",      what: "@reviewer", model: "claude-sonnet-5", needs: ["correctness", "security", "design"] },
          ],
          tools: 14, skills: 6, concurrency: 4, slowest: ["map", "correctness", "report"],
        };
        run(w, myEpoch, [
          { do: "pause", ms: 300 },

          /* No flow named — one is designed for the goal, and checked before it
             is allowed to spend anything. Both lines are the binary's own. */
          { do: "cmd", text: "@flow document how the export command works" },
          { do: "out", spans: [MUT("◈ no flow named — building a graph for this goal")] },
          { do: "spin", label: "building a graph for this…", ms: 1100 },
          { do: "out", spans: [MUT("◈ built a 3-node graph: read the code, write it up, check every claim · @flow show 1785639076-20816")] },
          /* Every run opens with this line: the flow's name and what it was given. */
          { do: "out", spans: [ACC("▸ document-how-the-export · how the export command works")] },
          { do: "board", model: built, states: { read: "running" },
            note: { read: "⚙ fs.search \"export\"" }, calls: { read: 3 }, ms: 900 },
          { do: "board", model: built, states: { read: "running" },
            note: { read: "⚙ fs.read src/export.rs" }, calls: { read: 7 }, ms: 900 },
          { do: "board", model: built, states: { read: "done", draft: "running" },
            settle: { read: { ms: 8100, tokens: 9400 } }, calls: { read: 11, draft: 2 },
            note: { draft: "⚙ fs.write docs/export.md" }, ms: 1100 },
          { do: "board", model: built, states: { read: "done", draft: "done", check: "running" },
            settle: { draft: { ms: 12300, tokens: 6200 } }, calls: { check: 4 },
            note: { check: "⚙ fs.read docs/export.md" }, ms: 1100 },
          { do: "board", model: built, states: { read: "done", draft: "done", check: "done" },
            settle: { check: { ms: 6700, tokens: 3100 } }, elapsed: "27.1s", ms: 1600 },
          { do: "stream", spans: [FG("Wrote docs/export.md — every claim points at a line in src/export.rs. Two options were undocumented; both are now, with their real defaults.")], speed: 10 },
          { do: "pause", ms: 1400 },

          /* The shipped `review` flow: three reviewers on one rank, so they run at
             the same time — and one of them failing is the beat a demo skips. */
          { do: "call", fn: (t) => t.clear() },
          { do: "cmd", text: "@flow review \"the changes on this branch\"" },
          { do: "out", spans: [ACC("▸ review · the changes on this branch")] },
          { do: "board", model: reviewFlow, states: { map: "done", correctness: "running", security: "running", design: "running" },
            settle: { map: { ms: 6400, tokens: 7100 } },
            calls: { map: 9, correctness: 4, security: 3, design: 2 },
            note: { correctness: "⚙ fs.read src/export.rs", security: "⚙ fs.search \"unwrap(\"", design: "⚙ fs.read src/cache.rs" }, ms: 2400 },
          { do: "board", model: reviewFlow,
            states: { map: "done", correctness: "failed", security: "done", design: "done", report: "blocked" },
            settle: { correctness: { ms: 11200, tokens: 8300 }, security: { ms: 9100, tokens: 6600 }, design: { ms: 7800, tokens: 5200 } },
            note: { correctness: "2 blockers — export.rs:88, cache.rs:41", report: "something it needed failed" },
            elapsed: "31.4s", ms: 2400 },
          { do: "out", spans: [MUT("  ← three reviewers on one rank, one round of wall clock")] },
          { do: "out", spans: [MUT("  ← the card says why, the pane says what it cost, and nothing behind it burns a token")] },
          { do: "pause", ms: 1600 },

          /* Free, and needs no model at all: the shape, the conditions, and what
             every node can reach — before you spend anything on running it. */
          { do: "call", fn: (t) => t.clear() },
          { do: "cmd", text: "@flow graph document" },
          ...GRAPH_DOCUMENT,
          { do: "pause", ms: 1400 },
          { do: "cmd", text: "@flow check build" },
          { do: "out", spans: [OK("  ✓ build"), DIM(" · 8 node(s) · worst case 21 agent run(s)")] },
          { do: "out", spans: [MUT("  ← @flow check and @flow graph spend nothing and need no model")] },
        ]);
      },
    },

    loop: {
      caption: () => caption("<code>@loop</code> — until it's <em>actually</em> done",
        "Give it a goal and a command whose exit status decides — <code>--check</code>. Give none and the AI proposes one <b>once</b>, the guard adjudicates it, and it is proven <em>before</em> the first token is spent; already green costs nothing. The check does not have to be a test suite: anything with an exit status works, which is why the second example is about prose."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "cmd", text: "@loop \"make the config tests pass\"" },
          { do: "out", spans: [FG("🔁 loop 'coder' — up to 5 iteration(s)")] },
          { do: "out", spans: [DIM("  verifier: cargo test -p framework config:: — proposed from the goal")] },
          { do: "out", spans: [FG("▶ iteration 1/5")] },
          { do: "tool", name: "fs.edit", args: "src/config.rs", ms: 7, size: "1 replaced" },
          { do: "out", spans: [DIM("  check: exit=1 · assertion failed: default theme")], ms: 500 },
          { do: "out", spans: [FG("▶ iteration 2/5")] },
          { do: "tool", name: "fs.edit", args: "src/config.rs", ms: 5, size: "1 replaced" },
          { do: "out", spans: [DIM("  check: exit=0")], ms: 400 },
          { do: "out", spans: [OK("✓ goal reached after 2 iteration(s)")] },
          { do: "pause", ms: 1200 },

          /* the same machine, no code in sight — `wc` and `test` are all it takes */
          { do: "cmd", text: "@loop \"cut the intro in intro.md under 200 words without losing the point\" --agent writer --check \"test $(wc -w < intro.md) -lt 200\"" },
          { do: "out", spans: [FG("🔁 loop 'writer' — up to 5 iteration(s)")] },
          { do: "out", spans: [DIM("  verifier: test $(wc -w < intro.md) -lt 200")] },
          { do: "out", spans: [FG("▶ iteration 1/5")] },
          { do: "tool", name: "fs.edit", args: "intro.md", ms: 5, size: "2 replaced" },
          { do: "out", spans: [DIM("  check: exit=1")], ms: 420 },
          { do: "out", spans: [FG("▶ iteration 2/5")] },
          { do: "tool", name: "fs.edit", args: "intro.md", ms: 4, size: "1 replaced" },
          { do: "out", spans: [DIM("  check: exit=0")], ms: 380 },
          { do: "out", spans: [OK("✓ goal reached after 2 iteration(s)")] },
          { do: "out", spans: [MUT("  ← the goal was prose; the check was still a command that exits 0")] },
        ]);
      },
    },

    job: {
      caption: () => caption("<code>@job</code> — say what to do, and when",
        "The AI reads the schedule out of your sentence <em>once</em>, at creation, and writes it into the record as cron — so every run after that is plain arithmetic. You <b>watch it think</b>: the one step that takes real time says so, and if it rewrote your sentence it tells you what it heard. Recurring jobs survive a reboot; a missed one catches up exactly once. And a job that runs a <em>command</em> (<code>@job -- …</code>) never touches a model."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },

          /* The reactive beat: the model reading the sentence is the whole of the
             wait, so it is the thing the terminal shows — and then it says what it
             heard, because a rewrite changes what the job IS. */
          { do: "cmd", text: '@job "audit the dependencies every Monday at 9 and write it to ~/reports/deps.md"' },
          { do: "spin", label: "reading when to run this…", ms: 1100 },
          { do: "out", spans: [MUT("◈ every Monday at 09:00 — audit the dependencies into ~/reports/deps.md")] },
          { do: "out", spans: [ACC("⧖ every Monday at 09:00 — audit the dependencies into ~/reports/deps.md · job 1785639076-20816")] },
          { do: "out", spans: [FG("  fires in 3d · list: @job · cancel: @job cancel 1785639076-20816")] },
          { do: "pause", ms: 1100 },

          /* A sentence it could not read is not silently guessed at — the word
             parser answers instead, and you are told it did. */
          { do: "cmd", text: '@job "tail the deploy log when the release cuts"' },
          { do: "spin", label: "reading when to run this…", ms: 900 },
          { do: "out", spans: [MUT("⚠  the model could not read that — using the words as typed")] },
          { do: "out", spans: [ACC("⧖ tail the deploy log · job 1785639140-20834")] },
          { do: "out", spans: [FG("  fires in 1m · list: @job · cancel: @job cancel 1785639140-20834")] },
          { do: "pause", ms: 1100 },

          /* No sentence to read, so no model, so no wait at all. */
          { do: "cmd", text: "@job --every 15m -- ./scripts/sync-fixtures.sh" },
          { do: "out", spans: [ACC("⧖ ./scripts/sync-fixtures.sh · job 1785639201-20851")] },
          { do: "out", spans: [FG("  fires in 15m · list: @job · cancel: @job cancel 1785639201-20851")] },
          { do: "out", spans: [MUT("  ← a command job asks no model and costs nothing")] },
          { do: "pause", ms: 1000 },

          /* `@job` with nothing after it: the list, exactly as it prints. */
          { do: "cmd", text: "@job" },
          { do: "out", spans: [FG("background jobs (4):")] },
          { do: "out", spans: [FG("  ⧖ 1785639076-20816 scheduled audit the dependencies every Monday at 9…  "), DIM("(fires in 3d)")] },
          { do: "out", spans: [DIM("      every Monday at 09:00 · 4 run(s) · last ok")] },
          { do: "out", spans: [FG("  ⧖ 1785639201-20851 scheduled ./scripts/sync-fixtures.sh  "), DIM("(fires in 12m)")] },
          { do: "out", spans: [DIM("      every 15m · 96 run(s) · last ok")] },
          { do: "out", spans: [FG("  ▶ 1785638840-20705 running   regenerate the API docs  "), DIM("(2m ago · 2m)")] },
          { do: "out", spans: [FG("  ✓ 1785638102-20544 done      summarise yesterday's CI failures  "), DIM("(4h ago · 51s)")] },
          { do: "out", spans: [DIM("      1 run(s) · last ok")] },
          { do: "out", spans: [MUT("  ← @job log <id> -f follows one like a log file")] },
        ]);
      },
    },

    md: {
      opts: { title: "release.md — @md", tabs: [{ title: "release.md [@md]", active: true }] },
      caption: () => caption("<code>@md</code> — read &amp; live-edit Markdown",
        "Two commands, both shown here. <code>@md render</code> pretty-prints a file to the terminal \u2014 headings, bullets, tables and <code>mermaid</code> diagrams drawn natively, no browser; a file longer than the window opens a scrollable pager. <code>@md edit</code> opens the same document as a split editor: source on the left with a line gutter, the live preview on the right, <kbd>^S</kbd> to save and <kbd>^W</kbd> to move focus between the halves."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 250 },
          /* First half: `@md render` — what the file looks like read, not edited. This is the
             binary's own output for the very document the editor opens next. */
          { do: "cmd", text: "@md render release.md" },
          { do: "out", spans: [ACC("Release 0.9 — the graph you can watch")], ms: 45 },
          { do: "out", spans: [MUT("\u2500".repeat(60))], ms: 45 },
          { do: "out", spans: [], ms: 45 },
          { do: "out", spans: [FG("The exporter now speaks JSON, and "), ACC2("@flow"), FG(" draws itself while it runs.")], ms: 45 },
          { do: "out", spans: [], ms: 45 },
          { do: "out", spans: [ACC("What changed")], ms: 45 },
          { do: "out", spans: [MUT("\u2500".repeat(60))], ms: 45 },
          { do: "out", spans: [], ms: 45 },
          /* The renderer's own table: ROUNDED corners, muted rules, a bold header
             row, and inline code still coloured inside a cell. */
          { do: "out", spans: [MUT("╭────────┬───────────────────────────────────────────┬───────╮")], ms: 45, dense: true },
          { do: "out", spans: [MUT("│"), FG(" Area   "), MUT("│"), FG(" Change                                    "), MUT("│"), FG(" Issue "), MUT("│")], ms: 45, dense: true },
          { do: "out", spans: [MUT("├────────┼───────────────────────────────────────────┼───────┤")], ms: 45, dense: true },
          { do: "out", spans: [MUT("│"), FG(" export "), MUT("│"), ACC2(" --json"), FG(" emits one object per row           "), MUT("│"), FG(" #412  "), MUT("│")], ms: 45, dense: true },
          { do: "out", spans: [MUT("│"), FG(" flow   "), MUT("│"), FG(" the board redraws in place                "), MUT("│"), FG(" #418  "), MUT("│")], ms: 45, dense: true },
          { do: "out", spans: [MUT("│"), FG(" jobs   "), MUT("│"), FG(" creation is reactive, not a frozen prompt "), MUT("│"), FG(" #421  "), MUT("│")], ms: 45, dense: true },
          { do: "out", spans: [MUT("╰────────┴───────────────────────────────────────────┴───────╯")], ms: 45, dense: true },
          { do: "out", spans: [], ms: 45 },
          { do: "out", spans: [ACC("• "), FG("Breaking: "), ACC2("--format=csv"), FG(" is now "), ACC2("--format csv")], ms: 45 },
          { do: "out", spans: [ACC("• "), FG("Sixty-one tests were added; none were weakened")], ms: 45 },
          { do: "out", spans: [ACC("• "), ACC2("cargo test"), FG(" is green on 1.86 and on nightly")], ms: 45 },
          { do: "out", spans: [], ms: 45 },
          { do: "out", spans: [ACC("│"), FG(" Upgrade with "), ACC2("brew upgrade aiterminal"), FG(".")], ms: 45, dense: true },
          { do: "out", spans: [], ms: 45 },
          { do: "out", spans: [ACC("The release train")], ms: 45 },
          { do: "out", spans: [MUT("\u2500".repeat(60))], ms: 45 },
          { do: "out", spans: [], ms: 45 },
          /* A ```mermaid fence, drawn in the terminal — no browser, no export step. */
          { do: "out", spans: [FG(" ┌─────┐   ┌──────┐   ┌─────┐   ┌─────────┐")], ms: 45, dense: true },
          { do: "out", spans: [FG(" │ cut │──▶┤ test │──▶┤ tag │──▶┤ publish │")], ms: 45, dense: true },
          { do: "out", spans: [FG(" └─────┘   └──────┘   └─────┘   └─────────┘")], ms: 45, dense: true },
          { do: "out", spans: [], ms: 45 },
          { do: "out", spans: [MUT("  \u2190 that diagram is a "), ACC2("```mermaid"), MUT(" block, drawn natively \u2014 and a file longer than the window opens a pager")], ms: 45 },
          { do: "pause", ms: 1500 },
          /* Second half: the same document, live. The preview on the right renders the
             source on the left — including the diagram. */
          { do: "cmd", text: "@md edit release.md" },
          { do: "pause", ms: 350 },
          { do: "call", fn: async (t) => buildMdEditor(t) },
        ]);
      },
    },

    /* ── @gate, in four chapters ───────────────────────────────────────────────
       The feature is too large for one story: pairing, the shell relay, attaching
       to a program, the guard, and the awkward real-world cases are each a
       different claim. So each is its own chapter, and the phone is rebuilt for
       every one — a chapter must make sense on its own, because a visitor will
       click straight into the middle of the list.

       The bot's words are the production strings (`gate/driver.rs`), tags and all. */

    gate: {
      opts: { title: "aiTerminal — @gate telegram", tabs: [{ title: "Terminal [project][@gate]", active: true }] },
      caption: () => caption("<code>@gate</code> — pair, then run",
        "<code>@gate telegram start</code> prints a six-digit code <em>in your terminal</em>. Until someone types it, a stranger who finds the bot gets <b>no reply at all</b> — not even a hint that it is live. Once paired, a plain message <em>is</em> a command: it runs in the shared shell and comes back with its exit status and how long it took.",
        "<span class=\"lbl\">/run</span><span class=\"lbl\">/status</span><span class=\"lbl\">/full</span><span class=\"lbl\">/help</span><span class=\"lbl\">/stop</span>"),
      demo(w, myEpoch) {
        const g = makePhone(phoneEl);
        /* Every beat is its own step, so `run` can abandon the story the moment
           another feature is selected — the phone lives outside the window and
           would otherwise keep animating on screen. */
        run(w, myEpoch, [
          { do: "pause", ms: 350 },

          /* 1 — the gate opens, and prints a code only this screen can see */
          { do: "cmd", text: "@gate telegram start" },
          { do: "out", spans: [ACC("  ⬤ telegram gate live"), DIM(" · @mourad_term_bot")], ms: 320 },
          { do: "out", spans: [MUT("  pair from the chat: "), ACC2("/pair 418-207"), MUT("   (nothing runs until you do)")], ms: 700 },

          /* 2 — a stranger finds the bot and learns nothing. The pane is the only
             place this is visible; the phone stays empty, which is the point. */
          { do: "out", spans: [MUT("  ▸ unknown chat 8814… — "), WARN("no reply sent"), MUT(" (not paired)")], ms: 950 },

          /* 3 — pairing, from the phone that can see the screen */
          { do: "call", fn: async () => { await g.type("/pair 418-207"); g.send("/pair 418-207"); } },
          { do: "call", fn: async () => { g.typing(true); await sleep(650); g.typing(false); } },
          { do: "call", fn: async () => { g.reply("<b>paired</b> — you are driving <code>mourad-mbp</code>"); await sleep(500); } },
          { do: "call", fn: async () => {
            g.menu("Send a command and I'll run it in your terminal.",
              ["/shot", "/status", "/full", "/help", "/stop"]);
          } },
          { do: "out", spans: [MUT("  ▸ Mourad paired from telegram")], ms: 850 },

          /* 4 — the pane is still yours, and still a normal shell */
          { do: "cmd", text: "ls" },
          { do: "out", spans: [FG("README.md  crates  docs  website")], ms: 900 },

          /* 5 — …and the chat drives the very same shell */
          { do: "call", fn: async () => { await g.type("git status"); g.send("git status"); await sleep(300); } },
          { do: "out", spans: [ACC("  ▸ Mourad: "), FG("git status")], ms: 260 },
          { do: "cmd", text: "git status", speed: 12 },
          { do: "out", spans: [FG("On branch "), OK("main")], ms: 110 },
          { do: "out", spans: [FG("nothing to commit, working tree clean")], ms: 260 },
          { do: "out", spans: [MUT("  ◂ sent 2 lines to telegram")], ms: 260 },
          { do: "call", fn: async () => {
            g.reply("❯ <code>git status</code> · ✓ 0 · 0.3s", "On branch main\nnothing to commit, working tree clean");
            await sleep(900);
          } },

          /* 6 — a big output is trimmed, with the whole thing one command away */
          { do: "call", fn: async () => { await g.type("cargo test"); g.send("cargo test"); await sleep(250); } },
          { do: "out", spans: [ACC("  ▸ Mourad: "), FG("cargo test")], ms: 200 },
          { do: "cmd", text: "cargo test", speed: 12 },
          { do: "out", spans: [OK("test result: ok"), FG(". 1386 passed; 0 failed")], ms: 300 },
          { do: "call", fn: async () => {
            g.reply("❯ <code>cargo test</code> · ✓ 0 · 41s",
              "…\ntest result: ok. 1386 passed; 0 failed");
            await sleep(400);
          } },
          { do: "call", fn: async () => {
            g.reply("output trimmed to 3 messages — send <code>/full</code> for the whole thing as a file.");
            await sleep(750);
          } },
          { do: "call", fn: async () => { await g.type("/status"); g.send("/status"); await sleep(250); } },
          { do: "call", fn: async () => {
            g.reply("<b>gate</b> telegram · paired with Mourad · idle\n<b>shell</b> zsh at <code>~/project</code> · nothing running\n<b>uptime</b> 6m");
            await sleep(900);
          } },
        ]);
      },
    },

    gateApp: {
      opts: { title: "aiTerminal — @gate telegram", tabs: [{ title: "Terminal [project][@gate]", active: true }] },
      caption: () => caption("Drive <em>any</em> interactive program",
        "Start <b>Claude Code, Codex, vim or a REPL</b> in the gated shell and the gate <b>attaches</b>: the chat becomes that program's live screen — <em>one</em> message, edited in place, so a long session never buries the chat. A numbered question turns into buttons. None of this knows what Claude Code is; it is read from the terminal protocol itself, so it works for every program and has code for none of them.",
        "<span class=\"lbl\">/key</span><span class=\"lbl\">/keys</span><span class=\"lbl\">/cancel</span><span class=\"lbl\">/sh</span><span class=\"lbl\">/shot</span>"),
      demo(w, myEpoch) {
        const g = makePhone(phoneEl);
        let live = null;
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "out", spans: [ACC("  ⬤ telegram gate live"), DIM(" · paired with Mourad")], ms: 500 },

          /* 1 — an interactive program takes over, and the chat becomes its screen */
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

          /* 2 — a numbered question becomes buttons, and a tap answers it */
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
          { do: "out", spans: [MUT("  ▸ Mourad tapped "), ACC2("1 · Yes")], ms: 800 },

          /* 3 — /run has nowhere to go while a program holds the terminal, so it is
             refused rather than fired unattended minutes later. /sh is the way. */
          { do: "call", fn: async () => { await g.type("/run git diff"); g.send("/run git diff"); await sleep(280); } },
          { do: "call", fn: async () => {
            g.reply("<b>claude</b> is using the terminal, so there is no shell to run that in.\nTry <code>/sh git diff</code> to run it out-of-band, or <code>/key ctrl-c</code> to interrupt.");
            await sleep(1000);
          } },
          { do: "call", fn: async () => { await g.type("/sh git diff --stat"); g.send("/sh git diff --stat"); await sleep(260); } },
          { do: "out", spans: [MUT("  ▸ Mourad: "), FG("/sh git diff --stat"), MUT("  (own shell)")], ms: 300 },
          { do: "call", fn: async () => {
            g.reply("❯ <code>git diff --stat</code> · ✓ 0 · 0.2s", " src/export.rs | 3 +++\n 1 file changed, 3 insertions(+)");
            await sleep(950);
          } },

          /* 4 — keys, for everything a sentence cannot say */
          { do: "call", fn: async () => { await g.type("/key ctrl-r"); g.send("/key ctrl-r"); await sleep(250); } },
          { do: "call", fn: async () => {
            live.update([
              ["dim", "(reverse-i-search)`': "],
              ["", ""],
              ["dim", "any key name works: enter tab esc up f1–f12,"],
              ["dim", "ctrl-<letter>, alt-<char>, or a single character."],
            ], [["↑", "↓", "⏎", "esc", "^C", "📷"]]);
            await sleep(1200);
          } },

          /* 5 — when the text frame isn't enough, ask for the real screen */
          { do: "call", fn: async () => { await g.type("/shot"); g.send("/shot"); await sleep(300); } },
          // capture first, THEN report it — so the photo shows the terminal as it
          // was asked for, not the line announcing itself
          { do: "call", fn: async () => {
            // a clone of the live pane: same lines, same theme, same geometry
            g.photo("terminal.png · 68 KB", w);
            await sleep(240);
          } },
          { do: "out", spans: [MUT("  ◂ sent a screenshot (68 KB)")], ms: 900 },

          /* 6 — the program exits and the shell comes back on its own */
          { do: "out", spans: [MUT("  ⬤ detached — claude exited, back to the shell")], ms: 500 },
          { do: "call", fn: async () => { g.reply("■ <b>detached</b> — <code>claude</code> exited. Back to the shell."); } },
        ]);
      },
    },

    gateSafe: {
      opts: { title: "aiTerminal — @gate telegram", tabs: [{ title: "Terminal [project][@gate]", active: true }] },
      caption: () => caption("A paired chat still meets the guard",
        "A remote command goes through the same guard as an AI suggestion — <b>denied</b> outright, or <b>held</b> until you reply <code>/yes</code>. Your <code>[[guard.secret]]</code> rules apply to everything leaving the machine, in <em>both</em> scopes, because a chat app is off-machine either way — and a command you send back carrying a placeholder runs here with the real value in it, so your phone can use a key it has never seen. Every remote command, block and reply is echoed in the pane, so the screen in front of you is always the truth.",
        "<span class=\"lbl\">deny</span><span class=\"lbl\">confirm</span><span class=\"lbl\">redact</span><span class=\"lbl\">echoed in the pane</span>"),
      demo(w, myEpoch) {
        const g = makePhone(phoneEl);
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "out", spans: [ACC("  ⬤ telegram gate live"), DIM(" · paired with Mourad")], ms: 550 },

          /* 1 — denied outright. It never reaches the shell. */
          { do: "call", fn: async () => { await g.type("rm -rf /"); g.send("rm -rf /"); await sleep(300); } },
          { do: "out", spans: [ACC("  ▸ Mourad: "), FG("rm -rf /")], ms: 200 },
          { do: "out", spans: [WARN("  ✗ blocked by guard"), MUT(" — never reached the shell")], ms: 500 },
          { do: "call", fn: async () => {
            g.reply("✗ <b>blocked by guard</b> — <code>rm -rf /</code> matches a denied pattern.");
            await sleep(1000);
          } },

          /* 2 — held for confirmation. The tier that exists for legitimate-but-serious. */
          { do: "call", fn: async () => { await g.type("git push --force"); g.send("git push --force"); await sleep(280); } },
          { do: "out", spans: [ACC("  ▸ Mourad: "), FG("git push --force"), MUT("  (held)")], ms: 300 },
          { do: "call", fn: async () => {
            g.menu("⚠ <b>confirm</b> — <code>git push --force</code> matches a confirm rule.\nReply <code>/yes</code> to run it.", ["/yes", "/no"]);
            await sleep(1100);
          } },
          { do: "call", fn: async () => { await g.type("/yes"); g.send("/yes"); await sleep(260); } },
          { do: "cmd", text: "git push --force", speed: 12 },
          { do: "out", spans: [OK("  + 3f2a91c...8d4e07b "), FG("main -> main (forced update)")], ms: 400 },
          { do: "call", fn: async () => {
            g.reply("❯ <code>git push --force</code> · ✓ 0 · 1.4s", " + 3f2a91c...8d4e07b main -> main (forced update)");
            await sleep(900);
          } },

          /* 3 — secrets are masked on the way out. The pane shows the real thing,
             because the boundary is egress, not display. */
          { do: "call", fn: async () => { await g.type("cat .env"); g.send("cat .env"); await sleep(280); } },
          { do: "out", spans: [ACC("  ▸ Mourad: "), FG("cat .env")], ms: 200 },
          { do: "cmd", text: "cat .env", speed: 12 },
          { do: "out", spans: [ACC2("AWS_ACCESS_KEY_ID"), MUT("="), WARN("AKIA…")], ms: 90 },
          { do: "out", spans: [ACC2("ANTHROPIC_API_KEY"), MUT("="), WARN("sk-ant-…")], ms: 90 },
          { do: "out", spans: [ACC2("LOG_LEVEL"), MUT("="), FG("debug")], ms: 260 },
          { do: "out", spans: [MUT("  ◂ sent 3 lines to telegram "), OK("· 2 redacted")], ms: 300 },
          { do: "call", fn: async () => {
            g.reply("❯ <code>cat .env</code> · ✓ 0 · 0.1s",
              "AWS_ACCESS_KEY_ID=«redacted»\nANTHROPIC_«redacted»\nLOG_LEVEL=debug");
            await sleep(1100);
          } },

          /* 4 — you are mid-line at the keyboard. A command from the chat WAITS
             rather than being spliced into what you are typing. */
          { do: "typing", text: "git comm", speed: 40 },
          { do: "pause", ms: 500 },
          { do: "call", fn: async () => { await g.type("ls -la"); g.send("ls -la"); await sleep(280); } },
          { do: "call", fn: async () => {
            g.reply("queued — the terminal is busy (1 ahead). It will run when the shell is free.");
            await sleep(500);
          } },
          { do: "out", spans: [MUT("  ▸ Mourad: "), FG("ls -la"), MUT("  ⏸ queued — your line is half-typed")], ms: 1100 },

          /* …and the moment your line is sent, the queue drains. Your typing was
             never cleared to make room for it. */
          { do: "submit", text: "it -m \"wip\"" },
          { do: "out", spans: [FG("[main 8d4e07b] wip")], ms: 500 },
          { do: "out", spans: [MUT("  ▸ the line is clear — running the queued command")], ms: 400 },
          { do: "cmd", text: "ls -la", speed: 12 },
          { do: "out", spans: [FG("total 48   README.md  crates  docs  website")], ms: 400 },
          { do: "call", fn: async () => {
            g.reply("❯ <code>ls -la</code> · ✓ 0 · 0.1s", "total 48\nREADME.md  crates  docs  website");
          } },
        ]);
      },
    },

    gateKit: {
      opts: { title: "aiTerminal — @gate telegram", tabs: [{ title: "Terminal [project][@gate]", active: true }] },
      caption: () => caption("The awkward cases, handled",
        "A long build sends progress and always finishes with its <b>real exit status</b> — never abandoned part-way. A command that goes quiet (<code>sudo</code>, an <code>ssh</code> host prompt) hands your next message to its <b>stdin</b> instead of starting a new one. <code>/ai</code> asks this terminal's own AI, from the chat. And stopping always restores the terminal — raw mode off, cursor back, out of the alternate screen.",
        "<span class=\"lbl\">/ai</span><span class=\"lbl\">/cancel</span><span class=\"lbl\">/stop</span><span class=\"lbl\">idle_timeout</span>"),
      demo(w, myEpoch) {
        const g = makePhone(phoneEl);
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "out", spans: [ACC("  ⬤ telegram gate live"), DIM(" · paired with Mourad")], ms: 500 },

          /* 1 — a long command is never abandoned part-way */
          { do: "call", fn: async () => { await g.type("cargo build --release"); g.send("cargo build --release"); await sleep(280); } },
          { do: "out", spans: [ACC("  ▸ Mourad: "), FG("cargo build --release")], ms: 220 },
          { do: "cmd", text: "cargo build --release", speed: 12 },
          { do: "out", spans: [DIM("   Compiling framework v0.0.0")], ms: 500 },
          { do: "call", fn: async () => {
            g.reply("⏳ still running — <code>cargo build --release</code>, 2m elapsed.");
            await sleep(800);
          } },
          { do: "out", spans: [OK("    Finished"), FG(" `release` profile in 2m 41s")], ms: 320 },
          { do: "call", fn: async () => {
            g.reply("❯ <code>cargo build --release</code> · ✓ 0 · 2m41s", "    Finished `release` profile in 2m 41s");
            await sleep(950);
          } },

          /* 2 — something goes quiet and wants input. The next message is stdin,
             not a new command. */
          { do: "call", fn: async () => { await g.type("sudo dscacheutil -flushcache"); g.send("sudo dscacheutil -flushcache"); await sleep(280); } },
          { do: "cmd", text: "sudo dscacheutil -flushcache", speed: 12 },
          { do: "out", spans: [FG("Password:")], ms: 400 },
          { do: "call", fn: async () => {
            g.reply("⌨ <code>sudo</code> is waiting for input:", "Password:");
            await sleep(400);
          } },
          { do: "call", fn: async () => {
            g.reply("Your next message goes to it, not to a new command.");
            await sleep(900);
          } },
          { do: "out", spans: [MUT("  ▸ Mourad sent 8 characters to stdin")], ms: 400 },
          { do: "out", spans: [OK("  ✓ cache flushed")], ms: 800 },

          /* 3 — the terminal's own AI, reachable from the chat */
          { do: "call", fn: async () => { await g.type("/ai why is the release build so slow?"); g.send("/ai why is the release build so slow?"); await sleep(300); } },
          { do: "out", spans: [ACC("  ▸ Mourad: "), FG("/ai why is the release build so slow?")], ms: 260 },
          { do: "call", fn: async () => { g.typing(true); await sleep(900); g.typing(false); } },
          { do: "call", fn: async () => {
            g.reply("<code>lto = \"fat\"</code> and <code>codegen-units = 1</code> in your release profile trade build time for runtime speed. Set <code>lto = \"thin\"</code> for a faster build with most of the win.");
            await sleep(1100);
          } },

          /* 4 — and the person at the keyboard takes it back */
          { do: "cmd", text: "@gate stop" },
          { do: "out", spans: [ACC("  ⬤ gate closed"), DIM(" — stopped from this pane")], ms: 340 },
          { do: "out", spans: [MUT("  terminal restored · raw mode off · cursor back")], ms: 300 },
          { do: "call", fn: async () => { g.reply("gate closed · this terminal is no longer reachable"); } },
          { do: "out", spans: [MUT(" ")], ms: 200 },
          // the shell, back to being just yours
          { do: "out", spans: [ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), S("t-cursor", " ")] },
        ]);
      },
    },

    guard: {
      caption: () => caption("The AI proposes. <em>The guard disposes.</em>",
        "What may <b>run</b>: allow · ⚠ confirm · ✗ deny, and deny always wins. Every rule is a <b>regex</b>, so one line covers a family of commands — and every line, every pipeline stage and each stage's program is judged, so a harmless first word cannot shield what follows it."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },

          /* The rules first: they are the feature, and they are data you write. */
          { do: "cmd", text: "cat ~/.aiTerminal/config.toml" },
          { do: "out", spans: [ACC2("[[guard.command]]")], ms: 55 },
          { do: "out", spans: [MUT("pattern = "), FG("\"git\\s+push\\s+.*--force\""), DIM("   # a regex, not a word list")], ms: 55 },
          { do: "out", spans: [MUT("rule    = "), FG("\"confirm\"")], ms: 55 },
          { do: "out", spans: [ACC2("[[guard.command]]")], ms: 55 },
          { do: "out", spans: [MUT("pattern = "), FG("\"\\bmkfs\\b|dd\\s+.*of=/dev/\""), DIM("   # one line, a whole family")], ms: 55 },
          { do: "out", spans: [MUT("rule    = "), FG("\"deny\"")], ms: 55 },
          { do: "pause", ms: 900 },

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

    paths: {
      caption: () => caption("Some <em>files and folders</em> are simply not yours to read",
        "What may be <b>touched</b>: the same three tiers, as <b>regex</b> over the full path. One line covers a folder and everything under it; another covers a kind of <em>file</em>, wherever it lives. They reach the file tools <em>and</em> the paths a command names — because <code>cat ~/.ssh/id_rsa</code> never goes near <code>fs.read</code>. A refused step is information, not a crash: the agent works around it."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },

          /* A folder rule, a file rule and a read-only rule — all regex. */
          { do: "cmd", text: "cat ~/.aiTerminal/config.toml" },
          { do: "out", spans: [ACC2("[[guard.path]]")], ms: 55 },
          { do: "out", spans: [MUT("pattern = "), FG("\"(^|/)\\.ssh/\""), DIM("   # a FOLDER, and everything under it")], ms: 55 },
          { do: "out", spans: [MUT("rule    = "), FG("\"deny\"")], ms: 55 },
          { do: "out", spans: [ACC2("[[guard.path]]")], ms: 55 },
          { do: "out", spans: [MUT("pattern = "), FG("\"\\.(pem|key|p12)$\""), DIM("   # a kind of FILE, wherever it lives")], ms: 55 },
          { do: "out", spans: [MUT("rule    = "), FG("\"deny\"")], ms: 55 },
          { do: "out", spans: [ACC2("[[guard.path]]")], ms: 55 },
          { do: "out", spans: [MUT("pattern = "), FG("\"^/etc/\""), DIM("   # read it, never change it")], ms: 55 },
          { do: "out", spans: [MUT("rule    = "), FG("\"read-only\"")], ms: 55 },
          { do: "pause", ms: 900 },

          { do: "cmd", text: "@agent coder \"why is the deploy failing?\"" },
          { do: "out", spans: [MUT("  ⚙ fs.read   "), FG("~/.ssh/config")], ms: 90 },
          { do: "out", spans: [ERR("  ⛔ refused"), DIM(" — the folder is off limits  /(^|/)\\.ssh//")], ms: 90 },
          { do: "out", spans: [MUT("  ⚙ fs.read   "), FG("deploy/certs/server.pem")], ms: 90 },
          { do: "out", spans: [ERR("  ⛔ refused"), DIM(" — the file is off limits  /\\.(pem|key|p12)$/")], ms: 90 },
          { do: "out", spans: [MUT("  ⚙ fs.read   "), FG("deploy/config.yml"), MUT("  ·  1.2KB")], ms: 90 },
          { do: "out", spans: [MUT("  ⚙ diag.check"), FG("  deploy/"), MUT("  ·  2 errors")], ms: 90 },
          { do: "pause", ms: 700 },
          { do: "out", spans: [FG("The host key check is off, and "), ACC2("deploy/config.yml"), FG(" points at the old bastion.")] },
          { do: "out", spans: [DIM("I could not read your ssh config — tell me the host and I'll finish it.")] },
          { do: "pause", ms: 1200 },

          // a shell one-liner never goes near fs.read, so the guard reads the paths it names
          { do: "cmd", text: "@ai show me my cloud credentials" },
          { do: "spin", label: "thinking…", ms: 700 },
          { do: "out", spans: [ERR("# blocked by guard: it names \"~/.aws/credentials\", which matches an off-limits path")] },
          { do: "pause", ms: 1100 },

          // read-only: look all you like, change nothing
          { do: "cmd", text: "@agent coder \"add our proxy to /etc/hosts\"" },
          { do: "out", spans: [MUT("  ⚙ fs.read   "), FG("/etc/hosts"), MUT("  ·  41 lines")], ms: 90 },
          { do: "out", spans: [ERR("  ⛔ refused"), DIM(" — /etc/hosts is read-only here  /^\\/etc\\//")], ms: 90 },
          { do: "out", spans: [FG("Here is the line to add, and the one command that adds it:")] },
          { do: "out", spans: [ACC2("  10.0.42.17  proxy.internal")] },
        ]);
      },
    },

    redact: {
      caption: () => caption("Your secrets leave as <em>placeholders</em> — and come back",
        "What may <b>leave</b>: a value matching one of your <b>regex</b> rules goes out as <code>«credential-1»</code> and becomes itself again the moment the text returns to your machine — so an agent can <em>use</em> a password it was never shown. Nine rules ship for the usual keys, tokens and sensitive <code>KEY=value</code> pairs; add <code>scope = \"terminal\"</code> to mask them on your screen too."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },

          /* Same vocabulary, third subject — and the name is what the placeholder
             is called, never anything about the value. */
          { do: "cmd", text: "cat ~/.aiTerminal/config.toml" },
          { do: "out", spans: [ACC2("[[guard.secret]]")], ms: 55 },
          { do: "out", spans: [MUT("pattern = "), FG("\"sk-[A-Za-z0-9_-]{16,}\""), DIM("   # a shape, not a list of keys")], ms: 55 },
          { do: "out", spans: [MUT("name    = "), FG("\"api-key\""), DIM("   # names the placeholder")], ms: 55 },
          { do: "out", spans: [ACC2("[[guard.secret]]")], ms: 55 },
          { do: "out", spans: [MUT("pattern = "), FG("\"(?i)(password|token|secret)\\s*[:=]\\s*\\S+\"")], ms: 55 },
          { do: "out", spans: [MUT("name    = "), FG("\"credential\"")], ms: 55 },
          { do: "pause", ms: 900 },

          { do: "cmd", text: "cat .env" },
          { do: "out", spans: [ACC2("DATABASE_URL"), MUT("="), FG("postgres://db.internal/prod")], ms: 70 },
          { do: "out", spans: [ACC2("AWS_ACCESS_KEY_ID"), MUT("="), WARN("AKIA…")], ms: 70 },
          { do: "out", spans: [ACC2("ANTHROPIC_API_KEY"), MUT("="), WARN("sk-ant-…")], ms: 70 },
          { do: "out", spans: [ACC2("GITHUB_TOKEN"), MUT("="), WARN("ghp_…")], ms: 70 },
          { do: "out", spans: [ACC2("LOG_LEVEL"), MUT("="), FG("debug")], ms: 900 },

          // your own machine shows you everything — the boundary is egress, not display
          { do: "cmd", text: "@ai why can't the app reach the database?" },
          { do: "spin", label: "reading your terminal…", ms: 1000 },
          { do: "call", fn: async (t) => buildRedactView(t) },
          { do: "pause", ms: 1400 },
          { do: "out", spans: [FG("db.internal has no port — try "), ACC2("db.internal:5432"), FG(" in DATABASE_URL")] },
          { do: "pause", ms: 900 },
          { do: "cmd", text: "@agent coder \"connect and count the users\"" },
          { do: "out", spans: [MUT("  ⚙ sys.run  "), FG("psql postgres://app:"), WARN("«credential-1»"), FG("@db.internal:5432 -c …")], ms: 80 },
          { do: "out", spans: [MUT("  ↺ the real value went back in here, and nowhere else")], ms: 80 },
          { do: "out", spans: [FG(" count ")], ms: 60 },
          { do: "out", spans: [FG(" 1 284 ")], ms: 60 },
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
