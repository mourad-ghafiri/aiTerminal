//! Boot-time loaders: build the keymap from config + plugins, read the user
//! keymap files, and start the background status worker. (The plugin registry
//! itself loads UI-free via `plugin::load_registry`; the security policy via
//! `security::build_policy`.)

use super::*;


pub(crate) fn build_keymap(config: &Config, registry: &crate::plugin::PluginRegistry) -> Keymap<Action> {
    let mut km = super::action::default_keymap();
    for kb in registry.keybindings() {
        if let Some(a) = Action::from_name(&kb.action) {
            km.bind_str(&kb.key, a);
        }
    }
    // Loadable keymap files (~/.aiTerminal/keymaps/*.toml) compose over the
    // plugin/app keymaps (drop a file in the keymaps dir).
    for (key, action) in load_keymap_files() {
        if let Some(a) = Action::from_name(&action) {
            km.bind_str(&key, a);
        }
    }
    // Config keybindings win last (user overrides everything).
    for (key, action) in &config.keybindings {
        if let Some(a) = Action::from_name(action) {
            km.bind_str(key, a);
        }
    }
    km
}

/// Read every `~/.aiTerminal/keymaps/*.toml` (sorted) and collect its
/// `[[keybinding]]` (key, action) pairs, composed in file order.
fn load_keymap_files() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(Config::keymaps_dir()) else { return out };
    let mut files: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .collect();
    files.sort();
    for p in files {
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(doc) = corelib::wire::Toml::parse(&text) else { continue };
        out.extend(super::action::keybinding_pairs(&doc));
    }
    out
}


pub(crate) fn start_status_worker(
    status: Arc<Mutex<crate::plugin::StatusLine>>,
    focus: super::FocusSignal,
    dirty: DirtyFlag,
    shared_config: Arc<Mutex<(u64, Config)>>,
) {
    thread::spawn(move || {
        // Rebuild the plugin registry whenever the host publishes a new config
        // generation (profile switch, plugin enable/disable, Cmd-, reload) — the
        // status bar follows live instead of holding the boot-time set forever.
        let (mut gen, cfg) = {
            let g = shared_config.lock().unwrap_or_else(|e| e.into_inner());
            (g.0, g.1.clone())
        };
        let mut registry = crate::plugin::load_registry(&cfg);
        let mut prev: Option<crate::plugin::StatusLine> = None;
        // The `lsof` fallback result, cached per pid with a short TTL — a shell
        // without OSC-7 costs one probe per 5 s, not one per 1 s tick.
        const CWD_CACHE_TTL: Duration = Duration::from_secs(5);
        let mut cwd_cache: Option<(i32, std::path::PathBuf, Instant)> = None;
        let (lock, cvar) = &*focus;
        loop {
            {
                let g = shared_config.lock().unwrap_or_else(|e| e.into_inner());
                if g.0 != gen {
                    gen = g.0;
                    let cfg = g.1.clone();
                    drop(g);
                    registry = crate::plugin::load_registry(&cfg);
                    prev = None; // force one repaint with the new set
                }
            }
            // Event-driven: block until a focus/cwd change wakes us (instant status on tab
            // switch / `cd`), with a 1 s timeout that still refreshes git/clock segments.
            let snapshot = {
                let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                let (guard, _timeout) = cvar
                    .wait_timeout(guard, Duration::from_millis(1000))
                    .unwrap_or_else(|e| e.into_inner());
                guard.clone()
            };
            // Prefer the shell's OSC-7 report (instant, and the REMOTE path/host over SSH);
            // fall back to `lsof` on the local pid, then the process cwd. `host=None` keeps
            // the local hostname.
            let (cwd, host) = match snapshot.cwd {
                Some((host, path)) => {
                    (std::path::PathBuf::from(path), (!host.is_empty()).then_some(host))
                }
                None => {
                    let cwd = if snapshot.pid > 0 {
                        match &cwd_cache {
                            Some((pid, path, at))
                                if *pid == snapshot.pid && at.elapsed() < CWD_CACHE_TTL =>
                            {
                                path.clone()
                            }
                            _ => {
                                let path =
                                    crate::plugin::process_cwd(snapshot.pid, Duration::from_millis(300))
                                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                                cwd_cache = Some((snapshot.pid, path.clone(), Instant::now()));
                                path
                            }
                        }
                    } else {
                        std::env::current_dir().unwrap_or_default()
                    };
                    (cwd, None)
                }
            };
            let ctx = crate::plugin::probe_context_host(&cwd, 0, host);
            let vars = registry.evaluate(&ctx);
            let line = registry.status_line(&vars);
            // Only repaint when the status line actually CHANGED — an idle prompt no longer
            // forces a full-window redraw (it just keeps re-probing). Other dirty sources are
            // independent (store(true)); we never clear a concurrent one.
            if prev.as_ref() != Some(&line) {
                *status.lock().unwrap_or_else(|e| e.into_inner()) = line.clone();
                dirty.set();
                prev = Some(line);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_keymap_composes_defaults_plugins_and_config() {
        // Hermetic: a temp $HOME so no user keymap files leak in.
        let (_h, _home) = crate::test_home::lock_home("boot-keymap");
        let mut config = Config::default();
        // A config [[keybinding]] overrides a default chord (config wins last).
        config.keybindings = vec![("cmd+t".into(), "close_tab".into()), ("ctrl+alt+z".into(), "zoom_pane".into())];
        let registry = crate::plugin::load_registry(&config);
        let km = build_keymap(&config, &registry);
        use corelib::types::Chord;
        assert_eq!(km.lookup(&Chord::parse("cmd+t").unwrap()), Some(&Action::CloseTab), "config override wins over the default new_tab");
        assert_eq!(km.lookup(&Chord::parse("ctrl+alt+z").unwrap()), Some(&Action::ZoomPane), "a new config chord binds");
        assert_eq!(km.lookup(&Chord::parse("cmd+d").unwrap()), Some(&Action::SplitRight), "untouched defaults survive");
    }
}
