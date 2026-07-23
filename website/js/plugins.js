/* ============================================================================
   plugins.js — the plugin showcase: the same presentation as the features.
   Pick a plugin from the grouped menu → the window demos it ONCE and rests.
   All 31 builtins, each with its own mini session. Click-driven, no loops.
   ========================================================================== */

document.addEventListener("DOMContentLoaded", () => {
  const host = document.getElementById("plugins-window");
  const captionEl = document.getElementById("plugins-caption");
  if (!host || !captionEl) return;

  let current = null;
  let epoch = 0;

  function fresh(opts) {
    epoch++;
    return makeWindow(host, Object.assign(
      { title: "aiTerminal", tabs: [{ title: "Terminal [project][zsh]", active: true }] },
      opts || {}));
  }

  async function run(w, myEpoch, steps) {
    for (const st of steps) {
      if (myEpoch !== epoch) return;
      switch (st.do) {
        case "cmd": await typeCmd(w, st.text, st); break;
        case "out": w.line(st.spans, st.paneIdx); await sleep(st.ms || 120); break;
        case "stream": await streamLine(w, st.spans, st); break;
        case "spin": await spinner(w, st.label || "thinking…", st.ms || 900, st); break;
        case "pause": await sleep(st.ms); break;
        case "call": await st.fn(w); break;
      }
    }
  }

  const caption = (name, text) => {
    captionEl.innerHTML = `<h3><code>${name}</code></h3><p>${text}</p>`;
  };

  /* one entry per builtin plugin: caption line + a tiny session */
  const PLUGINS = {

    /* ---- shell UX ---- */
    "syntax-highlight": {
      text: "Colors the command line as you type — valid commands, strings, flags; <code>@commands</code> in your accent.",
      demo: (w) => [
        { do: "out", spans: [ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), FG("cargo build "), ACC2("--release")] },
        { do: "out", spans: [ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), ERR("carg"), DIM("   ← unknown command, shown red as you type")] },
        { do: "out", spans: [ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), S("t-accent-b", "@coder"), ACC2(" \"fix the test\""), DIM("   ← @command in accent")] },
      ],
    },
    "autosuggest": {
      text: "Inline suggestions from your history as you type — press → to accept.",
      demo: (w) => [
        { do: "out", spans: [ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), FG("cargo t"), MUT("est --workspace")] },
        { do: "out", spans: [DIM("  → accepts · ghost text comes from YOUR history")] },
        { do: "cmd", text: "cargo test --workspace", speed: 8 },
        { do: "out", spans: [OK("    Finished"), FG(" test — 507 passed")] },
      ],
    },
    "history": {
      text: "Remembers everything, searchably — type a prefix, ↑ walks matching history.",
      demo: (w) => [
        { do: "out", spans: [ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), FG("git "), S("t-cursor", "")] },
        { do: "out", spans: [DIM("  ↑ git push origin main")] },
        { do: "out", spans: [DIM("  ↑ git rebase -i HEAD~3")] },
        { do: "out", spans: [DIM("  ↑ git stash pop   ← only git-prefixed entries")] },
      ],
    },
    "completion": {
      text: "Intelligent tab-completion — your aliases complete like the real command.",
      demo: (w) => [
        { do: "cmd", text: "gco ⇥" },
        { do: "out", spans: [FG("main   fix/parser   feat/themes   release/2.4")] },
        { do: "out", spans: [DIM("  gco = git checkout — the alias inherits its completions")] },
      ],
    },
    "lineedit": {
      text: "Edit commands like a text field — ⌥/⌘ arrows jump by word/line, ⇧ selects, typing replaces, Esc cancels, ⌘C copies.",
      demo: (w) => [
        { do: "out", spans: [ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), FG("cargo test "), S("t-sel", "--workspace")] },
        { do: "out", spans: [DIM("  ⇧⌥← selected a word — a light band, the colors stay")] },
        { do: "out", spans: [ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), FG("cargo test "), FG("--doc"), S("t-cursor", "")] },
        { do: "out", spans: [DIM("  typing replaced it · ⌫ deletes · Esc cancels · ⌘C copies")] },
      ],
    },
    "alias-hints": {
      text: "Suggests the shortest alias for any command you type — even with args, once per alias.",
      demo: (w) => [
        { do: "cmd", text: "git status" },
        { do: "out", spans: [DIM("  hint: gst → git status")] },
        { do: "cmd", text: "docker ps" },
        { do: "out", spans: [DIM("  hint: dps → docker ps")] },
      ],
    },
    "prompt": {
      text: "Default status segments — user@host and the clock — plus the themed ❯ prompt.",
      demo: (w) => [
        { do: "out", spans: [ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), S("t-cursor", "")] },
        { do: "out", spans: [DIM("  path · git branch · ❯ — all in your theme's colors")] },
        { do: "call", fn: async (t) => t.note("status bar: CPU · MEM · clock, live") },
      ],
    },
    "sudo": {
      text: "Esc Esc adds (or removes) sudo on the current — or previous — command.",
      demo: (w) => [
        { do: "out", spans: [ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), FG("systemctl restart nginx")] },
        { do: "call", fn: async (t) => t.note("Esc Esc") },
        { do: "pause", ms: 500 },
        { do: "out", spans: [ACC("~/project"), OK(" ⎇ main"), ACC2(" ❯ "), WARN("sudo "), FG("systemctl restart nginx")] },
      ],
    },
    "dir": {
      text: "Working-directory segment, navigation aliases, dir history keys, and mkcd/take.",
      demo: (w) => [
        { do: "cmd", text: "mkcd experiments/v2" },
        { do: "out", spans: [DIM("created + entered ~/project/experiments/v2")] },
        { do: "cmd", text: "..." },
        { do: "out", spans: [DIM("→ ~/project   (… = up two)")] },
      ],
    },
    "jump": {
      text: "Jump to frequently-used directories by name (frecency) plus named bookmarks.",
      demo: (w) => [
        { do: "cmd", text: "j term" },
        { do: "out", spans: [ACC("→ ~/Data/Projects/the-terminal")] },
        { do: "cmd", text: "mark infra" },
        { do: "out", spans: [DIM("marked ~/work/infra as @infra")] },
        { do: "cmd", text: "marks" },
        { do: "out", spans: [DIM("  @infra          ~/work/infra")] },
      ],
    },
    "term-cwd": {
      text: "Reports the working directory + host via OSC 7 — an instant, SSH-aware status bar.",
      demo: (w) => [
        { do: "cmd", text: "ssh deploy@prod" },
        { do: "cmd", text: "cd /var/www", prompt: [ACC("prod "), ACC2("❯ ")] },
        { do: "call", fn: async (t) => { t.defaultStatus({ cwd: "/var/www", branch: "release" }); t.note("status bar follows — even over SSH"); } },
      ],
    },

    /* ---- git & dev ---- */
    "git": {
      text: "Status segment (staged + unstaged dots), 100+ aliases, branch helpers, completions.",
      demo: (w) => [
        { do: "cmd", text: "gst" },
        { do: "out", spans: [OK("## main...origin/main"), WARN("  ●2 unstaged")] },
        { do: "cmd", text: "gcam \"fix parser newline\"" },
        { do: "out", spans: [FG("[main a1c2f3] fix parser newline — 2 files changed")] },
        { do: "call", fn: async (t) => t.note("⎇ main ● ● in the status bar") },
      ],
    },
    "github": {
      text: "GitHub CLI mastery — PRs, issues, repos, runs, gists — plus a clone-and-cd helper.",
      demo: (w) => [
        { do: "cmd", text: "ghprc" },
        { do: "out", spans: [FG("creating PR: fix/parser → main …")] },
        { do: "out", spans: [OK("✓ "), ACC2("github.com/org/repo/pull/128")] },
        { do: "cmd", text: "ghprco 128" },
        { do: "out", spans: [DIM("checked out PR #128 → fix/parser")] },
      ],
    },
    "docker": {
      text: "Container/image/network/volume verbs, exec-into-shell, compose — aliases + completions.",
      demo: (w) => [
        { do: "cmd", text: "dps" },
        { do: "out", spans: [FG("api    Up 2 days   0.0.0.0:8080")] },
        { do: "out", spans: [FG("db     Up 2 days   5432/tcp")] },
        { do: "cmd", text: "dsh api" },
        { do: "out", spans: [DIM("→ /bin/sh inside api")] },
      ],
    },
    "kubernetes": {
      text: "The full kubectl matrix + helm, JSON/YAML helpers, and a context·namespace segment.",
      demo: (w) => [
        { do: "cmd", text: "kgp" },
        { do: "out", spans: [FG("api-7d4b   1/1  Running   2d")] },
        { do: "out", spans: [FG("web-9f21   1/1  Running   2d")] },
        { do: "cmd", text: "kl api-7d4b" },
        { do: "out", spans: [DIM("streaming logs…  (☸ prod·default in the status bar)")] },
      ],
    },
    "rust": {
      text: "Comprehensive cargo shortcuts — build/test/clippy/watch — and a crate-name segment.",
      demo: (w) => [
        { do: "cmd", text: "ct" },
        { do: "out", spans: [OK("    Finished"), FG(" test — 507 passed")] },
        { do: "cmd", text: "ccl" },
        { do: "out", spans: [OK("    Finished"), FG(" clippy — no warnings")] },
        { do: "call", fn: async (t) => t.note("🦀 the-terminal in the status bar") },
      ],
    },
    "python": {
      text: "python/pip/venv/pytest + uv & poetry shortcuts, a __pycache__ cleaner, version segment.",
      demo: (w) => [
        { do: "cmd", text: "venv" },
        { do: "out", spans: [DIM(".venv created — python3 -m venv .venv")] },
        { do: "cmd", text: "pyt" },
        { do: "out", spans: [OK("42 passed"), FG(" in 1.8s")] },
        { do: "cmd", text: "pyclean" },
        { do: "out", spans: [DIM("removed 14 __pycache__ dirs")] },
      ],
    },
    "node": {
      text: "npm/yarn/pnpm shortcuts, an `nps` script lister, completions, a Node-version segment.",
      demo: (w) => [
        { do: "cmd", text: "nps" },
        { do: "out", spans: [FG("dev · build · test · lint · typecheck")] },
        { do: "cmd", text: "nr dev" },
        { do: "out", spans: [OK("  ➜  ready "), FG("on http://localhost:3000")] },
      ],
    },

    /* ---- utilities ---- */
    "extract": {
      text: "Extract any archive with <code>x</code>, create one with <code>pack</code> — 15+ formats.",
      demo: (w) => [
        { do: "cmd", text: "x release.tar.zst" },
        { do: "out", spans: [FG("extracted release.tar.zst → ./release/")] },
        { do: "cmd", text: "pack logs.tar.gz logs/" },
        { do: "out", spans: [FG("packed logs/ → logs.tar.gz (4.2 MB)")] },
      ],
    },
    "clipboard": {
      text: "clip · clippaste · copypath · copyfile · copyline — pipe anything to/from the clipboard.",
      demo: (w) => [
        { do: "cmd", text: "cat error.log | clip" },
        { do: "out", spans: [DIM("→ clipboard (2.1 KB)")] },
        { do: "cmd", text: "copypath src/parser.rs" },
        { do: "out", spans: [DIM("→ clipboard: ~/project/src/parser.rs")] },
      ],
    },
    "encode": {
      text: "base64/hex/URL/JSON/ROT13 + SHA-1/256/512/UUID — encode, decode & hash from the shell.",
      demo: (w) => [
        { do: "cmd", text: "b64 \"user:pass\"" },
        { do: "out", spans: [FG("dXNlcjpwYXNz")] },
        { do: "cmd", text: "sha release.tar.gz" },
        { do: "out", spans: [FG("9f86d08…a4c2  release.tar.gz")] },
        { do: "cmd", text: "uuid" },
        { do: "out", spans: [FG("8f4e2c1a-77d3-4b0e-9a12-3f5d8e6c21aa")] },
      ],
    },
    "web-search": {
      text: "Search 18 engines from the shell — Google, Stack Overflow, GitHub, MDN, crates, npm…",
      demo: (w) => [
        { do: "cmd", text: "so \"borrow checker E0502\"" },
        { do: "out", spans: [DIM("→ stackoverflow.com/search?q=borrow+checker+E0502")] },
        { do: "cmd", text: "crates \"toml parser\"" },
        { do: "out", spans: [DIM("→ crates.io/search?q=toml+parser")] },
      ],
    },
    "weather": {
      text: "Current weather from the shell — <code>weather &lt;city&gt;</code>.",
      demo: (w) => [
        { do: "cmd", text: "weather Paris" },
        { do: "out", spans: [FG("Paris: ⛅ 22°C · wind 12 km/h · humidity 54%")] },
      ],
    },
    "world-clock": {
      text: "Timezone helpers — tz · wclock · utc.",
      demo: (w) => [
        { do: "cmd", text: "tz tokyo" },
        { do: "out", spans: [FG("Tokyo: 22:32 (+9h)")] },
        { do: "cmd", text: "wclock" },
        { do: "out", spans: [FG("SF 06:32 · NYC 09:32 · London 14:32 · Tokyo 22:32")] },
      ],
    },
    "notes": {
      text: "Quick <code>note</code> capture right at the shell — <code>notes</code> lists, <code>noteclear</code> wipes.",
      demo: (w) => [
        { do: "cmd", text: "note \"rotate the staging certs before Friday\"" },
        { do: "out", spans: [DIM("noted")] },
        { do: "cmd", text: "notes" },
        { do: "out", spans: [FG("· rotate the staging certs before Friday")] },
        { do: "out", spans: [FG("· bump MSRV after 1.96 lands")] },
      ],
    },
    "sysinfo": {
      text: "System vitals — load & battery status segments plus a <code>sysinfo</code> dashboard.",
      demo: (w) => [
        { do: "cmd", text: "sysinfo" },
        { do: "out", spans: [FG("cpu 12% · mem 9.2/32 GB · disk 412/994 GB")] },
        { do: "out", spans: [FG("load 2.1 1.8 1.6 · battery 84% ⚡")] },
        { do: "call", fn: async (t) => t.note("⚙ 2.1 · 🔋 84% in the status bar") },
      ],
    },
    "colored-man": {
      text: "Themed, colorized man pages — truecolor from your active theme.",
      demo: (w) => [
        { do: "cmd", text: "man tar" },
        { do: "out", spans: [ACC("TAR(1)"), MUT("                 General Commands")] },
        { do: "out", spans: [ACC2("  -c"), FG("  create a new archive")] },
        { do: "out", spans: [ACC2("  -x"), FG("  extract files from an archive")] },
      ],
    },
    "common": {
      text: "Universal quality-of-life aliases + zsh pipe shortcuts.",
      demo: (w) => [
        { do: "cmd", text: "ll" },
        { do: "out", spans: [FG("drwxr-xr-x  src   drwxr-xr-x  docs   -rw-r--r--  Cargo.toml")] },
        { do: "cmd", text: "dmesg G usb" },
        { do: "out", spans: [DIM("G = | grep — global pipe shortcuts")] },
      ],
    },

    /* ---- AI & safety ---- */
    "ai-terminal": {
      text: "The whole @-command family — @ai · @agent · @flow · @loop · @job · @profile · @theme.",
      demo: (w) => [
        { do: "cmd", text: "@ai how big is this repo?" },
        { do: "spin", label: "thinking…", ms: 900 },
        { do: "out", spans: [ACC("❯ "), DIM("press Enter to run (or edit)")] },
        { do: "out", spans: [ACC("❯ "), ACC2("du -sh . && git ls-files | wc -l"), S("t-cursor", "")] },
      ],
    },
    "command-guard": {
      text: "The default command-safety rules — catastrophic commands denied, risky ones confirmed.",
      demo: (w) => [
        { do: "cmd", text: "@ai wipe the whole disk" },
        { do: "spin", label: "thinking…", ms: 700 },
        { do: "out", spans: [ERR("# blocked by guard: matches a deny rule  /rm -rf \\//")] },
        { do: "cmd", text: "@ai force push my branch" },
        { do: "spin", label: "thinking…", ms: 700 },
        { do: "out", spans: [WARN("⚠ "), DIM("review before running — /git push --force/")] },
      ],
    },
    "redactor": {
      text: "Secret redaction before AI egress — AWS/OpenAI/GitHub keys, JWTs, PEM blocks.",
      demo: (w) => [
        { do: "cmd", text: "export AWS_KEY=AKIA1234567890ABCDEF" },
        { do: "cmd", text: "@ai why did the deploy fail?" },
        { do: "out", spans: [DIM("  context sent: export AWS_KEY="), WARN("«redacted»")] },
        { do: "out", spans: [DIM("  the model never sees your secrets")] },
      ],
    },
  };

  /* ---------------- wiring ---------------- */
  const ORDER = [
    "syntax-highlight", "autosuggest", "history", "completion", "lineedit", "alias-hints",
    "prompt", "sudo", "dir", "jump", "term-cwd",
    "git", "github", "docker", "kubernetes", "rust", "python", "node",
    "extract", "clipboard", "encode", "web-search", "weather", "world-clock",
    "notes", "sysinfo", "colored-man", "common",
    "ai-terminal", "command-guard", "redactor",
  ];

  /* accordion: exactly one category open */
  function expand(pacc) {
    if (!pacc || pacc.classList.contains("open")) return;
    document.querySelectorAll(".pacc").forEach((p) => p.classList.toggle("open", p === pacc));
  }
  document.querySelectorAll(".pacc-head").forEach((head) =>
    head.addEventListener("click", () => expand(head.closest(".pacc"))));

  async function select(id, replay = false, scroll = true) {
    const p = PLUGINS[id];
    if (!p || (!replay && current === id)) return;
    current = id;
    const row = document.querySelector(`[data-plugin="${id}"]`);
    expand(row?.closest(".pacc"));
    document.querySelectorAll("[data-plugin]").forEach((r) =>
      r.classList.toggle("active", r.dataset.plugin === id));
    /* only a USER click scrolls the row into view — the boot-time selection
       must never yank a freshly opened page down to this section */
    if (scroll) row?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    const w = fresh(p.opts);
    caption(id, p.text);
    const myEpoch = epoch;
    await run(w, myEpoch, [{ do: "pause", ms: 300 }, ...p.demo(w)]);
    if (myEpoch === epoch) {
      w.line([DIM("· done — pick another plugin")]);
    }
  }

  document.querySelectorAll("[data-plugin]").forEach((row) =>
    row.addEventListener("click", () => select(row.dataset.plugin)));

  select("syntax-highlight", false, false); // boot: no scroll — the page stays at the top
});
