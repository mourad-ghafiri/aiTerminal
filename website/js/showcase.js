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

  /* The phone (`makePhone`), the `@md` split editor (`buildMdEditor`) and the
     redactor view (`buildRedactView`) come from js/scenes.js — video.html
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
      caption: () => caption("<code>@agent</code> — eight specialists at your prompt",
        "<code>@agent</code> lists what you have, <code>@agent &lt;name&gt;</code> shows one in full, and <code>@&lt;name&gt;</code> runs it. They are not all for writing code: <b>researcher</b> searches the web and reads the pages (keyless — no extra account, just <code>[ai] network = true</code>), <b>writer</b> drafts and saves the file, <b>planner</b> turns a goal into a plan. Each is an editable Markdown file."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },
          { do: "cmd", text: "@agent" },
          { do: "out", spans: [FG("agents (8):")] },
          { do: "out", spans: [FG("  ai         "), DIM("19 tools ·  6 steps  General assistant — a command to review, or an answer")], ms: 60 },
          { do: "out", spans: [FG("  coder      "), DIM("24 tools · 24 steps  Senior engineer — the smallest correct edit, verified")], ms: 60 },
          { do: "out", spans: [FG("  explorer   "), DIM(" 7 tools · 12 steps  Read-only scout — maps the code and reports tightly")], ms: 60 },
          { do: "out", spans: [FG("  planner    "), DIM(" 7 tools · 10 steps  Turns a goal into a plan with a concrete done-when")], ms: 60 },
          { do: "out", spans: [FG("  researcher "), DIM("12 tools · 16 steps  Finds sources, reads them, reports what they say")], ms: 60 },
          { do: "out", spans: [FG("  reviewer   "), DIM(" 7 tools · 12 steps  Read-only review — correctness, security, design")], ms: 60 },
          { do: "out", spans: [FG("  tester     "), DIM("13 tools · 18 steps  Runs the project's own tests, reports what happened")], ms: 60 },
          { do: "out", spans: [FG("  writer     "), DIM("10 tools · 14 steps  Docs and reports — and saves the file")], ms: 60 },
          { do: "pause", ms: 1000 },

          /* everyday: nothing here is about code */
          { do: "cmd", text: "@researcher \"which cordless drills do reviewers actually rate, and why\"" },
          { do: "out", spans: [ACC("✦ @researcher"), MUT(" · claude-opus-4-8")] },
          { do: "tool", name: "web.search", args: '{"query":"cordless drill reviews 2026"}', ms: 340, size: "3.4KB" },
          { do: "tool", name: "web.read", args: '{"url":"https://…/tool-reviews"}', ms: 1180, size: "18KB" },
          { do: "tool", name: "web.read", args: '{"url":"https://…/teardown"}', ms: 940, size: "11KB" },
          { do: "stream", spans: [FG("Three names recur, for two different reasons — and one of them is a rebrand. Sources and dates below; I could not confirm the torque figure anywhere.")], speed: 10 },
          { do: "footer", text: "14.2s · 3 tools · 9.1k in / 1.4k out" },
          { do: "pause", ms: 1100 },

          { do: "cmd", text: "@writer \"turn @notes.md into a one-page brief at brief.md\"" },
          { do: "out", spans: [ACC("✦ @writer"), MUT(" · claude-opus-4-8")] },
          { do: "tool", name: "fs.read", args: '{"path":"notes.md"}', ms: 4, size: "3.9KB" },
          { do: "tool", name: "fs.write", args: '{"path":"brief.md"}', ms: 6, size: "2.1KB" },
          { do: "stream", spans: [FG("Wrote brief.md — one page, the decision first. Two claims I could not check are marked.")], speed: 10 },
          { do: "pause", ms: 1100 },

          /* and the same harness on code */
          { do: "cmd", text: "@coder \"fix the failing parser test\"" },
          { do: "out", spans: [ACC("✦ @coder"), MUT(" · claude-opus-4-8")] },
          { do: "spin", label: "thinking…", ms: 900 },
          { do: "think", text: "The test expects a trailing newline — the parser drops it on the last line…" },
          { do: "tool", name: "todo.set", args: '{"items":["find it","fix it","re-run"]}', ms: 3, size: "88B" },
          { do: "tool", name: "fs.search", args: '{"q":"parse_line"}', ms: 18, size: "2.1KB" },
          { do: "tool", name: "fs.edit", args: '{"path":"src/parser.rs"}', ms: 6, size: "412B" },
          { do: "tool", name: "sys.run", args: '{"cmd":"cargo test parser"}', ms: 2100, size: "1.4KB" },
          { do: "stream", spans: [FG("The fix: the parser dropped the final line — added the flush in "), ACC2("parse_line()"), FG(".")], speed: 11 },
          { do: "footer", text: "8.4s · 4 tools · 12.3k in / 1.8k out" },
        ]);
      },
    },

    flow: {
      caption: () => caption("<code>@flow</code> — a <em>graph</em> of agents",
        "Nodes that need nothing from each other run at the same time, a condition routes on the edge, and one edge points backwards so a failing check loops through a fixer — bounded. Nothing runs until the graph is proved. Five ship: <b>build · fix · review</b> for code, and <b>research</b> for any question at all. A bare goal is routed by the model, which says which flow and why before spending anything."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },

          /* everyday first: a question, not a repo */
          { do: "cmd", text: "@flow \"which e-bike should I buy for a hilly commute\"" },
          { do: "out", spans: [ACC("▸ research"), MUT(" — the goal asks for sources and a comparison, not a code change")] },
          { do: "out", spans: [ACC("▸ research · which e-bike should I buy for a hilly commute")] },
          { do: "out", spans: [OK("  ✓"), FG(" plan     "), DIM("@planner      3.1s   1.2k   → 5 sub-questions")], ms: 300 },
          { do: "out", spans: [ACC("  ⠻"), FG(" gather   "), DIM("@researcher ×n        × 5 items   ⚙ web.search")], ms: 900 },
          { do: "out", spans: [OK("  ✓"), FG(" gather   "), DIM("@researcher  22.4s  18.9k   × 5 items")], ms: 300 },
          { do: "out", spans: [OK("  ✓"), FG(" compare  "), DIM("@researcher   8.2s   6.4k")], ms: 240 },
          { do: "out", spans: [OK("  ✓"), FG(" report   "), DIM("@writer       6.7s   3.1k")], ms: 240 },
          { do: "out", spans: [DIM("  4/4 done · 29.6k tokens · 40.4s")] },
          { do: "stream", spans: [FG("Two real contenders, and the reason is the motor, not the battery. Where the sources disagree is called out; one spec I could not confirm.")], speed: 10 },
          { do: "pause", ms: 1200 },

          /* then the same engine on code */
          { do: "cmd", text: "@flow \"add a --json flag to the export command\"" },
          { do: "out", spans: [ACC("▸ build"), MUT(" — it asks for working code, tested")] },
          { do: "out", spans: [ACC("▸ build · add a --json flag to the export command")] },
          { do: "out", spans: [OK("  ✓"), FG(" plan        "), DIM("@planner      4.2s   3.1k")], ms: 240 },
          { do: "out", spans: [OK("  ✓"), FG(" explore     "), DIM("@explorer     8.1s   9.4k")], ms: 200 },
          { do: "out", spans: [OK("  ✓"), FG(" conventions "), DIM("@explorer     7.6s   8.8k   ← both scouts, at once")], ms: 200 },
          { do: "out", spans: [ACC("  ⠻"), FG(" apply       "), DIM("@coder       12.3s          ⚙ fs.edit src/cli.rs")], ms: 800 },
          { do: "out", spans: [OK("  ✓"), FG(" apply       "), DIM("@coder       12.3s   6.2k")], ms: 200 },
          { do: "out", spans: [ERR("  ✗"), FG(" verify      "), DIM("@tester       9.8s   4.1k   VERDICT: FAIL")], ms: 300 },
          { do: "out", spans: [OK("  ✓"), FG(" fix         "), DIM("@coder        7.2s   3.3k")], ms: 240 },
          { do: "out", spans: [OK("  ✓"), FG(" verify      "), DIM("@tester       9.1s   4.0k   ×2  VERDICT: PASS")], ms: 240 },
          { do: "out", spans: [OK("  ✓"), FG(" review      "), DIM("@reviewer     6.4s   5.1k")], ms: 200 },
          { do: "out", spans: [OK("  ✓"), FG(" summary     "), DIM("@writer       3.9s   1.9k")], ms: 200 },
          { do: "out", spans: [DIM("  8/8 done · 46.3k tokens · 1m04s")] },
          { do: "pause", ms: 1000 },

          /* free, and needs no model at all */
          { do: "cmd", text: "@flow graph build" },
          { do: "out", spans: [DIM("        ┌───────────────┐")] },
          { do: "out", spans: [DIM("        │ plan @planner │")] },
          { do: "out", spans: [DIM("        └───────┬───────┘")] },
          { do: "out", spans: [DIM("      ┌─────────┴─────────┐")] },
          { do: "out", spans: [DIM("      ▼                   ▼")] },
          { do: "out", spans: [DIM(" ┌────┴─────┐   ┌─────────┴─────────┐")] },
          { do: "out", spans: [DIM(" │ explore  │   │    conventions    │")] },
          { do: "out", spans: [DIM(" └────┬─────┘   └─────────┬─────────┘")] },
          { do: "out", spans: [DIM("      └─────────┬─────────┘")] },
          { do: "out", spans: [DIM("                ▼")] },
          { do: "out", spans: [DIM("        ┌───────┴────────┐ up to 3x")] },
          { do: "out", spans: [DIM("        │ verify @tester │◀╌╌╌┐")] },
          { do: "out", spans: [DIM("        └───────┬────────┘    ╎")] },
          { do: "out", spans: [DIM("   VERDICT: FAIL│VERDICT: PASS ╎")] },
          { do: "out", spans: [DIM("      ┌─────────┴─────────┐   ╎")] },
          { do: "out", spans: [DIM("      ▼                   ▼   ╎")] },
          { do: "out", spans: [DIM(" ┌────┴─────┐   ┌──────────┴───────┐")] },
          { do: "out", spans: [DIM(" │ fix      │╌╌╌│ review @reviewer │")] },
          { do: "out", spans: [DIM(" └──────────┘   └──────────────────┘")] },
          { do: "out", spans: [MUT("  ← @flow graph and @flow check need no model and spend nothing")] },
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
          { do: "tool", name: "fs.edit", args: '{"path":"src/config.rs"}', ms: 7, size: "610B" },
          { do: "out", spans: [DIM("  check: exit=1 · assertion failed: default theme")], ms: 500 },
          { do: "out", spans: [FG("▶ iteration 2/5")] },
          { do: "tool", name: "fs.edit", args: '{"path":"src/config.rs"}', ms: 5, size: "188B" },
          { do: "out", spans: [DIM("  check: exit=0")], ms: 400 },
          { do: "out", spans: [OK("✓ goal reached after 2 iteration(s)")] },
          { do: "pause", ms: 1200 },

          /* the same machine, no code in sight — `wc` and `test` are all it takes */
          { do: "cmd", text: "@loop \"cut the intro in intro.md under 200 words without losing the point\" --agent writer --check \"test $(wc -w < intro.md) -lt 200\"" },
          { do: "out", spans: [FG("🔁 loop 'writer' — up to 5 iteration(s)")] },
          { do: "out", spans: [DIM("  verifier: test $(wc -w < intro.md) -lt 200")] },
          { do: "out", spans: [FG("▶ iteration 1/5")] },
          { do: "tool", name: "fs.edit", args: '{"path":"intro.md"}', ms: 5, size: "1.2KB" },
          { do: "out", spans: [DIM("  check: exit=1")], ms: 420 },
          { do: "out", spans: [FG("▶ iteration 2/5")] },
          { do: "tool", name: "fs.edit", args: '{"path":"intro.md"}', ms: 4, size: "640B" },
          { do: "out", spans: [DIM("  check: exit=0")], ms: 380 },
          { do: "out", spans: [OK("✓ goal reached after 2 iteration(s)")] },
          { do: "out", spans: [MUT("  ← the goal was prose; the check was still a command that exits 0")] },
        ]);
      },
    },

    job: {
      caption: () => caption("<code>@job</code> — say what to do, and when",
        "The AI reads the schedule out of your sentence <em>once</em>, at creation, and writes it into the record as cron — so every run after that is plain arithmetic. Recurring jobs survive a reboot: a missed one catches up exactly once. And a job that runs a <em>command</em> (<code>@job -- …</code>) needs no model at all."),
      demo(w, myEpoch) {
        run(w, myEpoch, [
          { do: "pause", ms: 300 },

          /* plain English, no repo anywhere near it */
          { do: "cmd", text: '@job "every Monday at 9, summarise what landed in ~/Documents/inbox into ~/Documents/weekly.md"' },
          { do: "out", spans: [FG("⧖ every Monday at 09:00 — summarise what landed in ~/Documents/inbox … · job 1753112060-4302")] },
          { do: "out", spans: [DIM("  cron 0 9 * * 1  ·  fires in 3d  ·  list: @job  ·  cancel: @job cancel 1753112060-4302")] },
          { do: "pause", ms: 900 },

          { do: "cmd", text: '@job "summarize the kafka logs into ~/reports/kafka.md every hour"' },
          { do: "out", spans: [FG("⧖ every hour — summarize the kafka logs into ~/reports/kafka.md · job 1753112100-4310")] },
          { do: "out", spans: [DIM("  fires in 1h  ·  cron 0 * * * *")] },
          { do: "pause", ms: 900 },

          { do: "cmd", text: "@job --every 15m -- ./sync.sh" },
          { do: "out", spans: [FG("⧖ every 15m — ./sync.sh · job 1753112140-4318")] },
          { do: "out", spans: [DIM("  fires in 15m  ·  no model needed to run this one")] },
          { do: "pause", ms: 900 },

          { do: "cmd", text: "@job" },
          { do: "out", spans: [FG("background jobs (4):")] },
          { do: "out", spans: [FG("  ⧖ 1753112060-4302 scheduled summarise what landed in ~/Documents… "), DIM("(fires in 3d)")] },
          { do: "out", spans: [DIM("      cron 0 9 * * 1  ·  4 run(s)  ·  last ok")] },
          { do: "out", spans: [FG("  ⧖ 1753112100-4310 scheduled summarize the kafka logs … "), DIM("(fires in 48m)")] },
          { do: "out", spans: [DIM("      cron 0 * * * *  ·  12 run(s)  ·  last ok")] },
          { do: "out", spans: [FG("  ▶ 1753112000-4242 running   audit the deps … "), DIM("(2m ago · 2m)")] },
          { do: "out", spans: [OK("  ✓ "), FG("1753111800-4101 done      create a CHANGELOG … "), DIM("(9m ago · 45s)")] },
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
          { do: "out", spans: [FG("Release plan")] },
          { do: "out", spans: [MUT("\u2500".repeat(48))] },
          { do: "out", spans: [] },
          { do: "out", spans: [ACC("\u2022 "), FG("cut the branch")] },
          { do: "out", spans: [ACC("\u2022 "), FG("run the suite")] },
          { do: "out", spans: [] },
          { do: "out", spans: [ACC(" \u250c\u2500\u2500\u2500\u2510   \u250c\u2500\u2500\u2500\u2510   \u250c\u2500\u2500\u2500\u2510")] },
          { do: "out", spans: [ACC(" \u2502 A \u2502"), MUT("\u2500\u2500\u25b6"), ACC("\u2524 B \u2502"), MUT("\u2500\u2500\u25b6"), ACC("\u2524 C \u2502")] },
          { do: "out", spans: [ACC(" \u2514\u2500\u2500\u2500\u2518   \u2514\u2500\u2500\u2500\u2518   \u2514\u2500\u2500\u2500\u2518")] },
          { do: "out", spans: [] },
          { do: "out", spans: [MUT("  \u2190 a file longer than the window opens a scrollable pager instead")] },
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
          { do: "out", spans: [OK("test result: ok"), FG(". 854 passed; 0 failed")], ms: 300 },
          { do: "call", fn: async () => {
            g.reply("❯ <code>cargo test</code> · ✓ 0 · 41s",
              "…\ntest result: ok. 854 passed; 0 failed");
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
        "A remote command goes through the same <code>[security]</code> rules as an AI suggestion — <b>denied</b> outright, or <b>held</b> until you reply <code>/yes</code>. Your <code>[[redact]]</code> rules apply to everything leaving the machine, in <em>both</em> scopes, because a chat app is off-machine either way. And every remote command, block and reply is echoed in the pane, so the screen in front of you is always the truth.",
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
          { do: "out", spans: [ACC2("AWS_ACCESS_KEY_ID"), MUT("="), WARN("AKIA3RJHF2P9QLXMZB4T")], ms: 90 },
          { do: "out", spans: [ACC2("ANTHROPIC_API_KEY"), MUT("="), WARN("sk-ant-api03-9Fk2LmQ7xTvB")], ms: 90 },
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
