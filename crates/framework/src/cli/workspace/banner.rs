//! The opening screen's raw material: the facts the welcome states.
//!
//! Pure data — the native welcome (`gui::chat::welcome`) draws it with the
//! engine, logo included.

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
