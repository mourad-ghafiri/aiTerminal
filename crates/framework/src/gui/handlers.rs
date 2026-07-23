//! Keymap action dispatch — `do_action` turns a resolved `Action` (from the
//! keymap / input layer) into a runtime effect (open/close tabs & splits, zoom,
//! scroll, copy/paste, config reload).

use super::*;

impl GuiApp {
    pub(in crate::gui) fn do_action(&mut self, action: Action) {
        match action {
            Action::NewTab => {
                if let Some(p) = self.open_terminal_pane() {
                    self.tabs.new_tab(p);
                    self.notify_focus_changed();
                    self.relayout();
                }
            }
            Action::CloseTab => {
                if self.tabs.close_tab().is_none() {
                    self.save_workspace_now();
                    platform::info!("shutting down (last tab closed)");
                    platform::log::flush();
                    std::process::exit(0);
                }
                self.notify_focus_changed();
                self.relayout();
            }
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
            Action::ClosePane => {
                if self.tabs.active_mut().close_focused().is_none() && self.tabs.close_tab().is_none() {
                    self.save_workspace_now();
                    platform::info!("shutting down (last pane closed)");
                    platform::log::flush();
                    std::process::exit(0);
                }
                self.notify_focus_changed();
                self.relayout();
            }
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
