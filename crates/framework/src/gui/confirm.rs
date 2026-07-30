//! The close-confirmation modal: the thing standing between an accidental ⌘Q and a
//! session with every tab, split and running shell in it.
//!
//! Mirrors [`switcher`](super::switcher): the broker owns the open state and the
//! answer, the renderer draws a centered panel and records per-button hit rects for
//! the mouse, and the *effect* — actually closing the pane, tab or process — stays on
//! `GuiApp`. Neither module needs to know what a tab is.
//!
//! **Cancel holds focus when it opens.** A feature whose whole purpose is catching a
//! mis-hit key must survive the reflex that follows one: the hand that hit ⌘Q by
//! accident hits Enter next. Confirming is deliberate — →, a click, or ⌘Q again.

use super::*;

/// What saying yes actually does. The modal never performs it; `GuiApp` reads this
/// back and acts, which is what keeps this module free of tabs and panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseIntent {
    Pane,
    Tab,
    /// Ends the session — ⌘Q, or closing the last tab/pane, which amounts to the same
    /// thing and deserves the same warning.
    Quit,
}

impl CloseIntent {
    /// The question this intent asks. Written as a match of LITERAL keys rather than
    /// one formatted from a stem: `i18n_no_dead_keys` finds references by scanning the
    /// source for translate-call sites, so a computed key would quietly exempt this
    /// whole section from the check that keeps dead translations out.
    fn title(self) -> String {
        match self {
            CloseIntent::Pane => crate::i18n::translate("confirm.pane_title", &[]),
            CloseIntent::Tab => crate::i18n::translate("confirm.tab_title", &[]),
            CloseIntent::Quit => crate::i18n::translate("confirm.quit_title", &[]),
        }
    }

    /// The confirm button's label — what it does, not "OK".
    fn button(self) -> String {
        match self {
            CloseIntent::Pane => crate::i18n::translate("confirm.pane_button", &[]),
            CloseIntent::Tab => crate::i18n::translate("confirm.tab_button", &[]),
            CloseIntent::Quit => crate::i18n::translate("confirm.quit_button", &[]),
        }
    }
}

/// What a close **actually** does, given how much is open.
///
/// The escalation is the whole safety argument: a ⌘W on the last tab does not close a
/// tab, it ends the session — so it must ask the quit question and obey `confirm_quit`,
/// whatever the tab setting says. Pure, so the rule can be tested without a window.
pub(crate) fn effective_intent(intent: CloseIntent, tabs: usize, panes_in_tab: usize) -> CloseIntent {
    match intent {
        // The last split of the last tab: closing it leaves nothing behind.
        CloseIntent::Pane if panes_in_tab <= 1 && tabs <= 1 => CloseIntent::Quit,
        // The last split of a tab IS a tab close, and is asked about as one.
        CloseIntent::Pane if panes_in_tab <= 1 => CloseIntent::Tab,
        CloseIntent::Tab if tabs <= 1 => CloseIntent::Quit,
        other => other,
    }
}

/// Whether this intent is one the user has asked to be warned about.
pub(crate) fn should_ask(cfg: &Config, intent: CloseIntent) -> bool {
    match intent {
        CloseIntent::Pane => cfg.confirm_close_pane,
        CloseIntent::Tab => cfg.confirm_close_tab,
        CloseIntent::Quit => cfg.confirm_quit,
    }
}

/// Which button has focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Button {
    Cancel,
    Confirm,
}

/// The open modal's state.
pub(crate) struct ConfirmState {
    intent: CloseIntent,
    /// What is at stake, counted from the live tree by the caller — never guessed.
    detail: String,
    focus: Button,
    /// Per-button screen rects, recorded by the renderer for mouse hit-testing.
    pub(crate) button_rects: Vec<(Button, Rect)>,
}

impl ConfirmState {
    fn new(intent: CloseIntent, detail: String) -> Self {
        // Cancel, always. See the module note.
        ConfirmState { intent, detail, focus: Button::Cancel, button_rects: Vec::new() }
    }

    /// Move focus between the two buttons. Any non-zero delta flips, so ←/→/Tab all
    /// work without the caller caring which direction it is.
    fn move_focus(&mut self) {
        self.focus = match self.focus {
            Button::Cancel => Button::Confirm,
            Button::Confirm => Button::Cancel,
        };
    }

    /// The button under a recorded hit rect, if any. `None` is the backdrop.
    fn button_at(&self, p: Point) -> Option<Button> {
        self.button_rects.iter().find(|(_, r)| r.contains(p)).map(|(b, _)| *b)
    }
}

/// Owns the single open confirmation (or none). Modal while open: the input layer
/// routes keys here instead of to the focused pane or the switcher.
pub(crate) struct Confirm {
    state: Option<ConfirmState>,
}

impl Confirm {
    pub(crate) fn new() -> Self {
        Confirm { state: None }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.state.is_some()
    }

    pub(crate) fn open(&mut self, intent: CloseIntent, detail: String) {
        self.state = Some(ConfirmState::new(intent, detail));
    }

    pub(crate) fn dismiss(&mut self) {
        self.state = None;
    }

    pub(crate) fn move_focus(&mut self) {
        if let Some(s) = &mut self.state {
            s.move_focus();
        }
    }

    /// The intent, if the focused button is Confirm — what Enter resolves to. Closes
    /// the modal either way: a decision was made.
    pub(crate) fn take_focused(&mut self) -> Option<CloseIntent> {
        let s = self.state.take()?;
        (s.focus == Button::Confirm).then_some(s.intent)
    }

    /// The intent regardless of focus — what a second ⌘Q resolves to. Pressing the
    /// quit chord again while being asked about quitting is not ambiguous.
    pub(crate) fn take_confirmed(&mut self) -> Option<CloseIntent> {
        self.state.take().map(|s| s.intent)
    }

    /// Resolve a click. `Some(Some(intent))` confirmed, `Some(None)` cancelled (a
    /// click on Cancel or on the backdrop), `None` when nothing is open.
    pub(crate) fn click_at(&mut self, p: Point) -> Option<Option<CloseIntent>> {
        let s = self.state.as_ref()?;
        let hit = s.button_at(p);
        let intent = s.intent;
        self.state = None; // a click anywhere decides
        Some(matches!(hit, Some(Button::Confirm)).then_some(intent))
    }

    pub(crate) fn state_mut(&mut self) -> Option<&mut ConfirmState> {
        self.state.as_mut()
    }
}

/// Draw the confirmation (app-owned, above every pane and the switcher): a dimmed
/// window and a centered panel with a title, what is at stake, two buttons and the
/// keys that drive them. Records the button rects into `s`.
pub(crate) fn draw_confirm(
    surface: &mut Surface,
    cache: &mut GlyphCache,
    theme: &Theme,
    base_px: f32,
    w: u32,
    h: u32,
    s: &mut ConfirmState,
) {
    use corelib::gfx::text::{draw_text, measure_text};
    let (wf, hf) = (w as f32, h as f32);
    // Darker than the switcher's scrim: this one is asking a question, and the
    // answer matters more than seeing what is behind it.
    surface.fill_rect(Rect::new(0.0, 0.0, wf, hf), corelib::types::Rgba8::new(0, 0, 0, 0xC8));

    let m = cache.metrics(base_px);
    let pad = 22.0;
    let title = s.intent.title();
    let confirm_label = s.intent.button();
    let cancel_label = crate::i18n::translate("confirm.cancel", &[]);
    let hint = crate::i18n::translate("confirm.hint", &[]);

    let btn_h = m.cell_h + 16.0;
    let gap = 12.0;
    // Buttons are sized to their own text, so a translated label can never be clipped
    // by a width chosen for English.
    let mut btn_w = |label: &str| (measure_text(cache, label, base_px) + 34.0).max(96.0);
    let (cancel_w, confirm_w) = (btn_w(&cancel_label), btn_w(&confirm_label));

    let content_w = measure_text(cache, &title, base_px)
        .max(measure_text(cache, &s.detail, base_px))
        .max(measure_text(cache, &hint, base_px))
        .max(cancel_w + gap + confirm_w + 40.0);
    let pw = (content_w + 2.0 * pad).clamp(320.0, (wf - 40.0).max(320.0));
    let ph = pad + m.cell_h + 10.0 + m.cell_h + 18.0 + btn_h + 14.0 + m.cell_h + pad;
    let px = ((wf - pw) * 0.5).round();
    let py = ((hf - ph) * 0.5).round();

    surface.fill_rounded_rect(Rect::new(px, py, pw, ph), 12.0, theme.surface);
    surface.fill_rect(Rect::new(px, py, pw, 2.0), theme.accent); // accent rule

    // Title, then what is actually at stake.
    let mut y = py + pad + m.ascent;
    draw_text(surface, cache, &title, base_px, px + pad, y, theme.fg, px + pw - pad, true);
    y += m.cell_h + 10.0;
    draw_text(surface, cache, &s.detail, base_px, px + pad, y, theme.muted, px + pw - pad, false);

    // Buttons, right-aligned, Cancel first so the safe choice is where the eye lands.
    let by = y + 18.0;
    let confirm_x = px + pw - pad - confirm_w;
    let cancel_x = confirm_x - gap - cancel_w;
    s.button_rects.clear();
    for (button, x, bw, label) in [
        (Button::Cancel, cancel_x, cancel_w, &cancel_label),
        (Button::Confirm, confirm_x, confirm_w, &confirm_label),
    ] {
        let rect = Rect::new(x, by, bw, btn_h);
        let focused = s.focus == button;
        // The focused button is filled; the other is an outline. Fill beats a ring for
        // a two-button choice — it reads at a glance, which is all the time anyone
        // gives a dialog they did not mean to open.
        if focused {
            surface.fill_rounded_rect(rect, 8.0, theme.accent);
        } else {
            surface.fill_rounded_rect(rect, 8.0, theme.bg);
            super::frame::draw_frame(surface, rect, theme.muted, 1.0);
        }
        let label_w = measure_text(cache, label, base_px);
        let tx = x + (bw - label_w) * 0.5;
        let tb = by + (btn_h - m.cell_h) * 0.5 + m.ascent;
        let fg = if focused { theme.bg } else { theme.fg };
        draw_text(surface, cache, label, base_px, tx, tb, fg, x + bw, focused);
        s.button_rects.push((button, rect));
    }

    // The keys that drive it — nobody should have to guess at a modal.
    let hy = by + btn_h + 14.0 + m.ascent;
    draw_text(surface, cache, &hint, base_px, px + pad, hy, theme.muted, px + pw - pad, false);
}

/// Headless proof of the confirmation over a faint backdrop — no GUI session needed.
pub fn render_confirm_proof(out_path: &str) -> std::io::Result<()> {
    // The catalog is thread-local and installed at boot; a `--render-*` invocation
    // never boots the window, so without this the proof would draw its own key names
    // and prove nothing about the strings people actually read.
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    let theme = corelib::theme::midnight();
    let mut cache = GlyphCache::new(platform::os::text_shaper());
    let (w, h) = (920u32, 560u32);
    let mut surface = Surface::new(w, h);
    surface.clear(theme.bg);
    // Built exactly the way `request_close` builds it, so the proof shows the real
    // sentence rather than a hand-written lookalike that could drift from it.
    let detail = crate::i18n::translate(
        "confirm.stake_quit",
        &[
            crate::i18n::translate("confirm.tabs", &["3".into()]),
            crate::i18n::translate("confirm.splits", &["5".into()]),
        ],
    );
    let mut s = ConfirmState::new(CloseIntent::Quit, detail);
    draw_confirm(&mut surface, &mut cache, &theme, 26.0, w, h, &mut s);
    crate::render::write_ppm(out_path, surface.pixels(), w, h)?;
    println!("rendered close confirmation \u{2192} {w}\u{00d7}{h}px \u{2192} {out_path}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(intent: CloseIntent) -> Confirm {
        let mut c = Confirm::new();
        c.open(intent, "2 tabs \u{00b7} 3 panes will close".into());
        c
    }

    #[test]
    fn enter_on_a_fresh_modal_keeps_the_session() {
        // THE test. A hand that hit ⌘Q by accident hits Enter next; if that quits,
        // the modal has achieved nothing but an extra keystroke.
        let mut c = open(CloseIntent::Quit);
        assert!(c.is_open());
        assert_eq!(c.take_focused(), None, "Enter with Cancel focused cancels");
        assert!(!c.is_open(), "and the modal closes — a decision was made");
    }

    #[test]
    fn moving_focus_then_entering_confirms() {
        let mut c = open(CloseIntent::Quit);
        c.move_focus();
        assert_eq!(c.take_focused(), Some(CloseIntent::Quit));
        assert!(!c.is_open());

        // Focus flips back and forth — ←/→/Tab all land on the same two buttons.
        let mut c = open(CloseIntent::Tab);
        c.move_focus();
        c.move_focus();
        assert_eq!(c.take_focused(), None, "back on Cancel");
    }

    #[test]
    fn a_second_quit_press_confirms_without_moving_focus() {
        // Pressing the quit chord again while being asked about quitting is not
        // ambiguous — it is the same intent, stated twice.
        let mut c = open(CloseIntent::Quit);
        assert_eq!(c.take_confirmed(), Some(CloseIntent::Quit));
        assert!(!c.is_open());
    }

    #[test]
    fn dismissing_resolves_to_nothing() {
        let mut c = open(CloseIntent::Pane);
        c.dismiss();
        assert!(!c.is_open());
        assert_eq!(c.take_focused(), None, "a dismissed modal answers nothing");
        assert_eq!(c.take_confirmed(), None);
    }

    #[test]
    fn a_click_hits_the_button_under_it_and_the_backdrop_cancels() {
        let mut c = open(CloseIntent::Tab);
        // The renderer records the rects; fake them so the hit-testing is what is tested.
        let s = c.state_mut().unwrap();
        s.button_rects = vec![
            (Button::Cancel, Rect::new(100.0, 100.0, 80.0, 30.0)),
            (Button::Confirm, Rect::new(200.0, 100.0, 80.0, 30.0)),
        ];
        assert_eq!(c.click_at(Point::new(240.0, 110.0)), Some(Some(CloseIntent::Tab)), "on Confirm");
        assert!(!c.is_open());

        let mut c = open(CloseIntent::Tab);
        c.state_mut().unwrap().button_rects = vec![(Button::Cancel, Rect::new(100.0, 100.0, 80.0, 30.0))];
        assert_eq!(c.click_at(Point::new(140.0, 110.0)), Some(None), "on Cancel");

        let mut c = open(CloseIntent::Tab);
        c.state_mut().unwrap().button_rects = vec![(Button::Confirm, Rect::new(200.0, 100.0, 80.0, 30.0))];
        assert_eq!(c.click_at(Point::new(10.0, 10.0)), Some(None), "the backdrop cancels");
        assert!(!c.is_open());

        // Nothing open → nothing to resolve.
        let mut c = Confirm::new();
        assert_eq!(c.click_at(Point::new(0.0, 0.0)), None);
    }

    #[test]
    fn a_close_that_ends_the_session_asks_about_quitting() {
        use CloseIntent::*;
        // The escalation that makes the feature safe rather than decorative. ⌘W on the
        // last tab does not close a tab — it ends the session, and must say so.
        assert_eq!(effective_intent(Tab, 1, 1), Quit, "the last tab");
        assert_eq!(effective_intent(Pane, 1, 1), Quit, "the last split of the last tab");
        // The last split of a tab is a tab close, and is asked about as one.
        assert_eq!(effective_intent(Pane, 3, 1), Tab, "the last split, other tabs open");
        // Nothing to escalate when there is plenty left.
        assert_eq!(effective_intent(Pane, 3, 4), Pane);
        assert_eq!(effective_intent(Tab, 3, 4), Tab);
        // Quit is already the top of the ladder.
        assert_eq!(effective_intent(Quit, 9, 9), Quit);
        // A zero count cannot slip past the guard into "just close a split".
        assert_eq!(effective_intent(Pane, 0, 0), Quit);
    }

    #[test]
    fn the_shipped_defaults_are_the_ones_asked_for() {
        use CloseIntent::*;
        let cfg = Config::default();
        assert!(!should_ask(&cfg, Pane), "a split is cheap to reopen — no prompt by default");
        assert!(should_ask(&cfg, Tab), "a tab holds splits and their shells");
        assert!(should_ask(&cfg, Quit), "⌘Q takes everything");

        // And each key actually governs its own intent — a settings screen that
        // silently ignores one is worse than not having it.
        let off = Config { confirm_close_pane: false, confirm_close_tab: false, confirm_quit: false, ..Config::default() };
        for i in [Pane, Tab, Quit] {
            assert!(!should_ask(&off, i), "{i:?} respects its off switch");
        }
        let on = Config { confirm_close_pane: true, confirm_close_tab: true, confirm_quit: true, ..Config::default() };
        for i in [Pane, Tab, Quit] {
            assert!(should_ask(&on, i), "{i:?} respects its on switch");
        }
    }

    #[test]
    fn every_intent_asks_its_own_question() {
        // A pane prompt that said "Quit aiTerminal?" would be worse than no prompt: it
        // would teach people to confirm without reading.
        //
        // The catalog is thread-local and only installed at boot, so a unit test gets
        // the key back unless it installs one — which is what makes this a real check
        // on the shipped strings rather than on three distinct constants.
        const BUILTIN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../builtin/i18n");
        crate::i18n::install(crate::i18n::Catalog::load(&[std::path::Path::new(BUILTIN)], "en"));

        let intents = [CloseIntent::Pane, CloseIntent::Tab, CloseIntent::Quit];
        let titles: Vec<String> = intents.iter().map(|i| i.title()).collect();
        let buttons: Vec<String> = intents.iter().map(|i| i.button()).collect();

        for (i, text) in intents.iter().zip(titles.iter().chain(buttons.iter())) {
            assert!(!text.starts_with("confirm."), "{i:?} resolves to a real string, not the key: {text}");
            assert!(!text.trim().is_empty(), "{i:?} has words");
        }
        assert_eq!(titles.iter().collect::<std::collections::HashSet<_>>().len(), 3, "distinct: {titles:?}");
        assert_eq!(buttons.iter().collect::<std::collections::HashSet<_>>().len(), 3, "distinct: {buttons:?}");
        // The one that matters most: the quit question names quitting, and the split
        // question does not.
        assert!(titles[2].to_lowercase().contains("quit"), "{}", titles[2]);
        assert!(!titles[0].to_lowercase().contains("quit"), "a split prompt must not say quit: {}", titles[0]);
    }
}
