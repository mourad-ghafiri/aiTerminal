/* ============================================================================
   board.js — the flow board, laid out the way the terminal lays it out.

   This is a port, not an impression. Every constant and every pass below comes
   from the shipping code:

     crates/framework/src/flow/board/card.rs   the geometry (ranks → columns)
     crates/framework/src/flow/board/graph.rs  what goes in a card, and the edges
     crates/framework/src/flow/board/view.rs   the pane and the tally
     crates/corelib/src/graph.rs               the ranking
     crates/corelib/src/cells.rs               the junction glyphs

   Boards used to be typed into the demo scripts as forty lines of ASCII each.
   That is why they looked approximate — a hand-drawn card is 24 columns because
   somebody counted 24, while the real one is whatever `(cols + GAP) / ranks`
   comes to, clamped. It is also why they drifted: every change to the board left
   the website describing the previous one.

   Generated instead, three things follow. It is exact at any width, because the
   width is measured off the replica's own character cell. It can ANIMATE — walk
   a run node by node and redraw, which is what the terminal does and what a
   still picture can never show. And when the board changes again, one file moves.
   ========================================================================== */

/* ---------------- the geometry, from card.rs ---------------- */
const CARD_H = 5;   // border, title, what, detail, border
const GAP    = 4;   // columns between two ranks — room for `───▸`
const VGAP   = 1;   // blank rows between two cards stacked in one column
const MIN_W  = 18;  // narrower than this cannot hold an id and a glyph
const MAX_W  = 34;
const TRACE_KEEP = 3;
const PANE_H = 2 + TRACE_KEEP;

/* ---------------- the junction table, from cells.rs ---------------- */
const UP = 1, RIGHT = 2, DOWN = 4, LEFT = 8;
const DASH_H = "╌", DASH_V = "╎";

function glyphOf(mask) {
  if (mask === 0) return " ";
  if ((mask & (LEFT | RIGHT)) === 0) return "│";
  if ((mask & (UP | DOWN)) === 0) return "─";
  const u = !!(mask & UP), r = !!(mask & RIGHT), d = !!(mask & DOWN), l = !!(mask & LEFT);
  if (u && r && !d && !l) return "└";
  if (u && !r && !d && l) return "┘";
  if (!u && r && d && !l) return "┌";
  if (!u && !r && d && l) return "┐";
  if (u && r && d && !l) return "├";
  if (u && !r && d && l) return "┤";
  if (!u && r && d && l) return "┬";
  if (u && r && !d && l) return "┴";
  return "┼";
}

/* A grid of cells, each a line junction (from a direction mask) or a literal
   character written on top. Characters win — that is what lets an arrowhead sit
   on the end of a line without the mask turning it back into a corner. */
class Canvas {
  constructor(w, h) {
    this.w = w; this.h = h;
    this.mask = new Uint8Array(w * h);
    this.over = new Array(w * h).fill("\0");
  }
  idx(x, y) { return (x < 0 || y < 0 || x >= this.w || y >= this.h) ? -1 : y * this.w + x; }
  add(x, y, bits) {
    const i = this.idx(x, y);
    if (i < 0) return;
    this.mask[i] |= bits;
    /* A solid line crossing a dashed one wins the cell, so it reads as
       continuous rather than pockmarked. This is why a `goto` travelling under
       the board does not pepper the arrows it passes beneath. */
    if (this.over[i] === DASH_H || this.over[i] === DASH_V) this.over[i] = "\0";
  }
  put(x, y, ch) { const i = this.idx(x, y); if (i >= 0 && ch !== "\0") this.over[i] = ch; }
  /* A cell nothing has been drawn on yet — what a dashed run asks before it
     commits, so a solid line already there keeps the cell and reads continuous. */
  free(x, y) { const i = this.idx(x, y); return i >= 0 && this.over[i] === "\0" && this.mask[i] === 0; }
  hline(x0, x1, y) {
    const a = Math.min(x0, x1), b = Math.max(x0, x1);
    for (let x = a; x <= b; x++) {
      let bits = 0;
      if (x > a) bits |= LEFT;
      if (x < b) bits |= RIGHT;
      this.add(x, y, a === b ? (LEFT | RIGHT) : bits);
    }
  }
  vline(y0, y1, x) {
    const a = Math.min(y0, y1), b = Math.max(y0, y1);
    for (let y = a; y <= b; y++) {
      let bits = 0;
      if (y > a) bits |= UP;
      if (y < b) bits |= DOWN;
      this.add(x, y, a === b ? (UP | DOWN) : bits);
    }
  }
  dashedH(x0, x1, y) {
    for (let x = Math.min(x0, x1); x <= Math.max(x0, x1); x++) if (this.free(x, y)) this.put(x, y, DASH_H);
  }
  at(x, y) {
    const i = this.idx(x, y);
    if (i < 0) return " ";
    return this.over[i] !== "\0" ? this.over[i] : glyphOf(this.mask[i]);
  }
}

/* ---------------- states, from board/mod.rs ---------------- */
const SPIN = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const STATES = {
  waiting: { glyph: () => "○",                       word: "waiting",         ink: "d"  },
  running: { glyph: (f) => SPIN[f % SPIN.length],    word: "running",         ink: "a"  },
  done:    { glyph: () => "✓",                       word: "done",            ink: "g"  },
  failed:  { glyph: () => "✗",                       word: "failed",          ink: "e"  },
  skipped: { glyph: () => "·",                       word: "skipped",         ink: "d"  },
  blocked: { glyph: () => "⊘",                       word: "blocked",         ink: "w"  },
  parked:  { glyph: () => "⏸",                       word: "waiting for you", ink: "w"  },
};
const wentWrong = (s) => s === "failed" || s === "blocked";

/* ---------------- the ranking, from corelib::graph ---------------- */

/* Every "runs after" edge in index space: `needs` points from a dependency to
   its dependent, and a `goto` points the other way — the node holding it sends
   the run BACK. */
function wires(nodes) {
  const at = (id) => nodes.findIndex((n) => n.id === id);
  const out = [];
  nodes.forEach((n, i) => {
    (n.needs || []).forEach((dep) => { const j = at(dep); if (j >= 0) out.push([j, i]); });
    if (n.goto) { const j = at(n.goto); if (j >= 0) out.push([i, j]); }
  });
  return out;
}

/* The edges that point backwards — found by a depth-first walk, so a cycle does
   not make the ranking diverge. */
function backEdges(n, edges) {
  const seen = new Array(n).fill(0);  // 0 unvisited · 1 on the stack · 2 done
  const back = new Set();
  const walk = (u) => {
    seen[u] = 1;
    edges.forEach(([a, b], i) => {
      if (a !== u) return;
      if (seen[b] === 1) back.add(i);
      else if (seen[b] === 0) walk(b);
    });
    seen[u] = 2;
  };
  for (let i = 0; i < n; i++) if (seen[i] === 0) walk(i);
  return back;
}

/* Longest-path layering, relaxed to a fixed point and bounded by n. */
function ranks(n, edges) {
  const back = backEdges(n, edges);
  const rank = new Array(n).fill(0);
  for (let pass = 0; pass < Math.max(n, 1); pass++) {
    let changed = false;
    edges.forEach(([from, to], i) => {
      if (back.has(i) || from === to) return;
      if (rank[to] < rank[from] + 1) { rank[to] = rank[from] + 1; changed = true; }
    });
    if (!changed) break;
  }
  return rank;
}

/* The nodes of each rank, top to bottom. Declaration order: the product runs a
   barycentre sweep to cut crossings, and with two or three nodes in a rank the
   two agree — which is every graph a showcase draws. */
function columns(nodes) {
  const rank = ranks(nodes.length, wires(nodes));
  const cols = [];
  rank.forEach((r, i) => { (cols[r] ||= []).push(i); });
  for (let i = 0; i < cols.length; i++) cols[i] ||= [];
  return cols;
}

/* Which edges the graph already implies: `a → c` alongside `a → b → c` says
   nothing the picture does not, and drawing it is the biggest source of clutter
   on a real flow. The dependency is untouched — only the arrow is dropped. */
function implied(n, edges) {
  const back = backEdges(n, edges);
  /* The ACYCLIC adjacency. A `goto` points backwards, and following it would
     make every node reach every other — which would drop the very edges the
     picture is made of. */
  const adj = Array.from({ length: n }, () => []);
  edges.forEach(([a, b], i) => { if (!back.has(i) && a !== b) adj[a].push(b); });
  return edges.map(([from, to], i) => {
    if (back.has(i) || from === to) return false;
    /* Is `to` reachable from `from` WITHOUT taking this edge? */
    const stack = adj[from].filter((c) => c !== to);
    const seen = new Set();
    while (stack.length) {
      const node = stack.pop();
      if (node === to) return true;
      if (seen.has(node)) continue;
      seen.add(node);
      adj[node].forEach((c) => stack.push(c));
    }
    return false;
  });
}

/* ---------------- layout ---------------- */
function plan(nodes, cols) {
  if (!nodes.length) return { cards: [], edges: [], w: 0, h: 0 };
  const colsOf = columns(nodes);
  const ranksN = Math.max(colsOf.length, 1);
  const room = Math.max(cols - 2, 0);
  /* Every rank the same width, so a card's left edge is a fact about its DEPTH
     rather than about the text in the card before it. */
  const want = Math.floor((room + GAP) / ranksN);
  const cardW = Math.min(Math.max(want - GAP, MIN_W), MAX_W);

  const cards = [];
  colsOf.forEach((ids, rank) => ids.forEach((node, slot) => {
    cards.push({ node, rank, slot, x: 2 + rank * (cardW + GAP), y: slot * (CARD_H + VGAP), w: cardW });
  }));
  const right = (c) => c.x + c.w - 1;
  const bottom = (c) => c.y + CARD_H - 1;
  const cy = (c) => c.y + Math.floor(CARD_H / 2);
  const tallest = Math.max(...colsOf.map((c) => c.length), 1);

  const grid = {
    cards, right, bottom, cy,
    card: (node) => cards.find((c) => c.node === node),
    w: Math.max(...cards.map((c) => right(c) + 1), 0),
    h: tallest * (CARD_H + VGAP),
  };

  const ws = wires(nodes);
  const imp = implied(nodes.length, ws);
  const lanes = new Map();
  let backs = 0;
  grid.edges = [];
  ws.forEach(([from, to], i) => {
    if (imp[i]) return;
    const a = grid.card(from), b = grid.card(to);
    if (!a || !b) return;
    if (b.rank <= a.rank) grid.edges.push({ from, to, link: "back", lane: backs++ });
    else if (b.rank === a.rank + 1 && b.slot === a.slot) grid.edges.push({ from, to, link: "straight", lane: 0 });
    else {
      const lane = lanes.get(b.rank) || 0;
      lanes.set(b.rank, lane + 1);
      grid.edges.push({ from, to, link: "elbow", lane });
    }
  });
  /* A band under the board carries every backward edge, one lane each. A board
     with no loops pays for no band at all. */
  grid.h += grid.edges.filter((e) => e.link === "back").length;
  return grid;
}

/* ---------------- what goes in a card, from graph.rs ---------------- */
const clip = (s, max) => {
  const one = String(s).split(/\s+/).filter(Boolean).join(" ");
  return [...one].length <= max ? one : [...one].slice(0, Math.max(max - 1, 0)).join("") + "…";
};
const humanTokens = (n) => (n >= 1000 ? (n / 1000).toFixed(1) + "k" : String(n));

function counters(row) {
  const parts = [];
  if (row.calls > 0) parts.push("⚙" + row.calls);
  if (row.attempts > 1) parts.push("×" + row.attempts);
  return parts.join(" ");
}

function subtitle(row, inner) {
  if (!row.model || [...row.what].length + [...row.model].length + 3 > inner) return row.what;
  return row.what + " · " + row.model;
}

/* The third line: what it is doing, why it stopped, or what it cost. A live note
   wins while it works; a settled FAILURE shows its reason, because what a
   failure cost is the least interesting thing about it. */
function detail(row) {
  if (row.state === "running" && row.note) return row.note;
  if (wentWrong(row.state) && noteOf(row)) return noteOf(row);
  const spent = [];
  if (row.ms >= 100) spent.push((row.ms / 1000).toFixed(1) + "s");
  if (row.tokens > 0) spent.push(humanTokens(row.tokens));
  if (spent.length) return spent.join(" · ");
  const waiting = [];
  if (noteOf(row)) waiting.push(noteOf(row));
  if (row.state === "waiting" && row.goto) waiting.push("↺≤" + row.max);
  return waiting.join(" · ");
}

const noteOf = (row) =>
  (row.state === "waiting" && row.when) ? "when " + row.when : (row.note || "");

/* ---------------- drawing ---------------- */

/* One board, as an array of `{ text, cls }` span rows the replica can print.
   `cls` is the site's own code palette — g success · e error · w warn · a accent
   · d muted — which is how the terminal's semantic tokens reach the page. */
function drawBoard(model, cols, frame = 0) {
  const nodes = model.nodes;
  const rows = nodes.map((n) => ({
    id: n.id, what: n.what || "", model: n.model || "", when: n.when || "",
    needs: n.needs || [], goto: n.goto || null, max: n.max || 0,
    state: (model.state && model.state[n.id]) || "waiting",
    note: (model.note && model.note[n.id]) || "",
    calls: (model.calls && model.calls[n.id]) || 0,
    attempts: (model.attempts && model.attempts[n.id]) || 0,
    ms: (model.ms && model.ms[n.id]) || 0,
    tokens: (model.tokens && model.tokens[n.id]) || 0,
    trace: (model.trace && model.trace[n.id]) || [],
  }));
  const grid = plan(nodes, cols);
  /* Cards cost height AND width. When they will not fit the window they are
     painting into, the denser view is not a downgrade — it is the only one that
     can be read, which is exactly what the terminal does here. */
  if (!fits(nodes, cols, model.rowsAvailable || 0)) return drawList(rows, cols, model, frame);
  const out = [];

  /* the shape line — the run's whole capability surface, in one glance */
  out.push([{ text: "  " + clip(shapeLine(rows, grid, model), Math.max(cols - 2, 0)), cls: "d" }]);

  const canvas = new Canvas(Math.max(grid.w, 1), Math.max(grid.h, 1));
  const ink = new Array(Math.max(grid.w, 1) * Math.max(grid.h, 1)).fill(null);
  const paint = (x, y, cls) => { if (x >= 0 && y >= 0 && x < canvas.w && y < canvas.h) ink[y * canvas.w + x] = cls; };
  const span = (x0, x1, y, cls) => { for (let x = x0; x <= Math.min(x1, canvas.w - 1); x++) paint(x, y, cls); };

  /* Edges first, so a card wins every cell it shares with one: a line should
     stop at the box it points at, never run through its text. */
  grid.edges.forEach((e) => drawEdge(canvas, paint, span, grid, rows, e));
  grid.cards.forEach((c) => drawCard(canvas, paint, span, c, rows[c.node], frame));

  for (let y = 0; y < canvas.h; y++) {
    const runs = [];
    for (let x = 0; x < canvas.w; x++) {
      const cls = ink[y * canvas.w + x];
      const ch = canvas.at(x, y);
      const last = runs[runs.length - 1];
      if (last && last.cls === cls) last.text += ch; else runs.push({ text: ch, cls });
    }
    while (runs.length && !runs[runs.length - 1].text.trim()) runs.pop();
    if (runs.length) runs[runs.length - 1].text = runs[runs.length - 1].text.replace(/\s+$/, "");
    out.push(runs.length ? runs : [{ text: "", cls: null }]);
  }

  paneRows(rows, cols).forEach((r) => out.push(r));
  out.push(tally(rows, cols, model));
  return out;
}

/* ---------------- the dense view, from list.rs ----------------
   One row per node: glyph, id, agent, time, tokens, attempts, and whatever note
   the remaining width can hold. No picture, so no width it can overflow. */
function drawList(rows, cols, model, frame) {
  const width = rows.reduce((w, r) => Math.max(w, [...r.id].length), 0);
  const out = [];
  rows.forEach((row) => {
    const st = STATES[row.state] || STATES.waiting;
    const time = row.ms >= 100 ? (row.ms / 1000).toFixed(1).padStart(5) + "s" : "       ";
    const tokens = row.tokens > 0 ? humanTokens(row.tokens).padStart(6) : "      ";
    const attempts = row.attempts > 1 ? " ×" + row.attempts : "";
    const head = `  ${st.glyph(frame)} ${cell(row.id, width)}  ${cell(row.what, 14)}${time}${tokens}${attempts}`;
    const room = Math.max(cols - head.length - 2, 0);
    const note = noteOf(row);
    const line = [
      { text: "  " + st.glyph(frame), cls: st.ink },
      { text: " " + cell(row.id, width) + "  ", cls: null },
      { text: cell(row.what, 14) + time + tokens, cls: "d" },
    ];
    if (attempts) line.push({ text: attempts, cls: "d" });
    if (note && room >= 8) line.push({ text: "  " + clip(note, Math.min(room, 44)), cls: "d" });
    out.push(line);
  });
  out.push(tally(rows, cols, model));
  return out;
}

const cell = (s, n) => { const t = clip(s, n); return t + " ".repeat(Math.max(n - [...t].length, 0)); };

function drawCard(canvas, paint, span, c, row, frame) {
  const x0 = c.x, y0 = c.y, x1 = c.x + c.w - 1, y1 = c.y + CARD_H - 1;
  canvas.hline(x0, x1, y0);
  canvas.hline(x0, x1, y1);
  canvas.vline(y0, y1, x0);
  canvas.vline(y0, y1, x1);
  /* The same rounded corners the diagram renderer draws a `Round` node with, so
     a card and a diagram box are one shape rather than two opinions about one. */
  [[x0, y0, "╭"], [x1, y0, "╮"], [x0, y1, "╰"], [x1, y1, "╯"]].forEach(([x, y, ch]) => canvas.put(x, y, ch));

  const st = STATES[row.state] || STATES.waiting;
  span(x0, x1, y0, st.ink);
  span(x0, x1, y1, st.ink);
  for (let y = y0; y <= y1; y++) { paint(x0, y, st.ink); paint(x1, y, st.ink); }

  const inner = Math.max(c.w - 4, 0);
  const at = c.x + 2;
  const line = (dy, text, cls) => {
    const t = clip(text, inner);
    [...t].forEach((ch, i) => canvas.put(at + i, c.y + dy, ch));
    span(at, at + Math.max([...t].length - 1, 0), c.y + dy, cls);
  };
  const title = st.glyph(frame) + " " + row.id;
  line(1, title, st.ink);
  /* The counters ride at the right end of the title rather than competing with
     the cost for the last line: they are the two numbers that keep climbing. */
  const counts = counters(row);
  if (counts && [...title].length + [...counts].length + 1 <= inner) {
    const x = c.x + c.w - 2 - [...counts].length;
    [...counts].forEach((ch, i) => canvas.put(x + i, c.y + 1, ch));
    span(x, x + [...counts].length - 1, c.y + 1, "d");
  }
  line(2, subtitle(row, inner), "d");
  line(3, detail(row), "d");
}

/* An edge takes the colour of the node it LEAVES, once that node has settled —
   so the path that actually ran lights up behind the board as it advances. */
function edgeInk(state) {
  return (state === "waiting" || state === "running") ? "d" : (STATES[state] || STATES.waiting).ink;
}

function drawEdge(canvas, paint, span, grid, rows, e) {
  const a = grid.card(e.from), b = grid.card(e.to);
  if (!a || !b) return;
  const cls = edgeInk(rows[e.from].state);
  const arrive = (x, y) => { canvas.put(x, y, "▸"); paint(x, y, cls); };
  const R = grid.right, CY = grid.cy;

  if (e.link === "straight") {
    const y = CY(a), x0 = R(a) + 1, x1 = Math.max(b.x - 1, 0);
    canvas.hline(x0, Math.max(x1 - 1, x0), y);
    span(x0, x1, y, cls);
    arrive(x1, y);
    return;
  }
  if (e.link === "elbow") {
    /* Out of the right port, along to the gap before the target, up or down it,
       then into the left port. Right angles only: an edge is read by following
       it rather than by guessing which dash belongs to which. */
    const turn = Math.max(b.x - 1 - (e.lane % Math.max(GAP, 1)), 0);
    const y0 = CY(a), y1 = CY(b);
    canvas.hline(R(a) + 1, turn, y0);
    span(R(a) + 1, turn, y0, cls);
    canvas.vline(y0, y1, turn);
    for (let y = Math.min(y0, y1); y <= Math.max(y0, y1); y++) paint(turn, y, cls);
    canvas.hline(turn, Math.max(b.x - 1, 0), y1);
    span(turn, Math.max(b.x - 1, 0), y1, cls);
    arrive(Math.max(b.x - 1, 0), y1);
    return;
  }
  /* A `goto` pointing back at a rank already passed. It travels in the band
     UNDER the whole board, and both of its verticals run in a GAP between two
     columns — never at a card's centre. */
  const lane = Math.max(grid.h - 1 - e.lane, 0);
  const last = Math.max(...grid.cards.map((c) => c.rank), 0);
  const after = a.rank < last;
  const down = after ? R(a) + 2 + e.lane : Math.max(a.x - 2 - e.lane, 0);
  const up = Math.max(b.x - 2 - e.lane, 0);
  const [px, qx] = after ? [R(a) + 1, down] : [down, Math.max(a.x - 1, 0)];
  canvas.dashedH(px, qx, CY(a));
  span(px, qx, CY(a), cls);
  for (let y = CY(a); y <= lane; y++) { canvas.put(down, y, DASH_V); paint(down, y, cls); }
  canvas.dashedH(Math.min(up, down), Math.max(up, down), lane);
  span(Math.min(up, down), Math.max(up, down), lane, cls);
  for (let y = CY(b); y <= lane; y++) { canvas.put(up, y, DASH_V); paint(up, y, cls); }
  canvas.hline(up, Math.max(b.x - 1, 0), CY(b));
  span(up, Math.max(b.x - 1, 0), CY(b), cls);
  arrive(Math.max(b.x - 1, 0), CY(b));
}

/* "7 nodes · 3 agents · 14 tools · 4 skills · 4 at a time" — what an agent can
   reach is a fact you read rather than a thing you assume. */
function shapeLine(rows, grid, model) {
  const agents = [];
  rows.filter((r) => r.what.startsWith("@")).forEach((r) => {
    if (!agents.some((a) => a.what === r.what)) agents.push(r);
  });
  const plural = (n, w) => `${n} ${w}${n === 1 ? "" : "s"}`;
  const parts = [plural(rows.length, "node")];
  if (agents.length) parts.push(plural(agents.length, "agent"));
  if (model.tools) parts.push(plural(model.tools, "tool"));
  if (model.skills) parts.push(plural(model.skills, "skill"));
  parts.push(`${model.concurrency || 4} at a time`);
  if (model.slowest) parts.push("slowest path " + model.slowest.join("→"));
  return parts.join(" · ");
}

/* The pane: the one node worth looking at, and everything a card has no room
   for. Running first, then a FAILURE — everything downstream of a failure
   settles after it, so "most recently touched" would look away from the break. */
function focus(rows) {
  return rows.find((r) => r.state === "running")
      || rows.find((r) => r.state === "failed")
      || rows.find((r) => r.state === "parked")
      || [...rows].reverse().find((r) => r.state !== "waiting")
      || rows[0];
}

function paneRows(rows, cols) {
  const room = Math.max(cols - 4, 0);
  const row = focus(rows);
  const out = [];
  if (!row) return Array.from({ length: PANE_H }, () => [{ text: "", cls: null }]);
  const st = STATES[row.state] || STATES.waiting;
  const title = [row.id, row.what, row.model].filter(Boolean).join(" · ");
  out.push([{ text: "  " + st.glyph(0), cls: st.ink }, { text: " " + clip(title, room), cls: "a" }]);

  const facts = [st.word];
  if (row.attempts > 1) facts.push("attempt " + row.attempts);
  if (row.ms >= 100) facts.push((row.ms / 1000).toFixed(1) + "s");
  if (row.tokens > 0) facts.push(humanTokens(row.tokens) + " tokens");
  if (row.calls > 0) facts.push(row.calls + " tool call" + (row.calls === 1 ? "" : "s"));
  if (row.needs.length) facts.push("needs " + row.needs.join(", "));
  if (noteOf(row) && !row.trace.length) facts.push(noteOf(row));
  out.push([{ text: "    " + clip(facts.join(" · "), room), cls: "d" }]);

  /* Oldest first, bottom-aligned: the newest call is always on the same line. */
  for (let i = 0; i < TRACE_KEEP - Math.min(row.trace.length, TRACE_KEEP); i++) out.push([{ text: "", cls: null }]);
  row.trace.slice(0, TRACE_KEEP).forEach((t) => out.push([{ text: "    " + clip(t, room), cls: "d" }]));
  return out.slice(0, PANE_H);
}

function tally(rows, cols, model) {
  const count = (want) => rows.filter((r) => r.state === want).length;
  const parts = [{ text: `${count("done")}/${rows.length} done`, cls: "d" }];
  [["running", "d"], ["failed", "e"], ["blocked", "w"], ["skipped", "d"], ["parked", "w"]].forEach(([s, cls]) => {
    const n = count(s);
    if (n) parts.push({ text: " · ", cls: "d" }, { text: `${n} ${STATES[s].word}`, cls });
  });
  const tokens = Object.values(model.tokens || {}).reduce((a, b) => a + b, 0);
  if (tokens > 0) parts.push({ text: " · " + humanTokens(tokens) + " tokens", cls: "d" });
  if (model.elapsed) parts.push({ text: " · " + model.elapsed, cls: "d" });
  return [{ text: "  ", cls: null }, ...parts];
}

/* Whether the cards fit the window they are painting into. Both dimensions:
   depth costs width, so a nine-deep flow asks for more columns than a terminal
   has — and a picture drawn past the right-hand edge is worse than no picture,
   which is why the product falls back to its dense list view exactly here. */
function fits(nodes, cols, rowsAvailable) {
  const grid = plan(nodes, cols);
  const budget = rowsAvailable > 0 ? Math.max(rowsAvailable - (3 + PANE_H), 0) : 40;
  return grid.cards.length > 1 && grid.h <= budget && grid.w <= cols;
}
