//! The VT engine — what a program prints, and what you end up looking at.
//!
//! This is the foundation the whole product sits on: every pane, every `/shot`, every
//! bit of AI context is read out of this grid. A scenario feeds it the escape sequences
//! real programs emit and asserts what a person would see.

use corelib::wire::Toml;
use platform::term::{Cell, Color, Selection, SelectionMode, Term};

use super::super::world::{self, World};

pub struct TerminalWorld {
    term: Term,
    /// The most recent selection, for the copy assertions.
    selection: Option<Selection>,
}

pub fn build(setup: &Toml) -> Result<Box<dyn World>, String> {
    let cols = world::int(setup, "cols").unwrap_or(40).clamp(2, 400) as u16;
    let rows = world::int(setup, "rows").unwrap_or(8).clamp(1, 200) as u16;
    let scrollback = world::int(setup, "scrollback").unwrap_or(100).clamp(0, 10_000) as usize;
    Ok(Box::new(TerminalWorld { term: Term::with_scrollback(cols, rows, scrollback), selection: None }))
}

impl World for TerminalWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        // ── what a program does ──────────────────────────────────────────────
        if let Some(s) = world::text(step, "feed") {
            self.term.feed(s.as_bytes());
            return Ok(());
        }
        if let Some(lines) = world::list(step, "feed_lines") {
            for l in lines {
                self.term.feed(l.as_bytes());
                self.term.feed(b"\r\n");
            }
            return Ok(());
        }
        if let Some(c) = world::int(step, "resize_cols") {
            let rows = world::int(step, "rows").unwrap_or(self.term.rows() as i64);
            self.term.resize(c as u16, rows as u16);
            return Ok(());
        }
        if let Some(d) = world::int(step, "scroll_view") {
            self.term.scroll_view(d as i32);
            return Ok(());
        }
        if world::flag(step, "scroll_to_bottom") == Some(true) {
            self.term.scroll_to_bottom();
            return Ok(());
        }
        if world::flag(step, "scroll_to_top") == Some(true) {
            self.term.scroll_to_top();
            return Ok(());
        }

        // ── what a person does ───────────────────────────────────────────────
        if let Some(at) = world::list(step, "select") {
            return self.select(&at, step);
        }

        // ── what must be true ────────────────────────────────────────────────
        if let Some(want) = world::list(step, "expect_screen") {
            return world::expect_lines(&self.term.screen_text(), &want, "the screen");
        }
        if let Some(want) = world::text(step, "expect_line") {
            let row = world::int(step, "row").unwrap_or(0).max(0) as u16;
            return world::expect_eq(&self.row_text(row), &want, &format!("row {row}"));
        }
        if let Some(want) = world::list(step, "expect_contains") {
            return world::expect_contains(&self.term.screen_text().join("\n"), &want, "the screen");
        }
        if let Some(bad) = world::list(step, "expect_not_contains") {
            return world::expect_missing(&self.term.screen_text().join("\n"), &bad, "the screen");
        }
        if let Some(want) = world::text(step, "expect_cell") {
            return self.expect_cell(&want, step);
        }
        if let Some(want) = world::list(step, "expect_cursor") {
            let (cx, cy) = self.term.cursor();
            let got = format!("{cx},{cy}");
            let want = want.join(",");
            return world::expect_eq(&got, &want, "the cursor (col,row)");
        }
        if let Some(want) = world::flag(step, "expect_cursor_visible") {
            return yes_no(self.term.cursor_visible(), want, "the cursor is visible");
        }
        if let Some(want) = world::flag(step, "expect_alt_screen") {
            return yes_no(self.term.in_alt_screen(), want, "the alternate screen is up");
        }
        if let Some(want) = world::int(step, "expect_scrollback") {
            let got = self.term.scrollback_len() as i64;
            if got != want {
                return Err(format!("scrollback holds {got} line(s) — expected {want}"));
            }
            return Ok(());
        }
        if let Some(want) = world::list(step, "expect_view") {
            let rows: Vec<String> = (0..self.term.rows()).map(|y| self.display_text(y)).collect();
            return world::expect_lines(&trim_tail(rows), &want, "the visible view");
        }
        if let Some(want) = world::text(step, "expect_title") {
            return world::expect_eq(self.term.title(), &want, "the window title");
        }
        if let Some(want) = world::text(step, "expect_cwd") {
            let got = self.term.cwd().map(|(h, p)| format!("{h}:{p}")).unwrap_or_default();
            return world::expect_eq(&got, &want, "the reported cwd (host:path)");
        }
        if let Some(want) = world::text(step, "expect_clipboard") {
            let got = self.term.take_clipboard().unwrap_or_default();
            return world::expect_eq(&got, &want, "the staged clipboard text");
        }
        if let Some(want) = world::list(step, "expect_modes") {
            return self.expect_modes(&want);
        }
        if let Some(want) = world::int(step, "expect_placements") {
            let got = self.term.placements().len() as i64;
            if got != want {
                return Err(format!("{got} inline diagram placement(s) — expected {want}"));
            }
            return Ok(());
        }
        if let Some(want) = world::text(step, "expect_selection") {
            let got = self.selection.as_ref().map(|s| platform::term::selection::text(&self.term, s)).unwrap_or_default();
            return world::expect_eq(&got, &want, "the selected text");
        }

        Err(world::unknown_verb(step))
    }
}

impl TerminalWorld {
    /// A row of the active screen as a person would read it.
    fn row_text(&self, y: u16) -> String {
        cells_text(self.term.row(y))
    }

    /// A row as it is *displayed*, which differs from `row` once you scroll up.
    fn display_text(&self, y: u16) -> String {
        cells_text(self.term.display_row(y))
    }

    /// `select = ["col", "row"]` with an optional `mode` — word, line, block, or char.
    fn select(&mut self, at: &[String], step: &Toml) -> Result<(), String> {
        let num = |i: usize| at.get(i).and_then(|v| v.parse::<u16>().ok()).ok_or("select needs [col, row]");
        let pos = platform::term::Pos::new(num(0)?, num(1)?);
        let mode = match world::text(step, "mode").unwrap_or_else(|| "char".into()).as_str() {
            "word" => SelectionMode::Word,
            "line" => SelectionMode::Line,
            "char" => SelectionMode::Char,
            other => return Err(format!("unknown selection mode {other:?}")),
        };
        let mut sel = platform::term::selection::expanded(&self.term, pos, mode);
        if let Some(to) = world::list(step, "extend_to") {
            let n = |i: usize| to.get(i).and_then(|v| v.parse::<u16>().ok()).unwrap_or(0);
            sel.extend(platform::term::Pos::new(n(0), n(1)));
        }
        self.selection = Some(sel);
        Ok(())
    }

    /// `expect_cell = "R"` at `row`/`col`, optionally with `fg`, `bg` and `flags`.
    fn expect_cell(&self, want_ch: &str, step: &Toml) -> Result<(), String> {
        let row = world::int(step, "row").unwrap_or(0).max(0) as u16;
        let col = world::int(step, "col").unwrap_or(0).max(0) as usize;
        let cells = self.term.row(row);
        let cell = cells.get(col).copied().unwrap_or(Cell::BLANK);
        let at = format!("cell ({col},{row})");

        if want_ch.chars().next() != Some(cell.ch) {
            return Err(format!("{at} holds {:?} — expected {want_ch:?}", cell.ch));
        }
        if let Some(want) = world::text(step, "fg") {
            world::expect_eq(&color_name(cell.fg), &want, &format!("{at} foreground"))?;
        }
        if let Some(want) = world::text(step, "bg") {
            world::expect_eq(&color_name(cell.bg), &want, &format!("{at} background"))?;
        }
        if let Some(want) = world::list(step, "flags") {
            let got = flag_names(&cell);
            for w in &want {
                if !got.contains(w) {
                    return Err(format!("{at} is not {w} — it is [{}]", got.join(", ")));
                }
            }
        }
        if let Some(bad) = world::list(step, "not_flags") {
            let got = flag_names(&cell);
            for b in &bad {
                if got.contains(b) {
                    return Err(format!("{at} must not be {b} — it is [{}]", got.join(", ")));
                }
            }
        }
        Ok(())
    }

    /// `expect_modes = ["alt", "mouse", "bracketed", "app_cursor"]` — every named mode
    /// must be on, and any not named must be off.
    fn expect_modes(&self, want: &[String]) -> Result<(), String> {
        let all = [
            ("alt", self.term.in_alt_screen()),
            ("mouse", self.term.wants_mouse()),
            ("mouse_sgr", self.term.mouse_sgr()),
            ("bracketed", self.term.bracketed_paste()),
            ("app_cursor", self.term.app_cursor_keys()),
        ];
        for (name, on) in all {
            let expected = want.iter().any(|w| w == name);
            if on != expected {
                return Err(format!(
                    "mode {name} is {} — expected {}",
                    if on { "on" } else { "off" },
                    if expected { "on" } else { "off" }
                ));
            }
        }
        Ok(())
    }
}

/// Cells as text: skip the spacer half of a wide glyph, and drop trailing blanks.
fn cells_text(cells: &[Cell]) -> String {
    cells.iter().filter(|c| !c.is_wide_spacer()).map(|c| c.ch).collect::<String>().trim_end().to_string()
}

fn trim_tail(mut rows: Vec<String>) -> Vec<String> {
    while rows.last().is_some_and(|l| l.is_empty()) {
        rows.pop();
    }
    rows
}

fn color_name(c: Color) -> String {
    match c {
        Color::Default => "default".into(),
        Color::Indexed(i) => match i {
            0 => "black".into(),
            1 => "red".into(),
            2 => "green".into(),
            3 => "yellow".into(),
            4 => "blue".into(),
            5 => "magenta".into(),
            6 => "cyan".into(),
            7 => "white".into(),
            n => format!("index {n}"),
        },
        Color::Rgb(r, g, b) => format!("rgb {r},{g},{b}"),
    }
}

fn flag_names(cell: &Cell) -> Vec<String> {
    use platform::term::CellFlags as F;
    [
        (F::BOLD, "bold"),
        (F::DIM, "dim"),
        (F::ITALIC, "italic"),
        (F::UNDERLINE, "underline"),
        (F::REVERSE, "reverse"),
        (F::STRIKE, "strike"),
        (F::HIDDEN, "hidden"),
    ]
    .iter()
    .filter(|(f, _)| cell.flags.contains(*f))
    .map(|(_, n)| (*n).to_string())
    .collect()
}

fn yes_no(got: bool, want: bool, what: &str) -> Result<(), String> {
    if got == want {
        return Ok(());
    }
    Err(format!("{what}: expected {want}, got {got}"))
}
