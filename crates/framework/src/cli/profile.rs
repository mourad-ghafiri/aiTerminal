// ===== profiles ==============================================================

/// `aiTerminal profile <list|current|create|rename|delete|edit|switch|<id>>` —
/// manage the named terminal profiles (config overlay + saved workspace) entirely
/// from the prompt. `@profile <id>` switches directly; `@profile edit [id]` opens
/// the profile's config overlay in `$EDITOR`. A running window follows switches
/// AND overlay edits live (it polls the pointer + config mtimes each second).
/// Resolve a user-typed profile reference — an exact id, or a display name
/// (case-insensitive) — to the profile id.
fn resolve_profile(word: &str) -> Option<String> {
    crate::profile::list()
        .into_iter()
        .find(|p| p.id == word || p.name.eq_ignore_ascii_case(word))
        .map(|p| p.id)
}

/// Switch to a profile by id-or-name, with the shared success/error reporting.
pub(crate) fn profile_switch(word: &str) -> i32 {
    let Some(id) = resolve_profile(word) else {
        eprintln!("no profile '{word}' — see them with: @profile");
        return 2;
    };
    match crate::profile::set_active(&id) {
        Ok(()) => {
            println!("{}", crate::i18n::translate("profile.switched", &[id]));
            0
        }
        Err(e) => {
            eprintln!("switch failed: {e}");
            1
        }
    }
}

pub fn profile(args: &[String]) -> i32 {
    crate::config::Config::ensure_default();
    crate::i18n::install(crate::config::Config::load().i18n_catalog());
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => {
            let active = crate::profile::active_id();
            let all = crate::profile::list();
            println!("{}", crate::i18n::translate("profile.list_header", &[crate::config::Config::profiles_dir().display().to_string(), all.len().to_string()]));
            for p in all {
                let mark = if p.id == active { "\u{25CF}" } else { "\u{25CB}" };
                println!("  {mark} {} {:<16} ({})", p.emoji, p.name, p.id);
            }
            println!("\n{}", crate::i18n::translate("profile.switch_hint", &[]));
            0
        }
        "current" => {
            let id = crate::profile::active_id();
            println!("{id}");
            0
        }
        "create" => match args.get(1) {
            Some(name) => {
                let emoji = args.get(2).map(String::as_str).unwrap_or("");
                match crate::profile::create(name, emoji) {
                    Ok(p) => {
                        println!("created profile '{}' ({}) — switch with: aiTerminal profile switch {}", p.name, p.id, p.id);
                        println!("its config overlay: {}", crate::profile::config_path(&p.id).unwrap().display());
                        0
                    }
                    Err(e) => {
                        eprintln!("create failed: {e}");
                        1
                    }
                }
            }
            None => {
                eprintln!("usage: aiTerminal profile create <name> [emoji]");
                2
            }
        },
        "rename" => match (args.get(1), args.get(2)) {
            (Some(id), Some(name)) => {
                let emoji = args.get(3).map(String::as_str).unwrap_or("");
                match crate::profile::update(id, name, emoji) {
                    Ok(()) => {
                        println!("renamed profile '{id}'");
                        0
                    }
                    Err(e) => {
                        eprintln!("rename failed: {e}");
                        1
                    }
                }
            }
            _ => {
                eprintln!("usage: aiTerminal profile rename <id> <new-name> [emoji]");
                2
            }
        },
        "delete" => match args.get(1) {
            Some(id) => match crate::profile::delete(id) {
                Ok(()) => {
                    println!("deleted profile '{id}'");
                    0
                }
                Err(e) => {
                    eprintln!("delete failed: {e}");
                    1
                }
            },
            None => {
                eprintln!("usage: aiTerminal profile delete <id>");
                2
            }
        },
        // `@profile edit [id]` — open the profile's config overlay in $EDITOR. The
        // window applies the saved changes live (config-mtime polling), so this IS
        // the profile settings surface: a TOML file in your editor, nothing else.
        "edit" => {
            let id = args.get(1).cloned().unwrap_or_else(crate::profile::active_id);
            let Some(path) = crate::profile::config_path(&id).filter(|p| p.exists()) else {
                eprintln!("no profile '{id}' (list them with: aiTerminal profile list)");
                return 2;
            };
            let editor = std::env::var("EDITOR").ok().filter(|e| !e.trim().is_empty()).unwrap_or_else(|| "vi".into());
            // $EDITOR may carry flags (e.g. "code --wait") — split words.
            let mut parts = editor.split_whitespace();
            let bin = parts.next().unwrap_or("vi").to_string();
            let status = std::process::Command::new(&bin).args(parts).arg(&path).status();
            match status {
                Ok(st) if st.success() => {
                    println!("{}", path.display());
                    println!("saved — a running window applies it within a second");
                    0
                }
                Ok(_) => 1,
                Err(e) => {
                    eprintln!("couldn't launch {bin}: {e}\nedit the file directly: {}", path.display());
                    1
                }
            }
        }
        "switch" => match args.get(1) {
            Some(word) => profile_switch(word),
            None => {
                eprintln!("usage: @profile <id>   (or: @profile switch <id>)");
                2
            }
        },
        // `@profile <id-or-name>` switches directly (the switch verb still works).
        other => {
            if resolve_profile(other).is_none() {
                eprintln!("no profile '{other}'. try: list, current, create, rename, delete, edit — or a profile id/name to switch");
                return 2;
            }
            profile_switch(other)
        }
    }
}
