//! Input routing + the platform event loop: `GuiApp` as the `EventHandler`
//! (key/mouse/resize/redraw), plus the control-byte and cursor-navigation
//! sequence encoders the focused PTY receives.

use super::*;

impl EventHandler for GuiApp {
    fn init(&mut self, win: &dyn Window, _gpu: &mut dyn Gpu) {
        self.ensure_cache(win.scale_factor());
        self.win_px = win.size_px();
        self.relayout();
    }

    fn handle(&mut self, ev: Event, win: &dyn Window, gpu: &mut dyn Gpu) {
        // The tab quick-switcher is modal: while open it captures typing (number or
        // filter), arrow navigation, Enter to jump, and Esc to close.
        if self.switcher.is_open() {
            match &ev {
                Event::KeyDown { code: KeyCode::Escape, .. } => {
                    self.switcher.dismiss();
                    self.dirty.set();
                }
                Event::KeyDown { code: KeyCode::Enter, .. } => self.confirm_switcher(),
                Event::KeyDown { code: KeyCode::Backspace, .. } => {
                    self.switcher.backspace();
                    self.dirty.set();
                }
                Event::KeyDown { code: KeyCode::Up, .. } => {
                    self.switcher.move_sel(-1);
                    self.dirty.set();
                }
                Event::KeyDown { code: KeyCode::Down, .. } => {
                    self.switcher.move_sel(1);
                    self.dirty.set();
                }
                Event::TextInput { text } => {
                    self.switcher.type_text(text);
                    self.dirty.set();
                }
                Event::MouseDown { button: MouseButton::Left, pos, .. } => {
                    let s = self.scale as f32;
                    let p = Point::new(pos.x * s, pos.y * s);
                    if let Some(tab) = self.switcher.pick_at(p) {
                        self.tabs.goto(tab);
                        self.notify_focus_changed();
                        self.relayout();
                    }
                    self.switcher.dismiss(); // a click anywhere (row or backdrop) resolves/closes
                    self.dirty.set();
                }
                Event::RedrawRequested => self.render(gpu),
                Event::Resized { width_px, height_px, scale } => {
                    self.ensure_cache(*scale);
                    self.win_px = (*width_px, *height_px);
                    self.relayout();
                }
                Event::CloseRequested => {
                    self.save_workspace_now();
                    std::process::exit(0)
                }
                _ => {}
            }
            return;
        }
        match ev {
            Event::Resized { width_px, height_px, scale } => {
                self.ensure_cache(scale);
                self.win_px = (width_px, height_px);
                self.relayout();
            }
            Event::KeyDown { code, mods, .. } => {
                let chord = Chord::new(code, mods);
                if let Some(action) = self.keymap.lookup(&chord).cloned() {
                    self.do_action(action);
                    return;
                }
                // A visible mouse selection captures a bare Enter: copy it (like ⌘C)
                // instead of running the command — a selection is intent to grab
                // text, not to execute. The highlight clears; Enter again runs.
                if code == KeyCode::Enter
                    && mods.is_empty()
                    && self.tabs.active().focused_content().is_some_and(|p| p.session.selection.is_some())
                {
                    self.copy_selection();
                    if let Some(Pane { session: s, .. }) = self.tabs.active_mut().focused_content_mut() {
                        s.selection = None;
                    }
                    self.dirty.set();
                    return;
                }
                // unbound keys go to the focused PTY
                if let Some(Pane { session: s, .. }) = self.tabs.active_mut().focused_content_mut() {
                    s.selection = None;
                    s.term.lock().unwrap_or_else(|e| e.into_inner()).scroll_to_bottom(); // typing returns to live
                    if let Some(seq) = encode_key(code, mods) {
                        s.write(&seq);
                    }
                }
            }
            Event::TextInput { text } => {
                if let Some(Pane { session: s, .. }) = self.tabs.active_mut().focused_content_mut() {
                    s.selection = None;
                    s.term.lock().unwrap_or_else(|e| e.into_inner()).scroll_to_bottom(); // typing returns to live
                    s.write(text.as_bytes());
                }
            }
            Event::MouseDown { button, pos, mods } => self.on_mouse_down(button, pos, mods),
            Event::MouseMove { pos, mods } => {
                // A live tab-reorder drag owns the pointer: update the carried pill + drop gap
                // and skip the pane hover/selection logic below.
                if let Some(mut d) = self.tab_drag.take() {
                    let scale = self.scale as f32;
                    let cursor = Point::new(pos.x * scale, pos.y * scale);
                    if (cursor.x - d.grab.x).abs() > 6.0 || (cursor.y - d.grab.y).abs() > 6.0 {
                        d.moved = true;
                    }
                    d.cursor = cursor;
                    d.gap = self.tab_drop_gap(cursor);
                    self.tab_drag = Some(d);
                    self.dirty.set();
                    return;
                }
                // ⌘-hover over a terminal underlines the link under the pointer (the
                // "⌘-click to open" cue); any non-⌘ move clears it.
                let cmd = mods.contains(Modifiers::SUPER);
                if let Some((id, rect)) = self.pane_at(pos) {
                    if self.update_link_hover(id, rect, pos, cmd) {
                        self.dirty.set();
                    } else if !cmd && self.link_hover.is_some() {
                        self.link_hover = None;
                        self.dirty.set();
                    }
                }
                if let Some(pane) = self.dragging {
                    if let Some((p, rect)) = self.pane_at(pos) {
                        if p == pane {
                            let cell = self.cell_at(pane, rect, pos); // before borrowing tabs
                            if let Some(Pane { session: s, .. }) = self.tabs.active_mut().get_mut(pane) {
                                if let Some(sel) = &mut s.selection {
                                    sel.extend(cell);
                                    self.dirty.set();
                                }
                            }
                        }
                    }
                }
            }
            Event::MouseUp { button: MouseButton::Left, .. } => {
                // Commit a tab-reorder drag: convert the visual gap to a final index (a drop
                // after the grabbed tab shifts left by one once it's removed) and move it.
                if let Some(d) = self.tab_drag.take() {
                    if d.moved {
                        let to = if d.gap > d.from { d.gap - 1 } else { d.gap };
                        self.tabs.move_tab(d.from, to);
                        self.notify_focus_changed();
                        self.relayout();
                    }
                    self.dirty.set();
                    return;
                }
                // A text drag ended — copy the selection on release.
                if self.dragging.take().is_some() {
                    self.copy_selection();
                }
            }
            Event::Scroll { delta, pos, .. } => {
                if let Some((id, _rect)) = self.pane_at(pos) {
                    let base_px = self.base_px();
                    let dy = match delta {
                        ScrollDelta::Lines { y, .. } => -y * base_px * 1.3,
                        ScrollDelta::Pixels { y, .. } => -y,
                    };
                    // Scroll the scrollback history by lines (wheel-up = into history).
                    if let Some(Pane { session: s, .. }) = self.tabs.active_mut().get_mut(id) {
                        let lines = (-dy / base_px).round() as i32;
                        if lines != 0 {
                            s.term.lock().unwrap_or_else(|e| e.into_inner()).scroll_view(lines);
                            self.dirty.set();
                        }
                    }
                }
            }
            Event::RedrawRequested => self.render(gpu),
            Event::CloseRequested => {
                self.save_workspace_now();
                std::process::exit(0)
            }
            _ => {
                let _ = win;
            }
        }
    }
}

/// The xterm modifier parameter for modified keys: `1 + Shift(1) + Alt(2) +
/// Ctrl(4) + Meta(8)` — Cmd maps to Meta, so every combination stays
/// distinguishable by the shell.
fn xterm_mod(mods: Modifiers) -> u8 {
    let mut m = 1;
    if mods.contains(Modifiers::SHIFT) {
        m += 1;
    }
    if mods.contains(Modifiers::ALT) {
        m += 2;
    }
    if mods.contains(Modifiers::CONTROL) {
        m += 4;
    }
    if mods.contains(Modifiers::SUPER) {
        m += 8;
    }
    m
}

/// The bytes an unbound key press sends to the PTY. Plain keys keep the classic
/// sequences (`nav_seq`); a MODIFIED nav/edit key becomes the standard xterm
/// `CSI 1;<mod>` form (`⇧←` → `ESC [1;2D`, `⌘←` → `ESC [1;9D`, …) so shell
/// plugins can bind word jumps, line jumps and shift-selection — the engine
/// stays generic, the meaning lives in the plugins (see builtin/plugins/lineedit).
fn encode_key(code: KeyCode, mods: Modifiers) -> Option<Vec<u8>> {
    use KeyCode::*;
    let m = xterm_mod(mods);
    if m > 1 {
        let seq = match code {
            Up => format!("\x1b[1;{m}A"),
            Down => format!("\x1b[1;{m}B"),
            Right => format!("\x1b[1;{m}C"),
            Left => format!("\x1b[1;{m}D"),
            Home => format!("\x1b[1;{m}H"),
            End => format!("\x1b[1;{m}F"),
            Delete => format!("\x1b[3;{m}~"),
            PageUp => format!("\x1b[5;{m}~"),
            PageDown => format!("\x1b[6;{m}~"),
            Tab if m == 2 => "\x1b[Z".into(), // Shift+Tab = back-tab (completion menus)
            // ⌥⌫ is the classic meta-backspace (zsh: backward-kill-word out of the box);
            // ⌘⌫ has no legacy encoding — use the CSI-u form the lineedit plugin binds.
            Backspace if mods.contains(Modifiers::ALT) => "\x1b\x7f".into(),
            Backspace if mods.contains(Modifiers::SUPER) => format!("\x1b[127;{m}u"),
            _ => String::new(),
        };
        if !seq.is_empty() {
            return Some(seq.into_bytes());
        }
    }
    if mods.contains(Modifiers::CONTROL) {
        return ctrl_byte(code).map(|b| vec![b]);
    }
    nav_seq(code).map(|s| s.to_vec())
}

fn ctrl_byte(code: KeyCode) -> Option<u8> {
    use KeyCode::*;
    let n: u8 = match code {
        A => 1, B => 2, C => 3, D => 4, E => 5, F => 6, G => 7, H => 8, I => 9,
        J => 10, K => 11, L => 12, M => 13, N => 14, O => 15, P => 16, Q => 17,
        R => 18, S => 19, T => 20, U => 21, V => 22, W => 23, X => 24, Y => 25,
        Z => 26, BracketLeft => 27, Backslash => 28, BracketRight => 29, Space => 0,
        _ => return None,
    };
    Some(n)
}

fn nav_seq(code: KeyCode) -> Option<&'static [u8]> {
    use KeyCode::*;
    Some(match code {
        Enter => b"\r",
        Tab => b"\t",
        Backspace => b"\x7f",
        Escape => b"\x1b",
        Left => b"\x1b[D",
        Right => b"\x1b[C",
        Up => b"\x1b[A",
        Down => b"\x1b[B",
        Home => b"\x1b[H",
        End => b"\x1b[F",
        PageUp => b"\x1b[5~",
        PageDown => b"\x1b[6~",
        Delete => b"\x1b[3~",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(code: KeyCode, mods: Modifiers) -> Option<Vec<u8>> {
        encode_key(code, mods)
    }
    fn seq(code: KeyCode, mods: Modifiers) -> String {
        String::from_utf8(enc(code, mods).expect("a sequence")).unwrap()
    }

    #[test]
    fn plain_keys_keep_the_classic_sequences() {
        assert_eq!(seq(KeyCode::Left, Modifiers::empty()), "\x1b[D");
        assert_eq!(seq(KeyCode::Up, Modifiers::empty()), "\x1b[A");
        assert_eq!(seq(KeyCode::Home, Modifiers::empty()), "\x1b[H");
        assert_eq!(seq(KeyCode::Backspace, Modifiers::empty()), "\x7f");
        assert_eq!(seq(KeyCode::Enter, Modifiers::empty()), "\r");
        assert_eq!(enc(KeyCode::A, Modifiers::empty()), None); // letters arrive as TextInput
    }

    #[test]
    fn modified_arrows_use_the_xterm_mod_encoding() {
        // 1 + shift(1) + alt(2) + ctrl(4) + cmd(8)
        assert_eq!(seq(KeyCode::Left, Modifiers::SHIFT), "\x1b[1;2D"); // ⇧← select char
        assert_eq!(seq(KeyCode::Left, Modifiers::ALT), "\x1b[1;3D"); // ⌥← word jump
        assert_eq!(seq(KeyCode::Right, Modifiers::SHIFT | Modifiers::ALT), "\x1b[1;4C"); // ⇧⌥→ select word
        assert_eq!(seq(KeyCode::Left, Modifiers::CONTROL), "\x1b[1;5D"); // ⌃← word jump
        assert_eq!(seq(KeyCode::Left, Modifiers::SUPER), "\x1b[1;9D"); // ⌘← line start
        assert_eq!(seq(KeyCode::Right, Modifiers::SUPER), "\x1b[1;9C"); // ⌘→ line end
        assert_eq!(seq(KeyCode::Right, Modifiers::SHIFT | Modifiers::SUPER), "\x1b[1;10C"); // ⇧⌘→ select to end
        assert_eq!(seq(KeyCode::Up, Modifiers::SUPER), "\x1b[1;9A");
    }

    #[test]
    fn modified_edit_keys_encode_too() {
        assert_eq!(seq(KeyCode::Home, Modifiers::SUPER), "\x1b[1;9H");
        assert_eq!(seq(KeyCode::End, Modifiers::SHIFT), "\x1b[1;2F");
        assert_eq!(seq(KeyCode::Delete, Modifiers::SHIFT), "\x1b[3;2~");
        assert_eq!(seq(KeyCode::PageUp, Modifiers::ALT), "\x1b[5;3~");
        assert_eq!(seq(KeyCode::Tab, Modifiers::SHIFT), "\x1b[Z"); // back-tab
        assert_eq!(seq(KeyCode::Backspace, Modifiers::ALT), "\x1b\x7f"); // ⌥⌫ kill word
        assert_eq!(seq(KeyCode::Backspace, Modifiers::SUPER), "\x1b[127;9u"); // ⌘⌫ kill to line start
        // Shift/Ctrl backspace stay the plain DEL byte — no surprise rebinds.
        assert_eq!(seq(KeyCode::Backspace, Modifiers::SHIFT), "\x7f");
    }

    #[test]
    fn control_letters_still_become_control_bytes() {
        assert_eq!(enc(KeyCode::C, Modifiers::CONTROL), Some(vec![3]));
        assert_eq!(enc(KeyCode::Space, Modifiers::CONTROL), Some(vec![0]));
        assert_eq!(enc(KeyCode::BracketLeft, Modifiers::CONTROL), Some(vec![27]));
        // and a ctrl+arrow is an arrow sequence, not a (nonexistent) ctrl byte
        assert_eq!(seq(KeyCode::Right, Modifiers::CONTROL), "\x1b[1;5C");
    }
}
