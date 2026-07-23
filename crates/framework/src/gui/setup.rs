//! GuiApp construction + the pane factory + live config / profile switching.
//!
//! [`PaneFactory`] is the single place that spawns a terminal pane (shell
//! integration + theme colors + security policy baked in). Profile switching is
//! terminal-native: `aiTerminal profile switch <id>` (run in any shell) moves the
//! on-disk active pointer; the frame loop polls it (throttled) and applies the
//! switch live — save the old profile's workspace, reload the config (the new
//! profile's overlay applies), and restore its saved workspace.

use super::*;

/// Builds terminal panes. Holds only construction-time-immutable inputs.
pub(crate) struct PaneFactory {
    config: Config,
    default_zoom: f32,
    dirty: DirtyFlag,
    policy: Arc<crate::security::Policy>,
}

impl PaneFactory {
    pub fn new(config: Config, default_zoom: f32, dirty: DirtyFlag, policy: Arc<crate::security::Policy>) -> Self {
        PaneFactory { config, default_zoom, dirty, policy }
    }

    /// The pane shown at startup — a fresh shell; a spawn failure is fatal (the
    /// window would be empty).
    pub fn initial_pane(&self) -> Pane {
        self.terminal_pane().unwrap_or_else(|e| {
            eprintln!("{}: failed to start shell: {e}", corelib::brand::NAME);
            platform::error!("failed to start shell: {e}");
            platform::log::flush();
            std::process::exit(1);
        })
    }

    /// A fresh terminal pane (the native PTY grid). Prepares shell integration —
    /// plugin aliases + theme-driven file colors + (optional) prompt — from the
    /// active plugins + theme, regenerated per spawn so a new tab reflects the theme.
    pub fn terminal_pane(&self) -> std::io::Result<Pane> {
        self.terminal_pane_at(None, None)
    }

    /// A fresh terminal pane started in `cwd` (when restoring a saved workspace), else
    /// the default login-shell `$HOME`; `restore` is the previous session's text
    /// content, replayed into the buffer above the fresh prompt. Shares all of
    /// [`terminal_pane`](Self::terminal_pane)'s shell-integration setup.
    pub fn terminal_pane_at(&self, cwd: Option<&str>, restore: Option<&str>) -> std::io::Result<Pane> {
        let registry = crate::plugin::load_registry(&self.config);
        let theme = Config::resolve_theme(&self.config.theme);
        let integ = crate::shell::prepare(&self.config, &registry, &theme, &self.config.shell);
        Ok(Pane::terminal(
            Session::spawn(&self.dirty, &self.config.shell, self.policy.clone(), integ, self.config.scrollback, cwd, restore)?,
            self.default_zoom,
        ))
    }
}

impl GuiApp {
    pub fn new(config: Config) -> GuiApp {
        let dirty = DirtyFlag::new();

        // Install the locale catalog FIRST — restored panes translate their marker.
        crate::i18n::install(config.i18n_catalog());

        // Plugin + config contributions (declarative — no code): the keymap and the
        // security policy.
        let registry = crate::plugin::load_registry(&config);
        let keymap = build_keymap(&config, &registry);
        let policy = Arc::new(crate::security::build_policy(&config, &registry));

        let default_zoom = config.zoom;
        let factory = PaneFactory::new(config.clone(), default_zoom, dirty.clone(), policy.clone());
        // Restore the active profile's saved workspace (all tabs/panes), or open a fresh
        // shell when it has none — so a single default profile just opens a terminal
        // while a profile with saved work comes back exactly as it was left.
        let tabs = workspace::startup_tabs(&factory);
        let active_profile = crate::profile::active_id();
        let config_stamp_now = config_stamp(&active_profile);
        let profile_chip = workspace::profile_chip();
        let focus: FocusSignal = Arc::new((Mutex::new(FocusState::default()), std::sync::Condvar::new()));
        let status = Arc::new(Mutex::new(crate::plugin::StatusLine::default()));
        let shared_config = Arc::new(Mutex::new((0u64, config.clone())));
        start_status_worker(status.clone(), focus.clone(), dirty.clone(), shared_config.clone());

        let theme = Config::resolve_theme(&config.theme);
        // The tab-bar orientation restores from the profile's saved workspace (a live
        // Cmd-Alt-T cycle persists there); config is the fallback for a fresh profile.
        let tab_bar = TabBarPos::from_str(
            &workspace::saved_tab_bar(&active_profile).unwrap_or_else(|| config.tab_bar.clone()),
        );
        let base_pt = config.font_size;

        let app = GuiApp {
            tabs,
            factory,
            keymap,
            dirty,
            theme,
            scale: 1.0,
            base_pt,
            cache: None,
            surface: None,
            win_px: (0, 0),
            layout: Vec::new(),
            panes_area: Rect::new(0.0, 0.0, 0.0, 0.0),
            tab_bar,
            tab_rects: Vec::new(),
            dragging: None,
            tab_drag: None,
            last_click: None,
            link_hover: None,
            click_count: 0,
            status,
            focus,
            last_cwd_seq: 0,
            active_profile,
            profile_chip,
            last_profile_check: 0,
            config_stamp: config_stamp_now,
            workspace_dirty: false,
            last_workspace_save: 0,
            last_saved_content: 0,
            frame_chrome: 0,
            pane_stamps: std::collections::HashMap::new(),
            config,
            default_zoom,
            policy,
            switcher: TabSwitcher::new(),
            session_ctx: String::new(),
            session_ctx_gen: 0,
            session_ctx_at: Instant::now() - Duration::from_secs(60),
            shared_config,
        };
        // Seed the focused PTY pid from tab 0.
        app.notify_focus_changed();
        app
    }

    pub(in crate::gui) fn base_px(&self) -> f32 {
        self.base_pt * self.scale as f32
    }

    pub(in crate::gui) fn ensure_cache(&mut self, scale: f64) {
        self.scale = scale;
        if self.cache.is_none() {
            self.cache = Some(GlyphCache::new(platform::os::text_shaper_with(&self.config.font_family)));
        }
    }

    /// A terminal pane for Cmd-T / splits (logs + skips on a spawn failure).
    pub(in crate::gui) fn open_terminal_pane(&self) -> Option<Pane> {
        match self.factory.terminal_pane() {
            Ok(p) => Some(p),
            Err(e) => {
                platform::error!("failed to spawn a shell: {e}");
                None
            }
        }
    }

    /// Apply a freshly-loaded [`Config`] live: theme, fonts, zoom, tab bar, keymap,
    /// security policy, and the pane factory (so NEW panes pick up the changes).
    /// Shared by `Cmd-,` reload and a live profile switch, so the two never drift.
    pub(in crate::gui) fn apply_config(&mut self, new: Config) {
        crate::i18n::install(new.i18n_catalog());
        self.theme = Config::resolve_theme(&new.theme);
        // Rewrite the shared shell colors file so every RUNNING shell recolors its
        // prompt/highlighting/ls at the next prompt (the integration re-sources it).
        if let Err(e) = crate::shell::write_colors_file(&self.theme) {
            platform::warn!("failed to refresh shell colors: {e}");
        }
        self.base_pt = new.font_size;
        self.default_zoom = new.zoom;
        // The tab-bar position is RUNTIME state (cycled live, persisted per profile) —
        // only an actual config-value change overrides it, so a theme switch or any
        // other config edit never snaps it back.
        if new.tab_bar != self.config.tab_bar {
            self.tab_bar = TabBarPos::from_str(&new.tab_bar);
        }
        let registry = crate::plugin::load_registry(&new);
        self.keymap = build_keymap(&new, &registry);
        self.policy = Arc::new(crate::security::build_policy(&new, &registry));
        self.factory = PaneFactory::new(new.clone(), new.zoom, self.dirty.clone(), self.policy.clone());
        // A font-family change needs a fresh glyph cache.
        if new.font_family != self.config.font_family {
            self.cache = None;
            self.ensure_cache(self.scale);
        }
        // Hand the fresh config to the status worker (it rebuilds its plugin
        // registry when the generation moves — segments follow plugin changes live).
        {
            let mut shared = self.shared_config.lock().unwrap_or_else(|e| e.into_inner());
            shared.0 += 1;
            shared.1 = new.clone();
        }
        self.config = new;
        self.relayout();
        self.dirty.set();
    }

    /// Follow external changes live (throttled to ~1 s):
    /// - `@profile switch` moves the on-disk active pointer → swap profile + workspace.
    /// - `@theme <name>` / any edit of the global config or the active profile's
    ///   overlay bumps a config mtime → reload + re-apply (same path as `Cmd-,`).
    pub(in crate::gui) fn follow_external_changes(&mut self) {
        let now = unix_now();
        if now.saturating_sub(self.last_profile_check) < 1 {
            return;
        }
        self.last_profile_check = now;
        let active = crate::profile::active_id();
        if active == self.active_profile {
            // Same profile — did its effective config change on disk?
            let stamp = config_stamp(&self.active_profile);
            if stamp != self.config_stamp {
                self.config_stamp = stamp;
                platform::info!("config changed on disk — reloading");
                self.apply_config(Config::load());
            }
            return;
        }
        platform::info!("profile switch: {} → {active}", self.active_profile);
        // Save the OUTGOING profile's workspace under its id before the pointer moves us.
        self.save_workspace_as(&self.active_profile.clone());
        self.active_profile = active;
        crate::profile::touch(&self.active_profile);
        self.apply_config(Config::load());
        self.tabs = workspace::startup_tabs(&self.factory);
        // The incoming profile's saved tab-bar orientation wins over its config.
        if let Some(pos) = workspace::saved_tab_bar(&self.active_profile) {
            self.tab_bar = TabBarPos::from_str(&pos);
        }
        self.profile_chip = workspace::profile_chip();
        self.config_stamp = config_stamp(&self.active_profile);
        self.workspace_dirty = false;
        self.notify_focus_changed();
        self.relayout();
    }

    /// Persist the active profile's workspace now (quit / close paths).
    pub(in crate::gui) fn save_workspace_now(&mut self) {
        self.save_workspace_as(&self.active_profile.clone());
        self.workspace_dirty = false;
        self.last_workspace_save = unix_now();
        self.last_saved_content = self.content_stamp();
    }

    /// Persist the CURRENT window state (tabs + logical size + tab bar) under `id`.
    fn save_workspace_as(&self, id: &str) {
        // Store the LOGICAL size (points) — device pixels ÷ scale — so the size
        // round-trips through `WindowConfig.logical_size` on any display density.
        let window = (self.win_px.0 > 0 && self.scale > 0.0)
            .then(|| (self.win_px.0 as f32 / self.scale as f32, self.win_px.1 as f32 / self.scale as f32));
        let band = crate::shell::selection_band(&self.theme);
        workspace::save_as(&self.tabs, id, window, self.tab_bar.name(), (band.r, band.g, band.b));
    }

    /// Flag the saved workspace stale; the frame loop's debounced autosave flushes it.
    pub(in crate::gui) fn mark_workspace_dirty(&mut self) {
        self.workspace_dirty = true;
    }

    /// Debounced autosave: a STRUCTURE change (tabs/splits) saves within 5 s; and
    /// even without one, a periodic save every 30 s keeps the persisted pane
    /// CONTENT fresh — so a crash loses at most half a minute of scrollback
    /// (a clean quit always saves exactly what was on screen). An IDLE window
    /// (no content generation moved since the last save) skips the periodic
    /// write entirely — no 30 s content dump + disk write for nothing.
    pub(in crate::gui) fn maybe_autosave_workspace(&mut self) {
        let now = unix_now();
        let elapsed = now.saturating_sub(self.last_workspace_save);
        if self.workspace_dirty && elapsed >= 5 {
            self.save_workspace_now();
            return;
        }
        if elapsed >= 30 {
            let stamp = self.content_stamp();
            if stamp != self.last_saved_content {
                self.save_workspace_now();
            } else {
                // Nothing changed — push the next periodic check out, don't re-stamp.
                self.last_workspace_save = now;
            }
        }
    }

    /// A cheap change stamp over every pane's content: the wrapping sum of all
    /// sessions' `generation()` + `cwd_seq()`. One lock+load per pane — no grid
    /// scan, no styling.
    pub(in crate::gui) fn content_stamp(&self) -> u64 {
        let mut stamp: u64 = 0;
        for tree in self.tabs.iter() {
            for id in tree.pane_ids() {
                if let Some(pane) = tree.get(id) {
                    stamp = stamp
                        .wrapping_add(pane.session.generation())
                        .wrapping_add(pane.session.cwd_seq())
                        .wrapping_add(1);
                }
            }
        }
        stamp
    }
}

/// A change stamp over the files that shape the EFFECTIVE config: the global
/// `config.toml` + the active profile's overlay. Mtime-sum, so a save to either
/// (`@theme`, `@profile`-driven `config_set`, or a hand edit) changes the stamp
/// and the frame loop re-applies live. Cheap: two stats per second.
pub(in crate::gui) fn config_stamp(profile_id: &str) -> u64 {
    let mtime = |p: &std::path::Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    };
    let global = mtime(&Config::path());
    let overlay = crate::profile::config_path(profile_id).map(|p| mtime(&p)).unwrap_or(0);
    global.wrapping_add(overlay)
}

/// Current unix time in seconds (the frame loop's throttle clock).
pub(in crate::gui) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_stamp_moves_when_either_config_file_changes() {
        let (_h, _home) = crate::test_home::lock_home("config-stamp");
        Config::ensure_default();
        let id = crate::profile::active_id();
        let a = config_stamp(&id);
        // Rewrite the profile overlay with a different mtime → the stamp moves.
        let overlay = crate::profile::config_path(&id).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&overlay, "[appearance]\ntheme = \"graphite\"\n").unwrap();
        let b = config_stamp(&id);
        assert_ne!(a, b, "an overlay edit is detected");
        // Touch the GLOBAL config too.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let text = std::fs::read_to_string(Config::path()).unwrap();
        std::fs::write(Config::path(), text).unwrap();
        assert_ne!(b, config_stamp(&id), "a global config edit is detected");
    }
}
