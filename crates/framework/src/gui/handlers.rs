//! Keymap action dispatch — `do_action` turns a resolved `Action` (from the
//! keymap / input layer) into a runtime effect (open/close tabs & splits, zoom,
//! scroll, copy/paste, config reload).

use super::*;

impl GuiApp {
    /// Resolve what a close would actually do, and either ask first or do it.
    ///
    /// The escalation is the point: a ⌘W on the last tab does not close a tab, it ends
    /// the session — so it is governed by `confirm_quit` and asks the quit question,
    /// whatever `confirm_close_tab` says. `Tabs::close_tab` returning `None` on the
    /// last tab (and `close_focused` on a single-pane tab) is what makes that knowable
    /// *before* anything closes.
    pub(in crate::gui) fn request_close(&mut self, intent: CloseIntent) {
        let tabs = self.tabs.len();
        let panes_here = self.tabs.active().pane_ids().len();
        let intent = super::confirm::effective_intent(intent, tabs, panes_here);
        if !super::confirm::should_ask(&self.config, intent) {
            self.perform_close(intent);
            return;
        }
        // Counted from the live tree, so the number is the truth rather than a guess.
        // Composed from pluralised fragments so a count reads as a sentence in every
        // locale — "1 split" and "5 splits", never "5 split(s)".
        let splits = |n: usize| crate::i18n::translate("confirm.splits", &[n.to_string()]);
        let detail = match intent {
            CloseIntent::Quit => {
                let panes: usize = self.tabs.iter().map(|t| t.pane_ids().len()).sum();
                let tabs_txt = crate::i18n::translate("confirm.tabs", &[tabs.to_string()]);
                crate::i18n::translate("confirm.stake_quit", &[tabs_txt, splits(panes)])
            }
            CloseIntent::Tab => crate::i18n::translate("confirm.stake_tab", &[splits(panes_here)]),
            CloseIntent::Pane => crate::i18n::translate("confirm.stake_pane", &[]),
        };
        // Persist BEFORE asking, not after confirming.
        //
        // The red button and menu ▸ Quit reach this through `CloseRequested` too, and
        // for those the window is already gone: the platform's run loop force-exits the
        // moment this returns, so a save deferred until "confirmed" would never run and
        // the workspace would be lost. Saving state that is still current is harmless —
        // losing it is not.
        self.save_workspace_now();
        // Two overlays must never both hold input; the question wins.
        self.switcher.dismiss();
        self.confirm.open(intent, detail);
        self.dirty.set();
    }

    /// Carry out a close the user has agreed to (or that needed no agreement).
    pub(in crate::gui) fn perform_close(&mut self, intent: CloseIntent) {
        match intent {
            CloseIntent::Quit => {
                self.save_workspace_now();
                platform::info!("shutting down (quit confirmed)");
                platform::log::flush();
                std::process::exit(0);
            }
            CloseIntent::Tab => {
                // `None` means it was the last tab — which `request_close` already
                // escalated to Quit, so reaching it here would be a logic error, not a
                // reason to exit behind the user's back.
                if self.tabs.close_tab().is_none() {
                    platform::warn!("close tab refused: it is the last one");
                    return;
                }
            }
            CloseIntent::Pane => {
                if self.tabs.active_mut().close_focused().is_none() {
                    platform::warn!("close split refused: it is the last one in the tab");
                    return;
                }
            }
        }
        self.notify_focus_changed();
        self.relayout();
    }

    pub(in crate::gui) fn do_action(&mut self, action: Action) {
        match action {
            Action::NewTab => {
                if let Some(p) = self.open_terminal_pane() {
                    self.tabs.new_tab(p);
                    self.notify_focus_changed();
                    self.relayout();
                }
            }
            Action::CloseTab => self.request_close(CloseIntent::Tab),
            Action::NextTab => {
                self.tabs.next_tab();
                self.notify_focus_changed();
                self.relayout();
            }
            Action::PrevTab => {
                self.tabs.prev_tab();
                self.notify_focus_changed();
                self.relayout();
            }
            Action::GoToTab(n) => {
                self.tabs.goto(n as usize);
                self.notify_focus_changed();
                self.relayout();
            }
            Action::TabSwitcher => {
                self.switcher.open(self.switcher_entries());
                self.dirty.set();
            }
            Action::SplitRight | Action::SplitDown => {
                let axis = if matches!(action, Action::SplitRight) { Axis::Horizontal } else { Axis::Vertical };
                if let Some(p) = self.open_terminal_pane() {
                    self.tabs.active_mut().split(axis, p);
                    self.notify_focus_changed();
                    self.relayout();
                }
            }
            Action::ClosePane => self.request_close(CloseIntent::Pane),
            Action::FocusLeft | Action::FocusRight | Action::FocusUp | Action::FocusDown => {
                let dir = match action {
                    Action::FocusLeft => Dir::Left,
                    Action::FocusRight => Dir::Right,
                    Action::FocusUp => Dir::Up,
                    _ => Dir::Down,
                };
                let area = self.panes_area;
                self.tabs.active_mut().focus_dir(dir, area);
                self.notify_focus_changed();
                self.dirty.set();
            }
            Action::FocusNext => {
                self.tabs.active_mut().focus_next();
                self.notify_focus_changed();
                self.dirty.set();
            }
            Action::ZoomPane => {
                self.tabs.active_mut().toggle_zoom();
                self.relayout();
            }
            Action::ZoomInPane => self.zoom(ZOOM_STEP),
            Action::ZoomOutPane => self.zoom(1.0 / ZOOM_STEP),
            Action::ResetZoom => self.reset_zoom(),
            Action::CycleTabBar => {
                self.tab_bar = self.tab_bar.next();
                self.relayout();
            }
            Action::ReloadConfig => {
                // The active profile's overlay is layered in by `Config::load`; `apply_config`
                // re-applies theme/fonts/zoom/tab-bar/keymap/policy/factory live (the same
                // path a profile switch uses, so the two never drift).
                let new = Config::load();
                self.apply_config(new);
            }
            Action::Copy => {
                // ⌘C copies the mouse selection when there is one; otherwise it is
                // forwarded to the shell (CSI-u ⌘c), where the lineedit plugin
                // copies the KEYBOARD selection (zsh's region) back via OSC 52.
                if self.tabs.active().focused_content().is_some_and(|p| p.session.selection.is_some()) {
                    self.copy_selection();
                } else {
                    self.write_focused(b"\x1b[99;9u");
                }
            }
            Action::Paste => {
                if let Some(t) = platform::os::clipboard_read() {
                    self.write_focused(t.as_bytes());
                }
            }
            // Scroll the focused pane's terminal scrollback.
            Action::ScrollLineUp => self.scroll_focused(ScrollCmd::Lines(-3)),
            Action::ScrollLineDown => self.scroll_focused(ScrollCmd::Lines(3)),
            Action::ScrollPageUp => self.scroll_focused(ScrollCmd::Page(-1)),
            Action::ScrollPageDown => self.scroll_focused(ScrollCmd::Page(1)),
            Action::ScrollTop => self.scroll_focused(ScrollCmd::Top),
            Action::ScrollBottom => self.scroll_focused(ScrollCmd::Bottom),
        }
    }
}
