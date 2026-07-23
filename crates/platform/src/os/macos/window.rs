//! NSWindow + CAMetalLayer + a manual NSEvent loop.
//!
//! We do not subclass NSView: instead the loop pumps `NSEvent`s itself and
//! translates them to the normalized `Event` enum, which avoids runtime class
//! creation and method trampolines (smaller, safer FFI surface). keyDown events
//! are consumed (no system beep); all other events are forwarded so window
//! dragging/resizing/close behave normally. GUI is exercised on a real desktop
//! session; the objc shim and the Metal data path are covered by headless tests.

use std::ffi::CStr;
use std::os::raw::c_char;

use crate::traits::{
    Event, EventHandler, Gpu, KeyCode, Modifiers, MouseButton, Platform, Point, RawSurfaceHandle,
    ScrollDelta, ScrollPhase, SurfaceConfig, Window, WindowConfig,
};

use super::metal::{MetalContext, MetalGpu};
use super::objc::{class, nsstring, sel, AutoreleasePool, CGPoint, CGRect, CGSize, Id, Sel, NIL};

/// Run one app-handler call ISOLATED from the event loop. A panic inside it (a bad render,
/// a parser edge case, …) is caught — the global panic hook logs the payload + location —
/// and the frame/event is dropped, so a bug in any single render/parse/event path degrades
/// to a dropped frame instead of aborting the whole app. Requires `panic = "unwind"` (set in
/// the release profile); the next frame rebuilds transient state.
#[inline]
fn guarded(f: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
}

#[cfg(test)]
mod guard_tests {
    #[test]
    fn guarded_catches_panic_and_continues() {
        // A panic in one "frame" must NOT propagate — the loop keeps running, so a
        // subsequent frame still executes (the app survives a bad frame).
        let mut after = false;
        super::guarded(|| panic!("simulated render panic"));
        super::guarded(|| after = true);
        assert!(after, "the event loop must continue after a caught panic");
    }
}

// NSWindowStyleMask
const STYLE_TITLED: usize = 1;
const STYLE_CLOSABLE: usize = 2;
const STYLE_MINIATURIZABLE: usize = 4;
const STYLE_RESIZABLE: usize = 8;
const BACKING_BUFFERED: usize = 2;
const PIXEL_FORMAT_BGRA8UNORM: usize = 80;

// NSEventType
const ET_LEFT_DOWN: u64 = 1;
const ET_LEFT_UP: u64 = 2;
const ET_RIGHT_DOWN: u64 = 3;
const ET_RIGHT_UP: u64 = 4;
const ET_MOUSE_MOVED: u64 = 5;
const ET_LEFT_DRAG: u64 = 6;
const ET_RIGHT_DRAG: u64 = 7;
const ET_KEY_DOWN: u64 = 10;
const ET_SCROLL: u64 = 22;
const ET_OTHER_DOWN: u64 = 25;
const ET_OTHER_UP: u64 = 26;
const ET_OTHER_DRAG: u64 = 27;

// NSEventModifierFlag bits
const MOD_SHIFT: u64 = 1 << 17;
const MOD_CONTROL: u64 = 1 << 18;
const MOD_OPTION: u64 = 1 << 19;
const MOD_COMMAND: u64 = 1 << 20;

// Hardware key codes (layout-independent)
const KC_RETURN: u16 = 36;
const KC_TAB: u16 = 48;
const KC_DELETE: u16 = 51; // backspace
const KC_ESCAPE: u16 = 53;
const KC_FWD_DELETE: u16 = 117;
const KC_HOME: u16 = 115;
const KC_END: u16 = 119;
const KC_PAGEUP: u16 = 116;
const KC_PAGEDOWN: u16 = 121;
const KC_LEFT: u16 = 123;
const KC_RIGHT: u16 = 124;
const KC_DOWN: u16 = 125;
const KC_UP: u16 = 126;

/// The macOS platform.
pub struct MacPlatform;

pub fn boot() -> Box<dyn Platform> {
    Box::new(MacPlatform)
}

/// The NSApplication singleton, published once the run loop owns it — the target
/// for [`post_wake_event`]. `0` until `run()` starts (wakes before that no-op:
/// there is no blocked loop to wake yet). NSApp lives for the whole process, so
/// the raw pointer never dangles.
static APP_FOR_WAKE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

// NSEventTypeApplicationDefined
const ET_APP_DEFINED: u64 = 15;

/// Unblock the run loop's `nextEventMatchingMask:untilDate:` wait from ANY thread
/// by posting an `NSApplicationDefined` event (the `glfwPostEmptyEvent` pattern).
/// This is how a background producer (PTY reader, status worker) turns "the frame
/// is dirty" into an immediate render instead of waiting out the idle tick.
pub fn post_wake_event() {
    let app = APP_FOR_WAKE.load(std::sync::atomic::Ordering::Acquire) as Id;
    if app.is_null() {
        return;
    }
    // SAFETY: NSEvent construction and postEvent: for application-defined events
    // are documented thread-safe (the standard cross-thread runloop wake); `app`
    // is the process-lifetime NSApp singleton.
    unsafe {
        let _pool = AutoreleasePool::new();
        let event: Id = msg_send![Id; class("NSEvent"),
            sel("otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:"),
            ET_APP_DEFINED => u64,
            CGPoint { x: 0.0, y: 0.0 } => CGPoint,
            0u64 => u64,
            0.0f64 => f64,
            0isize => isize,
            NIL => Id,
            0i16 => i16,
            0isize => isize,
            0isize => isize];
        if !event.is_null() {
            msg_send![(); app, sel("postEvent:atStart:"), event => Id, true => bool];
        }
    }
}

/// A live NSWindow + its CAMetalLayer.
struct MacWindow {
    ns_window: Id,
    layer: Id,
}

impl Window for MacWindow {
    fn scale_factor(&self) -> f64 {
        // SAFETY: valid NSWindow.
        unsafe { msg_send![f64; self.ns_window, sel("backingScaleFactor")] }
    }
    fn size_px(&self) -> (u32, u32) {
        // SAFETY: valid CAMetalLayer.
        let s: CGSize = unsafe { msg_send![CGSize; self.layer, sel("drawableSize")] };
        (s.width.max(1.0) as u32, s.height.max(1.0) as u32)
    }
    fn request_redraw(&self) {
        post_wake_event();
    }
    fn set_title(&self, title: &str) {
        // SAFETY: setTitle: with an NSString.
        unsafe { msg_send![(); self.ns_window, sel("setTitle:"), nsstring(title) => Id] }
    }
    fn raw_surface(&self) -> RawSurfaceHandle {
        RawSurfaceHandle::Metal { layer: self.layer }
    }
}

fn translate_mods(flags: u64) -> Modifiers {
    let mut m = Modifiers::empty();
    m.set(Modifiers::SHIFT, flags & MOD_SHIFT != 0);
    m.set(Modifiers::CONTROL, flags & MOD_CONTROL != 0);
    m.set(Modifiers::ALT, flags & MOD_OPTION != 0);
    m.set(Modifiers::SUPER, flags & MOD_COMMAND != 0);
    m
}

/// Map a hardware nav/edit key to a `KeyCode`; `None` for ordinary text keys
/// (those flow through `characters` as `TextInput`).
fn nav_keycode(kc: u16) -> Option<KeyCode> {
    Some(match kc {
        KC_LEFT => KeyCode::Left,
        KC_RIGHT => KeyCode::Right,
        KC_UP => KeyCode::Up,
        KC_DOWN => KeyCode::Down,
        KC_HOME => KeyCode::Home,
        KC_END => KeyCode::End,
        KC_PAGEUP => KeyCode::PageUp,
        KC_PAGEDOWN => KeyCode::PageDown,
        KC_FWD_DELETE => KeyCode::Delete,
        KC_RETURN => KeyCode::Enter,
        KC_TAB => KeyCode::Tab,
        KC_DELETE => KeyCode::Backspace,
        KC_ESCAPE => KeyCode::Escape,
        _ => return None,
    })
}

/// Read `[event characters]` as a Rust String. SAFETY: `event` is a valid
/// NSEvent; the UTF8String pointer is copied before the autorelease pool drains.
unsafe fn event_characters(event: Id) -> String {
    let ns: Id = msg_send![Id; event, sel("characters")];
    if ns.is_null() {
        return String::new();
    }
    let cstr: *const c_char = msg_send![*const c_char; ns, sel("UTF8String")];
    if cstr.is_null() {
        return String::new();
    }
    CStr::from_ptr(cstr).to_string_lossy().into_owned()
}

/// Read `[event charactersIgnoringModifiers]` — the character the key produces in the
/// active layout, ignoring Cmd/Ctrl/Alt but honoring Shift. Used to derive a
/// layout-correct [`KeyCode`] so a shortcut matches the *keycap*, not a US-QWERTY
/// physical position. SAFETY: as [`event_characters`].
unsafe fn event_chars_ignoring(event: Id) -> String {
    let ns: Id = msg_send![Id; event, sel("charactersIgnoringModifiers")];
    if ns.is_null() {
        return String::new();
    }
    let cstr: *const c_char = msg_send![*const c_char; ns, sel("UTF8String")];
    if cstr.is_null() {
        return String::new();
    }
    CStr::from_ptr(cstr).to_string_lossy().into_owned()
}

/// Map a layout-produced character (already lowercased) to a [`KeyCode`].
fn char_to_keycode(c: char) -> Option<KeyCode> {
    use KeyCode::*;
    Some(match c {
        'a' => A, 'b' => B, 'c' => C, 'd' => D, 'e' => E, 'f' => F, 'g' => G,
        'h' => H, 'i' => I, 'j' => J, 'k' => K, 'l' => L, 'm' => M, 'n' => N,
        'o' => O, 'p' => P, 'q' => Q, 'r' => R, 's' => S, 't' => T, 'u' => U,
        'v' => V, 'w' => W, 'x' => X, 'y' => Y, 'z' => Z,
        '0' => Digit0, '1' => Digit1, '2' => Digit2, '3' => Digit3, '4' => Digit4,
        '5' => Digit5, '6' => Digit6, '7' => Digit7, '8' => Digit8, '9' => Digit9,
        '-' => Minus, '=' => Equal, '[' => BracketLeft, ']' => BracketRight,
        '\\' => Backslash, ';' => Semicolon, '\'' => Quote, '`' => Backquote,
        ',' => Comma, '.' => Period, '/' => Slash, ' ' => Space,
        _ => return None,
    })
}

/// Derive a layout-correct [`KeyCode`] for a key event. Nav/function/editing keys are
/// layout-independent (from the scancode); every other key resolves from the character
/// it produces in the active layout, so `Cmd+Shift+M` matches the M keycap on AZERTY,
/// QWERTY, … Dead / unmapped keys fall back to the US scancode table.
unsafe fn keycode_from_event(kc: u16, event: Id) -> KeyCode {
    if let Some(k) = nav_keycode(kc) {
        return k;
    }
    if let Some(c) = event_chars_ignoring(event).chars().next() {
        if let Some(k) = char_to_keycode(c.to_ascii_lowercase()) {
            return k;
        }
    }
    keycode_from_hw(kc)
}

/// US-QWERTY scancode → [`KeyCode`] fallback for dead/unmapped character keys (the
/// layout-correct path is [`keycode_from_event`]).
fn keycode_from_hw(kc: u16) -> KeyCode {
    use KeyCode::*;
    match kc {
        0 => A, 1 => S, 2 => D, 3 => F, 4 => H, 5 => G, 6 => Z, 7 => X, 8 => C,
        9 => V, 11 => B, 12 => Q, 13 => W, 14 => E, 15 => R, 16 => Y, 17 => T,
        18 => Digit1, 19 => Digit2, 20 => Digit3, 21 => Digit4, 22 => Digit6,
        23 => Digit5, 24 => Equal, 25 => Digit9, 26 => Digit7, 27 => Minus,
        28 => Digit8, 29 => Digit0, 30 => BracketRight, 31 => O, 32 => U,
        33 => BracketLeft, 34 => I, 35 => P, 36 => Enter, 37 => L, 38 => J,
        39 => Quote, 40 => K, 41 => Semicolon, 42 => Backslash, 43 => Comma,
        44 => Slash, 45 => N, 46 => M, 47 => Period, 48 => Tab, 49 => Space,
        50 => Backquote, 51 => Backspace, 53 => Escape,
        115 => Home, 116 => PageUp, 117 => Delete, 119 => End, 121 => PageDown,
        123 => Left, 124 => Right, 125 => Down, 126 => Up,
        _ => Unidentified,
    }
}

/// Mouse position (logical points, top-left origin) + active modifiers.
unsafe fn mouse_pos_mods(event: Id, view: Id) -> (Point, Modifiers) {
    let loc: CGPoint = msg_send![CGPoint; event, sel("locationInWindow")];
    let b: CGRect = msg_send![CGRect; view, sel("bounds")];
    let pos = Point::new(loc.x as f32, (b.size.height - loc.y) as f32);
    let flags: u64 = msg_send![u64; event, sel("modifierFlags")];
    (pos, translate_mods(flags))
}

/// Translate one NSEvent into our `Event`(s) and forward to the handler. Mouse
/// A new `NSMenu` with the given title.
unsafe fn nsmenu(title: &str) -> Id {
    let m: Id = msg_send![Id; class("NSMenu"), sel("alloc")];
    msg_send![Id; m, sel("initWithTitle:"), nsstring(title) => Id]
}

/// Append an item to `menu` and return it. `target = nil`, so the `action`
/// selector travels the responder chain (to NSApp / the key window).
unsafe fn menu_item(menu: Id, title: &str, action: Sel, key: &str) -> Id {
    let it: Id = msg_send![Id; class("NSMenuItem"), sel("alloc")];
    let it: Id = msg_send![Id; it, sel("initWithTitle:action:keyEquivalent:"),
        nsstring(title) => Id, action => Sel, nsstring(key) => Id];
    msg_send![(); menu, sel("addItem:"), it => Id];
    it
}

unsafe fn menu_separator(menu: Id) {
    let sep: Id = msg_send![Id; class("NSMenuItem"), sel("separatorItem")];
    msg_send![(); menu, sel("addItem:"), sep => Id];
}

/// Attach `menu` as a top-level submenu of `main` (via an empty holder item).
unsafe fn add_submenu(main: Id, menu: Id) {
    let holder: Id = msg_send![Id; class("NSMenuItem"), sel("alloc")];
    let holder: Id = msg_send![Id; holder, sel("init")];
    msg_send![(); holder, sel("setSubmenu:"), menu => Id];
    msg_send![(); main, sel("addItem:"), holder => Id];
}

/// Install the standard menu bar (App + Window menus). Items use built-in
/// responder selectors — `terminate:` / `hide:` / `performMiniaturize:` / … — that
/// NSApp or the key window already implement, so no custom NSView methods are
/// needed. Clicks work via the forwarded mouse events; the Hide/Minimize keyboard
/// chords are routed to the menu from `dispatch`.
unsafe fn install_menu_bar(app: Id) {
    let main = nsmenu("");

    let name = corelib::brand::NAME;
    let app_menu = nsmenu(name);
    menu_item(app_menu, &format!("About {name}"), sel("orderFrontStandardAboutPanel:"), "");
    menu_separator(app_menu);
    menu_item(app_menu, &format!("Hide {name}"), sel("hide:"), "h");
    let hide_others = menu_item(app_menu, "Hide Others", sel("hideOtherApplications:"), "h");
    msg_send![(); hide_others, sel("setKeyEquivalentModifierMask:"), MOD_COMMAND | MOD_OPTION => u64];
    menu_item(app_menu, "Show All", sel("unhideAllApplications:"), "");
    menu_separator(app_menu);
    // Quit closes the (single) window, so the run loop's `!isVisible` branch persists the
    // workspace then exits — instead of AppKit's `terminate:`, which bypasses the loop (and the
    // save). ⌘Q has no menu key-equivalent: `dispatch` owns it and also delivers `CloseRequested`.
    menu_item(app_menu, &format!("Quit {name}"), sel("performClose:"), "");
    add_submenu(main, app_menu);

    let win_menu = nsmenu("Window");
    menu_item(win_menu, "Minimize", sel("performMiniaturize:"), "m");
    menu_item(win_menu, "Zoom", sel("performZoom:"), "");
    menu_separator(win_menu);
    // No `cmd+w` key equivalent — the app binds it to Close Tab.
    menu_item(win_menu, "Close", sel("performClose:"), "");
    add_submenu(main, win_menu);
    msg_send![(); app, sel("setWindowsMenu:"), win_menu => Id];

    msg_send![(); app, sel("setMainMenu:"), main => Id];
}

/// events are also passed to AppKit so window chrome (drag/resize/close) works.
unsafe fn dispatch(
    app: Id,
    event: Id,
    view: Id,
    handler: &mut dyn EventHandler,
    win: &MacWindow,
    gpu: &mut dyn Gpu,
) {
    let etype: u64 = msg_send![u64; event, sel("type")];
    match etype {
        ET_KEY_DOWN => {
            let kc: u16 = msg_send![u16; event, sel("keyCode")];
            let flags: u64 = msg_send![u64; event, sel("modifierFlags")];
            let mods = translate_mods(flags);
            let code = keycode_from_event(kc, event);
            // The bare system chords (Cmd alone, by the *logical* key, any layout):
            // Cmd-Q quits; Cmd-H / Cmd-M (Hide / Minimize) go to AppKit. Anything with
            // Shift/Alt/Ctrl (our Cmd+Shift / Cmd+Alt app chords) passes through.
            if mods == Modifiers::SUPER {
                if code == KeyCode::Q {
                    // Cmd-Q: let the app persist its workspace before exit. The handler's
                    // `CloseRequested` arm saves + exits; the trailing exit is a fallback.
                    handler.handle(Event::CloseRequested, win, gpu);
                    std::process::exit(0);
                }
                if matches!(code, KeyCode::H | KeyCode::M) {
                    msg_send![(); app, sel("sendEvent:"), event => Id];
                    return;
                }
            }
            handler.handle(Event::KeyDown { code, mods, repeat: false }, win, gpu);
            // committed text for plain typing (letters/digits/punct/space): NOT
            // for nav/function keys (the app encodes those from KeyDown) and not
            // while Cmd/Ctrl is held (those are chords).
            if nav_keycode(kc).is_none()
                && !mods.contains(Modifiers::SUPER)
                && !mods.contains(Modifiers::CONTROL)
            {
                let text = event_characters(event);
                if !text.is_empty() {
                    handler.handle(Event::TextInput { text }, win, gpu);
                }
            }
        }
        ET_LEFT_DOWN | ET_RIGHT_DOWN | ET_OTHER_DOWN => {
            let (pos, mods) = mouse_pos_mods(event, view);
            let button = match etype {
                ET_LEFT_DOWN => MouseButton::Left,
                ET_RIGHT_DOWN => MouseButton::Right,
                _ => MouseButton::Middle,
            };
            handler.handle(Event::MouseDown { button, pos, mods }, win, gpu);
            msg_send![(); app, sel("sendEvent:"), event => Id];
        }
        ET_LEFT_UP | ET_RIGHT_UP | ET_OTHER_UP => {
            let (pos, mods) = mouse_pos_mods(event, view);
            let button = match etype {
                ET_LEFT_UP => MouseButton::Left,
                ET_RIGHT_UP => MouseButton::Right,
                _ => MouseButton::Middle,
            };
            handler.handle(Event::MouseUp { button, pos, mods }, win, gpu);
            msg_send![(); app, sel("sendEvent:"), event => Id];
        }
        ET_LEFT_DRAG | ET_RIGHT_DRAG | ET_OTHER_DRAG | ET_MOUSE_MOVED => {
            let (pos, mods) = mouse_pos_mods(event, view);
            handler.handle(Event::MouseMove { pos, mods }, win, gpu);
            msg_send![(); app, sel("sendEvent:"), event => Id];
        }
        ET_SCROLL => {
            let (pos, mods) = mouse_pos_mods(event, view);
            let dx: f64 = msg_send![f64; event, sel("scrollingDeltaX")];
            let dy: f64 = msg_send![f64; event, sel("scrollingDeltaY")];
            let precise: bool = msg_send![bool; event, sel("hasPreciseScrollingDeltas")];
            let delta = if precise {
                ScrollDelta::Pixels { x: dx as f32, y: dy as f32 }
            } else {
                ScrollDelta::Lines { x: dx as f32, y: dy as f32 }
            };
            handler.handle(Event::Scroll { delta, phase: ScrollPhase::Wheel, pos, mods }, win, gpu);
        }
        _ => {
            msg_send![(); app, sel("sendEvent:"), event => Id];
        }
    }
}

impl Platform for MacPlatform {
    fn run(self: Box<Self>, cfg: WindowConfig, mut handler: Box<dyn EventHandler>) -> ! {
        let _pool = AutoreleasePool::new();
        // SAFETY: standard AppKit setup; all selectors below are typed at the
        // call site by msg_send!.
        unsafe {
            let app: Id = msg_send![Id; class("NSApplication"), sel("sharedApplication")];
            APP_FOR_WAKE.store(app as usize, std::sync::atomic::Ordering::Release);
            msg_send![(); app, sel("setActivationPolicy:"), 0isize => isize]; // Regular
            install_menu_bar(app); // a native menu bar (App + Window menus)

            let ctx = MetalContext::new().expect("no Metal device available");
            let device = ctx.device();

            // CAMetalLayer configured for blit-present.
            let layer: Id = msg_send![Id; class("CAMetalLayer"), sel("layer")];
            msg_send![(); layer, sel("setDevice:"), device => Id];
            msg_send![(); layer, sel("setPixelFormat:"), PIXEL_FORMAT_BGRA8UNORM => usize];
            msg_send![(); layer, sel("setFramebufferOnly:"), false => bool]; // allow blit dest

            let rect = CGRect::new(0.0, 0.0, cfg.logical_size.w as f64, cfg.logical_size.h as f64);
            let style = STYLE_TITLED | STYLE_CLOSABLE | STYLE_MINIATURIZABLE | STYLE_RESIZABLE;
            let win_alloc: Id = msg_send![Id; class("NSWindow"), sel("alloc")];
            let window: Id = msg_send![Id; win_alloc,
                sel("initWithContentRect:styleMask:backing:defer:"),
                rect => CGRect, style => usize, BACKING_BUFFERED => usize, false => bool];
            msg_send![(); window, sel("setTitle:"), nsstring(&cfg.title) => Id];

            let view_alloc: Id = msg_send![Id; class("NSView"), sel("alloc")];
            let view: Id = msg_send![Id; view_alloc, sel("initWithFrame:"), rect => CGRect];
            msg_send![(); view, sel("setWantsLayer:"), true => bool];
            msg_send![(); view, sel("setLayer:"), layer => Id];
            msg_send![(); window, sel("setContentView:"), view => Id];
            msg_send![(); window, sel("makeFirstResponder:"), view => Id];
            // Deliver `mouseMoved:` even with no button held, so ⌘-hover can underline the
            // link under the pointer (AppKit suppresses these events by default).
            msg_send![(); window, sel("setAcceptsMouseMovedEvents:"), true => bool];
            msg_send![(); window, sel("center")];
            msg_send![(); window, sel("makeKeyAndOrderFront:"), NIL => Id];
            msg_send![(); app, sel("activateIgnoringOtherApps:"), true => bool];

            let scale: f64 = msg_send![f64; window, sel("backingScaleFactor")];
            let bounds: CGRect = msg_send![CGRect; view, sel("bounds")];
            let mut dw = (bounds.size.width * scale).max(1.0) as u32;
            let mut dh = (bounds.size.height * scale).max(1.0) as u32;
            msg_send![(); layer, sel("setContentsScale:"), scale => f64];
            msg_send![(); layer, sel("setDrawableSize:"),
                CGSize { width: dw as f64, height: dh as f64 } => CGSize];

            let win = MacWindow { ns_window: window, layer };
            let mut gpu = MetalGpu::new(ctx, layer);

            guarded(|| handler.init(&win, &mut gpu));
            gpu.configure(SurfaceConfig { width_px: dw, height_px: dh, scale });
            guarded(|| handler.handle(Event::Resized { width_px: dw, height_px: dh, scale }, &win, &mut gpu));

            let mode = nsstring("kCFRunLoopDefaultMode");
            let distant_past: Id = msg_send![Id; class("NSDate"), sel("distantPast")];

            // Pacing: a clean, idle window BLOCKS below and only ticks ~1×/s (for the
            // app's coarse timers — autosave, config-follow); input or a background
            // `post_wake_event` unblocks instantly; a flooding producer (a fast PTY)
            // is coalesced to at most ~60 renders/s.
            const MIN_FRAME: f64 = 0.016;
            const IDLE_TICK: f64 = 1.0;
            let mut last_render = std::time::Instant::now();

            loop {
                let _frame_pool = AutoreleasePool::new();

                // Block until input / a wake event / the idle tick. This wait is where
                // an idle terminal spends its life — near-zero CPU.
                let until: Id = msg_send![Id; class("NSDate"),
                    sel("dateWithTimeIntervalSinceNow:"), IDLE_TICK => f64];
                let event: Id = msg_send![Id; app,
                    sel("nextEventMatchingMask:untilDate:inMode:dequeue:"),
                    u64::MAX => u64, until => Id, mode => Id, true => bool];
                if !event.is_null() {
                    guarded(|| dispatch(app, event, view, &mut *handler, &win, &mut gpu));
                }

                // Drain everything else pending without blocking.
                loop {
                    let event: Id = msg_send![Id; app,
                        sel("nextEventMatchingMask:untilDate:inMode:dequeue:"),
                        u64::MAX => u64, distant_past => Id, mode => Id, true => bool];
                    if event.is_null() {
                        break;
                    }
                    guarded(|| dispatch(app, event, view, &mut *handler, &win, &mut gpu));
                }

                // Frame pacing: absorb further events until the next ~60 Hz slot, so a
                // wake-per-read PTY burst becomes ONE render, not one per chunk.
                loop {
                    let remain = MIN_FRAME - last_render.elapsed().as_secs_f64();
                    if remain <= 0.0 {
                        break;
                    }
                    let until: Id = msg_send![Id; class("NSDate"),
                        sel("dateWithTimeIntervalSinceNow:"), remain => f64];
                    let event: Id = msg_send![Id; app,
                        sel("nextEventMatchingMask:untilDate:inMode:dequeue:"),
                        u64::MAX => u64, until => Id, mode => Id, true => bool];
                    if event.is_null() {
                        break;
                    }
                    guarded(|| dispatch(app, event, view, &mut *handler, &win, &mut gpu));
                }

                // Window closed by the user (red button, or menu Quit → performClose:)? Persist
                // the workspace first — the handler's `CloseRequested` arm saves, then exits.
                let visible: bool = msg_send![bool; window, sel("isVisible")];
                if !visible {
                    guarded(|| handler.handle(Event::CloseRequested, &win, &mut gpu));
                    std::process::exit(0);
                }

                // Resize?
                let scale: f64 = msg_send![f64; window, sel("backingScaleFactor")];
                let bounds: CGRect = msg_send![CGRect; view, sel("bounds")];
                let nw = (bounds.size.width * scale).max(1.0) as u32;
                let nh = (bounds.size.height * scale).max(1.0) as u32;
                if nw != dw || nh != dh {
                    dw = nw;
                    dh = nh;
                    msg_send![(); layer, sel("setDrawableSize:"),
                        CGSize { width: dw as f64, height: dh as f64 } => CGSize];
                    gpu.configure(SurfaceConfig { width_px: dw, height_px: dh, scale });
                    guarded(|| handler.handle(Event::Resized { width_px: dw, height_px: dh, scale }, &win, &mut gpu));
                }

                // Render this frame (the handler early-outs when nothing is dirty).
                guarded(|| handler.handle(Event::RedrawRequested, &win, &mut gpu));
                last_render = std::time::Instant::now();
            }
        }
    }
}
