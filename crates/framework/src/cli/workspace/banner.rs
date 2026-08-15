//! The opening screen's raw material: the mark and the facts.
//!
//! Pure data — the native welcome (`gui::chat::welcome`) draws it with the
//! engine. The mark is drawn in the same rounded strokes the flow board's cards
//! and `@md`'s diagrams use, so the first screen and the rest of the product are
//! one hand.

/// The wordmark, three rows of the product's own rounded strokes.
pub(crate) const MARK: [&str; 3] = [
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
