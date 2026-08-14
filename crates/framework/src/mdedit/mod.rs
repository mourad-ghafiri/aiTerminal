//! `@md edit` — a full-screen split Markdown editor: raw source on the left, a LIVE rendered
//! preview on the right (native diagrams included), with vertical + horizontal scroll by keyboard
//! and mouse. A self-contained alt-screen TUI over stdin/stdout — it needs no GUI; it runs inside
//! aiTerminal (mouse works, diagrams draw natively) or any xterm (mouse via the host, diagrams as
//! boxes). The pure pieces — the text buffer, the preview layout, key/mouse parsing, and the
//! horizontal slicers — are split out and unit-tested; the run loop just does I/O.

// The pure pieces, one file each; `session` and `pager` are the two run loops.
mod buffer;
mod chrome;
mod editor;
pub(crate) mod key;
mod pager;
mod preview;
mod session;

/// The diagram fence language (kept internal — never shown to the user).
pub(crate) const DIAGRAM_LANG: &str = "mermaid";

// What the rest of the crate names: the two entry points, and the preview model
// `@md render` measures a document with before choosing inline vs pager.
pub use pager::page;
pub use session::run;

/// `@md render` measures a document with these before choosing inline vs pager.
pub(crate) use preview::preview_height;

/// The scenario world drives the editor and the pager the way a person does.
#[cfg(test)]
pub(crate) use crate::mdedit::{
    pager::Pager,
    preview::{build_preview_at, DiagramPaint, PObj, PRow},
};

#[cfg(test)]
mod tests;
