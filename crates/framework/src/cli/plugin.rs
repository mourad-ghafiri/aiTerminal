use std::path::Path;
use crate::cli::style::{muted, reset};

/// `aiTerminal plugin <list|install|enable|disable|remove|info>`.
pub fn plugin(args: &[String]) -> i32 {
    let store = match crate::plugin::store::PluginStore::open_default() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("plugin store error: {e}");
            return 1;
        }
    };
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => {
            // Both sources, because both are running. The bundled plugins load from the
            // registry root and the installed ones from `~/.aiTerminal/plugins/` — listing
            // only the second printed "(none)" on a fresh machine while thirty-one were
            // active, which reads as "you have no plugins".
            let cfg = crate::config::Config::load();
            let registry = crate::plugin::load_registry(&cfg);
            let installed = store.installed();
            let names: Vec<String> = installed.iter().map(|p| p.name.clone()).collect();
            let bundled: Vec<(String, String, String, bool, bool)> =
                registry.loaded().into_iter().filter(|(n, ..)| !names.contains(n)).collect();
            println!("plugins ({} bundled · {} installed):", bundled.len(), installed.len());
            for (name, version, description, _, enabled) in &bundled {
                // Marked from the real state, exactly as the installed rows are. A
                // hardcoded ● reported every bundled plugin as running, including the
                // ones the user had just turned off.
                let mark = if *enabled { "\u{25CF}" } else { "\u{25CB}" };
                println!("  {mark} {name:<18} {version:<8} {description}");
            }
            for p in &installed {
                let mark = if p.enabled { "\u{25CF}" } else { "\u{25CB}" };
                println!("  {mark} {:<18} {:<8} {}  (installed)", p.name, p.version, p.description);
            }
            let (dim, r) = (muted(), reset());
            println!("\n{dim}bundled plugins live in the app; yours go in {}{r}", crate::config::Config::plugins_dir().display());
            println!("{dim}one in full:  @plugin info <name>   \u{b7}  turn one off:  @plugin disable <name>{r}");
            0
        }
        "install" => match args.get(1) {
            Some(path) => match store.install(Path::new(path)) {
                Ok(name) => {
                    println!("installed plugin '{name}' (restart to load)");
                    0
                }
                Err(e) => {
                    eprintln!("install failed: {e}");
                    1
                }
            },
            None => {
                eprintln!("usage: aiTerminal plugin install <path-to.toml | path-to.tplugin>");
                1
            }
        },
        "enable" | "disable" => match args.get(1) {
            Some(name) => {
                let on = sub == "enable";
                // The name has to name a real plugin. `set_enabled` just writes the
                // config list, so `@plugin enable nosuchplugin` cheerfully reported
                // success for a plugin that has never existed — and the typo sat in
                // `[plugins] disabled` doing nothing for as long as it took to notice.
                if let Some(err) = unknown_plugin(&store, name) {
                    eprintln!("{err}");
                    return 1;
                }
                match store.set_enabled(name, on) {
                    Ok(()) => {
                        println!("{} plugin '{name}'", if on { "enabled" } else { "disabled" });
                        0
                    }
                    Err(e) => {
                        eprintln!("failed: {e}");
                        1
                    }
                }
            }
            None => {
                eprintln!("usage: aiTerminal plugin {sub} <name>");
                1
            }
        },
        "remove" => match args.get(1) {
            Some(name) if store.remove(name) => {
                println!("removed plugin '{name}'");
                0
            }
            Some(name) => {
                // A bundled plugin is not missing — it lives in the app bundle and
                // cannot be deleted, only turned off. Saying "not found" about `git`,
                // which is loaded and working, sent people looking for the wrong thing.
                match unknown_plugin(&store, name) {
                    Some(err) => eprintln!("{err}"),
                    None => eprintln!(
                        "plugin '{name}' ships with the app \u{2014} it cannot be removed, only turned off:  @plugin disable {name}"
                    ),
                }
                1
            }
            None => {
                eprintln!("usage: aiTerminal plugin remove <name>");
                1
            }
        },
        "info" => match args.get(1) {
            // Installed first (yours shadows a bundled one of the same name), then the
            // bundled set — `info git` used to say "not installed" about a plugin that was
            // loaded and working.
            Some(name) => match store.installed().into_iter().find(|p| &p.name == name) {
                Some(p) => {
                    println!("{}  v{}\n{}\ninstalled \u{b7} enabled: {}", p.name, p.version, p.description, p.enabled);
                    0
                }
                None => {
                    let cfg = crate::config::Config::load();
                    let registry = crate::plugin::load_registry(&cfg);
                    match registry.loaded().into_iter().find(|(n, ..)| n == name) {
                        Some((n, v, d, _, enabled)) => {
                            println!("{n}  v{v}\n{d}\nbundled with the app \u{b7} enabled: {enabled}");
                            0
                        }
                        None => {
                            let all: Vec<String> = registry.names();
                            let refs: Vec<&str> = all.iter().map(String::as_str).collect();
                            eprintln!("no plugin '{name}'{}", crate::flow::verify::nearest(name, &refs));
                            1
                        }
                    }
                }
            },
            None => {
                eprintln!("usage: aiTerminal plugin info <name>");
                1
            }
        },
        other => {
            eprintln!("unknown subcommand '{other}'. try: list, install, enable, disable, remove, info");
            1
        }
    }
}

/// `Some(message)` when `name` is neither installed nor bundled — the one check the
/// mutating plugin subcommands share, so they can never disagree about what exists.
pub(crate) fn unknown_plugin(store: &crate::plugin::store::PluginStore, name: &str) -> Option<String> {
    if store.installed().iter().any(|p| p.name == name) {
        return None;
    }
    let registry = crate::plugin::load_registry(&crate::config::Config::load());
    if registry.loaded().iter().any(|(n, ..)| n == name) {
        return None;
    }
    let all: Vec<String> = registry.names();
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    Some(format!("no plugin '{name}'{}", crate::flow::verify::nearest(name, &refs)))
}
