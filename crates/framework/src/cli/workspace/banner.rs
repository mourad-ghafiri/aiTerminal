//! The opening banner: the mark, the folder, the facts.
//!
//! Pure — rows in, rows out — like every renderer in the workspace. The mark is
//! drawn in the same rounded strokes the flow board's cards and `@md`'s diagrams
//! use, so the first screen and the rest of the product are one hand. Under ~56
//! columns the art yields to a one-line wordmark rather than wrapping into noise.

use crate::cli::style::{accent, muted, reset};

/// The wordmark, three rows of the product's own rounded strokes.
const MARK: [&str; 3] = [
    "\u{256d}\u{2500}\u{256e} \u{2577} \u{2576}\u{252c}\u{2574} \u{256d}\u{2500}\u{2574} \u{256d}\u{2500}\u{256e} \u{256d}\u{252c}\u{256e} \u{2577} \u{256d}\u{256e}\u{2577} \u{256d}\u{2500}\u{256e} \u{2577}",
    "\u{251c}\u{2500}\u{2524} \u{2502}  \u{2502}  \u{251c}\u{2500}\u{2574} \u{251c}\u{252c}\u{256f} \u{2502}\u{2502}\u{2502} \u{2502} \u{2502}\u{2570}\u{2524} \u{251c}\u{2500}\u{2524} \u{2502}",
    "\u{2575} \u{2575} \u{2575}  \u{2575}  \u{2570}\u{2500}\u{2574} \u{2575}\u{2570}\u{2574} \u{2575}\u{2575}\u{2575} \u{2575} \u{2575} \u{2575} \u{2575} \u{2575} \u{2570}\u{2500}\u{2574}",
];

/// What the banner states.
pub(crate) struct Facts {
    pub root: String,
    /// The overlay inventory line ("project overlay ON — 2 agent(s) · …"), or why not.
    pub overlay: String,
    /// The instructions file at the root, when there is one.
    pub instructions: Option<&'static str>,
    /// "3 model(s) · strategy weighted" — or `None` when nothing is configured yet.
    pub pool: Option<String>,
}

/// The full banner for the opening screen.
pub(crate) fn render(facts: &Facts, cols: usize) -> Vec<String> {
    let (a, dim, r) = (accent(), muted(), reset());
    let mut rows: Vec<String> = Vec::new();
    match cols >= 56 {
        true => rows.extend(MARK.iter().map(|line| format!("{a}{line}{r}"))),
        false => rows.push(format!("{a}\u{2726} aiTerminal{r}")),
    }
    rows.push(String::new());
    rows.push(format!("{dim}the folder as a conversation \u{b7} v{}{r}", env!("CARGO_PKG_VERSION")));
    rows.push(format!("{a}{}{r}", facts.root));
    rows.push(format!("{dim}{}{r}", facts.overlay));
    if let Some(name) = facts.instructions {
        rows.push(format!("{dim}instructions: {name}{r}"));
    }
    rows.push(match &facts.pool {
        Some(pool) => format!("{dim}{pool} \u{b7} answers render as Markdown, diagrams included{r}"),
        None => format!("{dim}no model configured yet \u{2014} the workspace opens anyway; a prompt will say how to add one{r}"),
    });
    rows.push(String::new());
    rows.push(format!(
        "{dim}enter sends \u{b7} ctrl+j newline \u{b7} / commands \u{b7} @ agents & flows \u{b7} ! shell \u{b7} shift+tab plan \u{b7} esc interrupts{r}"
    ));
    rows
}
