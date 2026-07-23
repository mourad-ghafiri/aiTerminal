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
  if (!host || !captionEl) return;

  let current = null;
  let epoch = 0; // a rebuilt window invalidates any in-flight script

  function fresh(opts) {
    epoch++;
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
