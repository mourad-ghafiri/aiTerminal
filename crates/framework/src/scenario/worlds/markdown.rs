//! `@md` — rendering Markdown, and moving around a long document.
//!
//! Three layers, all pure: the renderer (`corelib::md`), the row model both the editor
//! pane and the pager lay out from (`mdedit::build_preview`), and the pager's scroll
//! state machine — driven through **real key bytes**, so a scenario proves the escape
//! decoding too rather than synthesising a `Key`.

use corelib::md::{self, Style};
use corelib::wire::Toml;

use super::super::world::{self, World};
use crate::mdedit::{build_preview, parse_key, PRow, Pager};

pub struct MarkdownWorld {
    /// The document under test.
    source: String,
    width: usize,
    style: Style,
    /// The rendered rows, once `render` or `preview` has run.
    rows: Vec<String>,
    preview: Vec<PRow>,
    pager: Pager,
    /// Streaming chunks, for the live-answer path.
    stream: Vec<String>,
    diagrams: Vec<String>,
}

pub fn build(setup: &Toml) -> Result<Box<dyn World>, String> {
    let width = world::int(setup, "width").unwrap_or(60).clamp(4, 400) as usize;
    // Plain by default: a scenario asserts text, not escape codes. `styled = true`
    // turns the real SGR back on for the colour assertions.
    let style = Style { enabled: world::flag(setup, "styled").unwrap_or(false), ..Style::default() };
    Ok(Box::new(MarkdownWorld {
        source: String::new(),
        width,
        style,
        rows: Vec::new(),
        preview: Vec::new(),
        pager: Pager::new("scenario.md"),
        stream: Vec::new(),
        diagrams: Vec::new(),
    }))
}

impl World for MarkdownWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // ── the document ─────────────────────────────────────────────────────
        if let Some(lines) = world::list(step, "markdown") {
            self.source = lines.join("\n");
            return self.render();
        }
        if let Some(w) = world::int(step, "width") {
            self.width = w.clamp(4, 400) as usize;
            return self.render();
        }
        if let Some(deltas) = world::list(step, "stream") {
            let mut sr = md::StreamRenderer::new(self.style.clone(), self.width, &["mermaid"]);
            self.stream.clear();
            self.diagrams.clear();
            for d in &deltas {
                self.absorb(sr.push(d));
            }
            self.absorb(sr.finish());
            return Ok(());
        }

        // ── moving around ────────────────────────────────────────────────────
        if let Some(keys) = world::list(step, "keys") {
            let body_h = world::int(step, "body_h").unwrap_or(10).max(1) as usize;
            let len = self.preview.len();
            for k in &keys {
                let bytes = world::unescape(k);
                let (key, _) = parse_key(bytes.as_bytes()).ok_or_else(|| format!("unreadable key {k:?}"))?;
                self.pager.on_key(key, body_h, len);
            }
            return Ok(());
        }

        // ── what must be true ────────────────────────────────────────────────
        if let Some(want) = world::list(step, "expect_lines") {
            return world::expect_lines(&self.rows, &want, "the rendered document");
        }
        if let Some(want) = world::list(step, "expect_contains") {
            return world::expect_contains(&self.rows.join("\n"), &want, "the rendered document");
        }
        if let Some(bad) = world::list(step, "expect_not_contains") {
            return world::expect_missing(&self.rows.join("\n"), &bad, "the rendered document");
        }
        if let Some(want) = world::int(step, "expect_width_at_most") {
            let widest = self.rows.iter().map(|l| corelib::unicode::str_width(l)).max().unwrap_or(0);
            if widest as i64 > want {
                let line = self.rows.iter().max_by_key(|l| corelib::unicode::str_width(l)).cloned().unwrap_or_default();
                return Err(format!("a line is {widest} columns wide, over the {want} limit: {}", world::show(&line)));
            }
            return Ok(());
        }
        if let Some(want) = world::list(step, "expect_preview") {
            let got: Vec<String> = self.preview.iter().map(row_label).collect();
            return world::expect_lines(&got, &want, "the preview rows");
        }
        if let Some(want) = world::int(step, "expect_preview_rows") {
            let got = self.preview.len() as i64;
            if got != want {
                return Err(format!("the preview is {got} row(s) tall — expected {want}"));
            }
            return Ok(());
        }
        if let Some(want) = world::int(step, "expect_top") {
            let got = self.pager.top as i64;
            if got != want {
                return Err(format!("the pager is at line {got} — expected {want}"));
            }
            return Ok(());
        }
        if let Some(want) = world::int(step, "expect_left") {
            let got = self.pager.left as i64;
            if got != want {
                return Err(format!("the pager is scrolled {got} column(s) right — expected {want}"));
            }
            return Ok(());
        }
        if let Some(want) = world::flag(step, "expect_quit") {
            if self.pager.quit != want {
                return Err(format!("the pager quit flag is {}, expected {want}", self.pager.quit));
            }
            return Ok(());
        }
        if let Some(want) = world::list(step, "expect_stream") {
            return world::expect_lines(&self.stream, &want, "the streamed chunks");
        }
        if let Some(want) = world::list(step, "expect_diagrams") {
            return world::expect_lines(&self.diagrams, &want, "the diagrams found while streaming");
        }
        if let Some(want) = world::int(step, "expect_diagram_rows") {
            let src = self.diagrams.first().cloned().unwrap_or_default();
            let got = crate::cli::diagram_rows(&src) as i64;
            if got != want {
                return Err(format!("the diagram reserves {got} row(s) — expected {want}"));
            }
            return Ok(());
        }
        if let Some(want) = world::list(step, "expect_mermaid_nodes") {
            return self.expect_mermaid_nodes(&want);
        }

        Err(world::unknown_verb(step))
    }
}

impl MarkdownWorld {
    fn render(&mut self) -> Result<(), String> {
        let out = md::render(&md::parse(&self.source), &self.style, self.width);
        self.rows = out.lines().map(str::to_string).collect();
        self.preview = build_preview(&self.source, self.width, self.style.clone());
        Ok(())
    }

    fn absorb(&mut self, chunks: Vec<md::Chunk>) {
        for c in chunks {
            match c {
                md::Chunk::Text(t) => self.stream.extend(t.lines().map(str::to_string)),
                md::Chunk::Diagram(src) => self.diagrams.push(src.trim().to_string()),
            }
        }
    }

    fn expect_mermaid_nodes(&self, want: &[String]) -> Result<(), String> {
        let src = self.diagrams.first().cloned().unwrap_or_else(|| self.source.clone());
        let Some(diagram) = corelib::mermaid::parse(&src) else {
            return Err(format!("this is not a diagram mermaid can read: {}", world::show(&src)));
        };
        // The product's own measure, so the geometry a scenario sees is the real one.
        let layout = corelib::mermaid::layout(&diagram, &|s| (corelib::unicode::str_width(s) as u32 * 8, 16));
        let got: Vec<String> = layout.nodes.iter().map(|n| n.label.clone()).collect();
        world::expect_lines(&got, want, "the diagram's nodes")
    }
}

/// A preview row as a scenario writes it: text verbatim, a diagram as `<diagram N/M>`.
fn row_label(r: &PRow) -> String {
    match r {
        PRow::Text(t) => t.clone(),
        PRow::Diagram { rows, offset, .. } => format!("<diagram {}/{rows}>", offset + 1),
    }
}
