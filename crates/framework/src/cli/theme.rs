use crate::cli::style::{accent, reset};

/// `aiTerminal theme [<name> | list | path | export <name>]` — list themes, or
/// SWITCH the active profile's theme (`@theme nord`): the name is validated, the
/// profile's config overlay is updated, and a running window applies it live
/// (it follows config-file changes each second).
pub fn theme(args: &[String]) -> i32 {
    crate::config::Config::ensure_default();
    let cfg = crate::config::Config::load();
    crate::i18n::install(cfg.i18n_catalog());
    match args.first().map(String::as_str) {
        Some("path") => {
            println!("{}", crate::config::Config::themes_dir().display());
            return 0;
        }
        // `theme export <name>` — print the COMPLETE, normalized theme TOML (every token
        // resolved, including the derived depth + file-type colors), so the file is a full
        // editable reference. Curated values are preserved; only missing tokens are filled.
        Some("export") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: aiTerminal theme export <name>");
                return 2;
            };
            // The name has to EXIST. `resolve_theme` falls back to midnight, which is
            // the right thing for a config naming a theme somebody deleted — the window
            // must still open — and the wrong thing here: `@theme export typo` printed a
            // complete, valid Midnight and exited 0, so you saved it, edited it, and
            // wondered why your theme was not your theme.
            let available = crate::config::Config::user_theme_names();
            let Some(canonical) = available.iter().find(|n| n.eq_ignore_ascii_case(name)) else {
                eprintln!("{}", crate::i18n::translate("theme.unknown", &[name.to_string(), available.join(", ")]));
                return 2;
            };
            print!("{}", crate::config::Config::resolve_theme(canonical).to_toml());
            return 0;
        }
        // `theme <name>` (or `theme set <name>`) — switch the active profile's theme.
        Some(word) if word != "list" => {
            let name = if word == "set" {
                match args.get(1) {
                    Some(n) => n.clone(),
                    None => {
                        eprintln!("usage: aiTerminal theme set <name>");
                        return 2;
                    }
                }
            } else {
                word.to_string()
            };
            return theme_set(&name);
        }
        _ => {}
    }
    let active = cfg.theme;
    let user = crate::config::Config::user_theme_names();
    println!("themes in {} ({}):", crate::config::Config::themes_dir().display(), user.len());
    for n in &user {
        let mark = if n.eq_ignore_ascii_case(&active) { "\u{25CF}" } else { "\u{25CB}" };
        // A file that will not parse is named as such, the way `@agent` marks a broken
        // agent — so you find out from the listing rather than from a switch that is
        // refused, or worse, from a window that looks wrong.
        match crate::config::Config::theme_problem(n) {
            Some(why) => println!("  {mark} {n}  {}\u{26a0} {why}{}", accent(), reset()),
            None => println!("  {mark} {n}"),
        }
    }
    println!("\n{}", crate::i18n::translate("theme.switch_hint", &[]));
    0
}

/// Switch the ACTIVE profile's theme (its config overlay — so each profile keeps
/// its own look). The name must exist; a running window follows within a second.
pub(crate) fn theme_set(name: &str) -> i32 {
    let available = crate::config::Config::user_theme_names();
    let Some(canonical) = available.iter().find(|n| n.eq_ignore_ascii_case(name)) else {
        eprintln!("{}", crate::i18n::translate("theme.unknown", &[name.to_string(), available.join(", ")]));
        return 2;
    };
    // Parse it BEFORE switching. `theme::resolve` falls back to a working theme when a
    // file will not parse — right at render time, wrong here: the name went into your
    // profile, the window rendered something else, and nothing said so. Every sibling
    // already refuses what it cannot use (`@flow check` a broken graph, `@plugin` a name
    // that is not a plugin, and this very function an unknown theme); only the malformed
    // file got through.
    if let Some(why) = crate::config::Config::theme_problem(canonical) {
        eprintln!("aiTerminal: theme '{canonical}' {why}");
        eprintln!("  the theme was NOT changed \u{2014} fix the file, or pick another: {}", available.join(", "));
        return 2;
    }
    let active = crate::profile::active_id();
    let rendered = format!("\"{}\"", canonical.replace('\\', "\\\\").replace('"', "\\\""));
    if let Err(e) = crate::profile::config_set(&active, "appearance", "theme", &rendered) {
        eprintln!("aiTerminal: {e}");
        return 1;
    }
    println!("{}", crate::i18n::translate("theme.switched", &[canonical.clone(), active]));
    0
}
