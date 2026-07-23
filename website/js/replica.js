/* ============================================================================
   replica.js — the FULL aiTerminal window replica: titlebar, a dockable tab
   bar (top / bottom / left / right, like `[behavior] tab_bar`), split panes
   with accent focus frames, the plugin status bar, and the ⌘P switcher
   overlay. Fixed size — lines flow naturally top → down, and once the pane is
   full the oldest lines scroll off the top; the window never grows. Colors
   ride CSS custom properties
   set from the real theme palettes. Pure JS, no dependencies.
   ========================================================================== */

const REDUCED = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const SPINNER_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};
const sleep = (ms) => new Promise((r) => setTimeout(r, REDUCED ? Math.min(ms, 40) : ms));

/* span helpers for demo scripts */
const S = (cls, text) => ({ cls, text });
const FG = (t) => S("t-fg", t);
const DIM = (t) => S("t-dim", t);
const MUT = (t) => S("t-muted", t);
const ACC = (t) => S("t-accent", t);
const ACC2 = (t) => S("t-accent2", t);
const OK = (t) => S("t-success", t);
const WARN = (t) => S("t-warn", t);
const ERR = (t) => S("t-error", t);
/* the real default prompt (builtin/plugins/prompt/shell.zsh):
   %~ in accent · " ⎇ branch" in success via vcs_info · "❯ " in accent2 */
const PROMPT = (cwd = "~/project", branch = "main") =>
  branch ? [ACC(cwd), OK(" ⎇ " + branch), ACC2(" ❯ ")] : [ACC(cwd), ACC2(" ❯ ")];

/* live sysinfo for the status bar — CPU / MEM wander a little like real load,
   the clock is the actual time (the app's {time.hm} = HH:MM, 24h). */
const SYS = { cpu: 4, mem: 6.3, windows: new Set() };
const fmtCpu = () => "CPU " + Math.round(SYS.cpu) + "%";
const fmtMem = () => "MEM " + SYS.mem.toFixed(1) + "G";
const fmtClock = () => {
  const d = new Date();
  return String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
};
setInterval(() => {
  SYS.cpu = Math.max(1, Math.min(27, SYS.cpu + (Math.random() * 6 - 3)));
  SYS.mem = Math.max(5.6, Math.min(9.8, SYS.mem + (Math.random() * 0.4 - 0.2)));
  SYS.windows.forEach((w) => w._statusOver && w.defaultStatus(w._statusOver));
}, 2400);

/* the syntax-highlight plugin's rule: @word accent-bold, the rest accent2 */
function cmdSpans(text) {
  const m = text.match(/^(@[\w-]*)(.*)$/s);
  if (m) return [S("t-accent-b", m[1]), S("t-accent2", m[2])];
  return [S("t-fg", text)];
}

function spanEl(s, cls) {
  const e = el("span", cls !== undefined ? cls : s.cls);
  e.textContent = s.text;
  return e;
}

/* ---------------- the window ---------------- */
function makeWindow(root, opts = {}) {
  const o = Object.assign(
    /* the real window title stays the brand name unless a program sets one via OSC */
    { title: "aiTerminal", tabs: [{ title: "Terminal [project][zsh]", active: true }],
      theme: "midnight", tabpos: "top", cwd: "~/project", branch: "main" },
    opts
  );
  root.classList.add("rw");
  root.dataset.tabpos = o.tabpos;
  root.innerHTML = "";

  const titlebar = el("div", "rw-titlebar");
  const lights = el("div", "rw-lights");
  ["r", "y", "g"].forEach((c) => lights.appendChild(el("span", "rw-light " + c)));
  const title = el("div", "rw-title", o.title);
  titlebar.append(lights, title);

  const tabbar = el("div", "rw-tabbar");
  const main = el("div", "rw-main");
  const body = el("div", "rw-body");
  const pane0 = el("div", "rw-pane focused");
  body.appendChild(pane0);
  main.appendChild(body);

  const statusbar = el("div", "rw-statusbar");
  const segL = el("div", "rw-segs");
  const segR = el("div", "rw-segs");
  statusbar.append(segL, segR);

  const switcher = el("div", "rw-switcher");
  const swPanel = el("div", "rw-switcher-panel");
  switcher.appendChild(swPanel);
  const note = el("div", "rw-note");

  root.append(titlebar, tabbar, main, statusbar, switcher, note);

  const w = {
    root, body, panes: [pane0], focusIdx: 0,
    visible: !("IntersectionObserver" in window),

    setTheme(key) {
      const t = THEMES[key] || THEMES.midnight;
      const map = { bg: t.bg, surface: t.surface, fg: t.fg, muted: t.muted, accent: t.accent,
        accent2: t.accent2, success: t.success, warn: t.warn, error: t.error,
        cursor: t.cursor, selection: t.selection, border: t.border, hover: t.hover };
      for (const [k, v] of Object.entries(map)) root.style.setProperty("--t-" + k, v);
      w.theme = key;
    },

    /* the tab bar docks on any edge — the real `[behavior] tab_bar` */
    setTabpos(pos) {
      if (!["top", "bottom", "left", "right"].includes(pos)) return;
      root.dataset.tabpos = pos;
      if (pos === "top") root.insertBefore(tabbar, main);
      else if (pos === "bottom") root.insertBefore(tabbar, statusbar);
      else if (pos === "left") main.insertBefore(tabbar, body);
      else main.appendChild(tabbar);
      w.tabpos = pos;
    },

    setTitle(t) { title.textContent = t; },

    setTabs(tabs) {
      tabbar.innerHTML = "";
      /* the real app shows the tab strip only when there is more than one tab */
      tabbar.style.display = tabs.length > 1 ? "" : "none";
      tabs.forEach((t) => {
        const tab = el("span", "rw-tab" + (t.active ? " active" : ""));
        tab.textContent = `🖥 ${t.title}`;
        tabbar.appendChild(tab);
      });
    },

    setStatus(left, right) {
      w._statusOver = null; /* custom segments — stop the sysinfo ticker refresh */
      segL.innerHTML = ""; segR.innerHTML = "";
      (left || []).forEach((s) => segL.appendChild(spanEl(s, "rw-seg " + s.cls)));
      (right || []).forEach((s) => segR.appendChild(spanEl(s, "rw-seg " + s.cls)));
    },

    /* the real default segments, in the app's exact order: LEFT dir + git
       plugins; RIGHT the profile chip, then the prompt plugin (user@host,
       CPU, MEM, clock), then sysinfo's battery — all live. */
    defaultStatus(over = {}) {
      w.setStatus(
        [ACC("📁 " + (over.cwd || o.cwd)), OK("⎇ " + (over.branch || o.branch)), OK("●"), WARN("●")],
        [ACC(over.profile || "🚀 Default"), MUT("dev@mac"),
         MUT(fmtCpu()), MUT(fmtMem()), MUT(over.clock || fmtClock()), MUT("🔋 100%")]
      );
      w._statusOver = over; /* after setStatus — keep the live sysinfo refresh on */
    },

    pane(i) { return w.panes[i == null ? w.focusIdx : i]; },

    line(spans, paneIdx) {
      const l = el("div", "rw-line");
      (Array.isArray(spans) ? spans : [spans]).forEach((s) => l.appendChild(spanEl(s)));
      w.pane(paneIdx).appendChild(l);
      trim(w.pane(paneIdx));
      return l;
    },

    clear(paneIdx) { w.pane(paneIdx).innerHTML = ""; },

    split(vertical) {
      const p = el("div", "rw-pane enter");
      body.classList.add("split");
      body.classList.toggle("vsplit", !!vertical);
      body.appendChild(p);
      w.panes.push(p);
      w.focus(w.panes.length - 1);
      return w.panes.length - 1;
    },

    unsplit() {
      w.panes.slice(1).forEach((p) => p.remove());
      w.panes = [w.panes[0]];
      body.classList.remove("split", "vsplit");
      w.focus(0);
    },

    focus(i) {
      w.focusIdx = i;
      w.panes.forEach((p, k) => p.classList.toggle("focused", k === i));
    },

    openSwitcher(query, rows, activeIdx = 0) {
      swPanel.innerHTML = "";
      const search = el("div", "rw-switcher-search");
      search.appendChild(spanEl(ACC("❯ ")));
      search.appendChild(spanEl(query ? FG(query) : MUT("type a number or name…")));
      search.appendChild(el("span", "t-cursor"));
      swPanel.appendChild(search);
      rows.forEach((r, i) => {
        const row = el("div", "rw-switcher-row" + (i === activeIdx ? " active" : ""));
        row.appendChild(el("span", "", `${i + 1} - 🖥 ${r.title}`));
        row.appendChild(el("span", "detail", r.detail || ""));
        swPanel.appendChild(row);
      });
      switcher.classList.add("open");
    },
    closeSwitcher() { switcher.classList.remove("open"); },

    note(text) {
      note.textContent = text;
      note.classList.add("show");
      setTimeout(() => note.classList.remove("show"), 2600);
    },
  };

  w.setTheme(o.theme);
  w.setTabs(o.tabs);
  w.defaultStatus();
  w.setTabpos(o.tabpos);
  SYS.windows.add(w);

  if ("IntersectionObserver" in window) {
    new IntersectionObserver((es) => es.forEach((e) => (w.visible = e.isIntersecting)),
      { rootMargin: "160px" }).observe(root);
  }
  return w;
}

/* Natural terminal behavior: lines flow top → down; when the pane is full,
   the oldest lines scroll off the TOP (the fixed window never grows). */
function trim(pane) {
  while (pane.children.length > 1 && pane.scrollHeight > pane.clientHeight) {
    pane.removeChild(pane.firstChild);
  }
}

/* ---------------- primitives the scripts use ---------------- */
async function waitVisible(w) { while (!w.visible) await sleep(280); }

async function typeCmd(w, text, { prompt, paneIdx, speed = 30 } = {}) {
  await waitVisible(w);
  const l = el("div", "rw-line");
  (prompt || PROMPT()).forEach((s) => l.appendChild(spanEl(s)));
  const parts = cmdSpans(text);
  const spans = parts.map((p) => spanEl({ cls: p.cls, text: "" }));
  spans.forEach((s) => l.appendChild(s));
  const cursor = el("span", "t-cursor");
  l.appendChild(cursor);
  w.pane(paneIdx).appendChild(l);
  trim(w.pane(paneIdx));
  for (let pi = 0; pi < parts.length; pi++) {
    for (const ch of parts[pi].text) {
      spans[pi].textContent += ch;
      trim(w.pane(paneIdx));
      await sleep(speed + Math.random() * 22);
    }
  }
  await sleep(280);
  cursor.remove();
  return l;
}

async function spinner(w, label, ms, { paneIdx } = {}) {
  await waitVisible(w);
  const l = el("div", "rw-line");
  const f = el("span", "t-dim", SPINNER_FRAMES[0]);
  l.append(f, spanEl(DIM(" " + label)));
  w.pane(paneIdx).appendChild(l);
  trim(w.pane(paneIdx));
  let i = 0;
  const iv = setInterval(() => { f.textContent = SPINNER_FRAMES[(i = (i + 1) % 10)]; }, 80);
  await sleep(ms);
  clearInterval(iv);
  l.remove();
}

async function streamLine(w, spansDef, { paneIdx, speed = 11, prefix = [] } = {}) {
  await waitVisible(w);
  const l = el("div", "rw-line");
  prefix.forEach((s) => l.appendChild(spanEl(s)));
  w.pane(paneIdx).appendChild(l);
  trim(w.pane(paneIdx));
  for (const def of Array.isArray(spansDef) ? spansDef : [spansDef]) {
    const s = spanEl({ cls: def.cls, text: "" });
    l.appendChild(s);
    for (const ch of def.text) {
      s.textContent += ch;
      trim(w.pane(paneIdx));
      await sleep(speed);
    }
  }
  return l;
}
