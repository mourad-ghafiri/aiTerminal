//! Themes — the token model, and what happens when one is missing.
//!
//! Pure: themes are built in memory or round-tripped through TOML. The resolution
//! scenarios use a scratch directory, never `$HOME`.

use corelib::theme::Theme;
use corelib::wire::Toml;

use super::super::world::{self, World};

pub struct ThemeWorld {
    theme: Theme,
    /// The most recent serialization, for the round-trip.
    serialized: String,
    dir: std::path::PathBuf,
}

pub fn build(_setup: &Toml) -> Result<Box<dyn World>, String> {
    let dir = std::env::temp_dir().join(format!("tt-scenario-theme-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(Box::new(ThemeWorld { theme: corelib::theme::midnight(), serialized: String::new(), dir }))
}

impl World for ThemeWorld {
    fn apply(&mut self, step: &Toml) -> Result<(), String> {
        if let Some(name) = world::text(step, "builtin") {
            self.theme = corelib::theme::collection()
                .into_iter()
                .find(|t| crate::theme::slug(&t.name) == crate::theme::slug(&name))
                .ok_or_else(|| format!("no bundled theme called {name:?}"))?;
            return Ok(());
        }
        if world::flag(step, "serialize") == Some(true) {
            self.serialized = self.theme.to_toml();
            return Ok(());
        }
        if world::flag(step, "reload") == Some(true) {
            self.theme = Theme::from_toml(&self.serialized).map_err(|e| format!("the theme did not reload: {e}"))?;
            return Ok(());
        }
        if let Some(lines) = world::list(step, "theme_file") {
            let name = world::text(step, "name").ok_or("theme_file needs a `name`")?;
            std::fs::write(self.dir.join(format!("{name}.toml")), lines.join("\n")).map_err(|e| e.to_string())?;
            return Ok(());
        }
        if let Some(name) = world::text(step, "resolve") {
            self.theme = crate::theme::resolve(&self.dir, &name);
            return Ok(());
        }
        if world::flag(step, "write_collection") == Some(true) {
            crate::theme::write_collection(&self.dir).map_err(|e| e.to_string())?;
            return Ok(());
        }

        if let Some(want) = world::text(step, "expect_name") {
            return world::expect_eq(&self.theme.name, &want, "the theme's name");
        }
        if let Some(want) = world::flag(step, "expect_dark") {
            if self.theme.is_dark != want {
                return Err(format!("`is_dark` is {}, expected {want}", self.theme.is_dark));
            }
            return Ok(());
        }
        if let Some(want) = world::text(step, "expect_token") {
            let token = world::text(step, "token").ok_or("expect_token needs a `token`")?;
            let got = self.token(&token)?;
            return world::expect_eq(&got, &want, &format!("token `{token}`"));
        }
        if world::flag(step, "expect_round_trip") == Some(true) {
            let again = self.theme.to_toml();
            if again != self.serialized {
                return Err("the theme changed when it was written, reloaded and written again".into());
            }
            return Ok(());
        }
        if let Some(want) = world::list(step, "expect_names") {
            return world::expect_lines(&crate::theme::names(&self.dir), &want, "the themes on disk");
        }
        if let Some(want) = world::text(step, "expect_slug") {
            let of = world::text(step, "of").ok_or("expect_slug needs an `of`")?;
            return world::expect_eq(&crate::theme::slug(&of), &want, &format!("the slug of {of:?}"));
        }
        if let Some(want) = world::int(step, "expect_collection_at_least") {
            let got = corelib::theme::collection().len() as i64;
            if got < want {
                return Err(format!("only {got} bundled theme(s) — expected at least {want}"));
            }
            return Ok(());
        }

        Err(world::unknown_verb(step))
    }
}

impl ThemeWorld {
    fn token(&self, name: &str) -> Result<String, String> {
        let t = &self.theme;
        let c = match name {
            "bg" => t.bg,
            "fg" => t.fg,
            "accent" => t.accent,
            "muted" => t.muted,
            "success" => t.success,
            "warn" => t.warn,
            "error" => t.error,
            "term_bg" => t.term_bg,
            "term_fg" => t.term_fg,
            "cursor" => t.cursor,
            "surface" => t.surface,
            "border" => t.border(),
            "accent2" => t.accent2(),
            "ansi1" => t.ansi(1),
            "ansi2" => t.ansi(2),
            other => return Err(format!("no such theme token: {other:?}")),
        };
        Ok(format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b))
    }
}
