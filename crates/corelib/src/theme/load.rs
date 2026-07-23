//! Theme loading + serialization (declarative TOML). Themes are data files; the
//! only code theme is the default `noir` (see `lib.rs`).

use crate::types::Rgba8;
use crate::wire::Toml;

use crate::theme::Theme;

impl Theme {
    /// Parse a declarative theme from TOML. Unspecified colors fall back to
    /// sensible defaults (`term_bg`→`bg`, `term_fg`→`fg`, `cursor`→`accent`), so
    /// a minimal theme still works.
    /// Serialize this theme to the declarative TOML format `from_toml` reads
    /// (used to write the built-in themes into `~/.aiTerminal/themes/`).
    pub fn to_toml(&self) -> String {
        use std::fmt::Write as _;
        let hex = |c: Rgba8| format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b);
        let hexa = |c: Rgba8| {
            if c.a == 255 {
                hex(c)
            } else {
                format!("#{:02X}{:02X}{:02X}{:02X}", c.r, c.g, c.b, c.a)
            }
        };
        let mut s = String::new();
        let _ = writeln!(s, "name = {:?}", self.name);
        let _ = writeln!(s, "dark = {}\n", self.is_dark);
        for (k, v) in [
            ("bg", self.bg),
            ("surface", self.surface),
            ("fg", self.fg),
            ("muted", self.muted),
            ("accent", self.accent),
            ("success", self.success),
            ("warn", self.warn),
            ("error", self.error),
            ("term_bg", self.term_bg),
            ("term_fg", self.term_fg),
            ("cursor", self.cursor),
        ] {
            let _ = writeln!(s, "{k:<9}= {:?}", hex(v));
        }
        let _ = writeln!(s, "selection= {:?}", hexa(self.selection));
        // Extended depth tokens (resolved values written so the file is editable).
        let _ = writeln!(s, "surface_hover = {:?}", hex(self.surface_hover()));
        let _ = writeln!(s, "accent2  = {:?}", hex(self.accent2()));
        let _ = writeln!(s, "border   = {:?}", hexa(self.border()));
        let _ = writeln!(s, "shadow   = {:?}\n", hexa(self.shadow()));
        let _ = writeln!(s, "[ansi]");
        const NAMES: [&str; 16] = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
            "bright_black", "bright_red", "bright_green", "bright_yellow", "bright_blue",
            "bright_magenta", "bright_cyan", "bright_white",
        ];
        for (i, name) in NAMES.iter().enumerate() {
            let _ = writeln!(s, "{name:<14}= {:?}", hex(self.ansi[i]));
        }
        // Per-file-type `ls` colors (resolved values written so the file is editable).
        let f = self.files();
        let _ = writeln!(s, "\n[files]");
        for (k, v) in [
            ("directory", f.directory),
            ("symlink", f.symlink),
            ("executable", f.executable),
            ("archive", f.archive),
            ("image", f.image),
            ("media", f.media),
            ("document", f.document),
            ("code", f.code),
            ("config", f.config),
            ("hidden", f.hidden),
            ("broken", f.broken),
        ] {
            let _ = writeln!(s, "{k:<10}= {:?}", hex(v));
        }
        s
    }

    pub fn from_toml(text: &str) -> Result<Theme, String> {
        let doc = Toml::parse(text)?;
        let is_dark = doc.get("dark").and_then(|v| v.as_bool()).unwrap_or(true);
        let mut t = crate::theme::base_theme(is_dark);
        t.is_dark = is_dark;
        if let Some(n) = doc.get("name").and_then(|v| v.as_str()) {
            t.name = n.to_string();
        }
        let col = |k: &str, default: Rgba8| {
            doc.get(k).and_then(|v| v.as_str()).and_then(Rgba8::from_hex_str).unwrap_or(default)
        };
        let bg = col("bg", t.bg);
        let fg = col("fg", t.fg);
        t.bg = bg;
        t.fg = fg;
        t.surface = col("surface", t.surface);
        t.muted = col("muted", t.muted);
        t.accent = col("accent", t.accent);
        t.success = col("success", t.success);
        t.warn = col("warn", t.warn);
        t.error = col("error", t.error);
        t.term_bg = col("term_bg", bg);
        t.term_fg = col("term_fg", fg);
        t.cursor = col("cursor", t.accent);
        t.selection = col("selection", t.selection);
        // Extended depth tokens — optional; left `None` to use the derived defaults.
        let opt = |k: &str| doc.get(k).and_then(|v| v.as_str()).and_then(Rgba8::from_hex_str);
        t.surface_hover = opt("surface_hover");
        t.accent2 = opt("accent2");
        t.border = opt("border");
        t.shadow = opt("shadow");

        if let Some(ansi) = doc.get("ansi") {
            const NAMES: [&str; 16] = [
                "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
                "bright_black", "bright_red", "bright_green", "bright_yellow", "bright_blue",
                "bright_magenta", "bright_cyan", "bright_white",
            ];
            for (i, name) in NAMES.iter().enumerate() {
                if let Some(c) = ansi.get(name).and_then(|v| v.as_str()).and_then(Rgba8::from_hex_str) {
                    t.ansi[i] = c;
                }
            }
        }

        // Per-file-type `ls` colors — optional `[files]`, parsed AFTER `[ansi]` so the
        // derived defaults track the theme's own palette; each key overrides one slot.
        if let Some(files) = doc.get("files") {
            let mut fc = t.files(); // derived defaults from the (now-final) ANSI palette
            let fcol = |k: &str, d: Rgba8| files.get(k).and_then(|v| v.as_str()).and_then(Rgba8::from_hex_str).unwrap_or(d);
            fc.directory = fcol("directory", fc.directory);
            fc.symlink = fcol("symlink", fc.symlink);
            fc.executable = fcol("executable", fc.executable);
            fc.archive = fcol("archive", fc.archive);
            fc.image = fcol("image", fc.image);
            fc.media = fcol("media", fc.media);
            fc.document = fcol("document", fc.document);
            fc.code = fcol("code", fc.code);
            fc.config = fcol("config", fc.config);
            fc.hidden = fcol("hidden", fc.hidden);
            fc.broken = fcol("broken", fc.broken);
            t.files = Some(fc);
        }
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::midnight;

    #[test]
    fn to_toml_round_trips() {
        let orig = midnight();
        let parsed = Theme::from_toml(&orig.to_toml()).expect("generated theme parses");
        assert_eq!(parsed.name, orig.name);
        assert_eq!(parsed.is_dark, orig.is_dark);
        assert_eq!(parsed.bg, orig.bg);
        assert_eq!(parsed.accent, orig.accent);
        assert_eq!(parsed.selection, orig.selection); // incl. alpha
        assert_eq!(parsed.ansi, orig.ansi);
    }

    #[test]
    fn from_toml_minimal_with_fallbacks() {
        let t = Theme::from_toml(
            "name = \"X\"\ndark = false\nbg = \"#ffffff\"\nfg = \"#000000\"\naccent = \"#ff0000\"\n",
        )
        .unwrap();
        assert_eq!(t.name, "X");
        assert!(!t.is_dark);
        assert_eq!(t.bg, Rgba8::rgb(255, 255, 255));
        assert_eq!(t.term_bg, Rgba8::rgb(255, 255, 255)); // defaulted to bg
        assert_eq!(t.cursor, Rgba8::rgb(255, 0, 0)); // defaulted to accent
    }

    #[test]
    fn from_toml_ansi_and_alpha_selection() {
        let t = Theme::from_toml("name=\"Y\"\nselection=\"#11223344\"\n[ansi]\nred = \"#abcdef\"\n").unwrap();
        assert_eq!(t.ansi(1), Rgba8::rgb(0xab, 0xcd, 0xef));
        assert_eq!(t.selection, Rgba8::new(0x11, 0x22, 0x33, 0x44));
    }

    #[test]
    fn extended_tokens_derive_when_absent_and_parse_when_present() {
        // Absent → derived (not equal to the base surface; dark shadow alpha). Built from
        // a minimal TOML so the depth tokens are genuinely unset (the collection sets them).
        let n = Theme::from_toml("name=\"D\"\ndark=true\nsurface=\"#161A23\"\n").unwrap();
        assert!(n.surface_hover.is_none() && n.accent2.is_none());
        assert_ne!(n.surface_hover(), n.surface);
        assert_eq!(n.shadow().a, 0x70);
        // Present → parsed, including alpha; resolved through the getter.
        let t = Theme::from_toml("name=\"Z\"\naccent2=\"#ff00ff\"\nborder=\"#11223344\"\n").unwrap();
        assert_eq!(t.accent2(), Rgba8::rgb(0xff, 0x00, 0xff));
        assert_eq!(t.border(), Rgba8::new(0x11, 0x22, 0x33, 0x44));
        // to_toml writes resolved values, so a round-trip keeps them.
        let rt = Theme::from_toml(&t.to_toml()).unwrap();
        assert_eq!(rt.accent2(), t.accent2());
    }

    #[test]
    fn file_colors_derive_and_override() {
        // Absent `[files]` → derived from the ANSI palette.
        let m = midnight();
        assert!(m.files.is_none());
        assert_eq!(m.files().directory, m.ansi[4]); // blue
        assert_eq!(m.files().executable, m.ansi[2]); // green
        // A `[files]` override changes one slot; the rest keep deriving.
        let t = Theme::from_toml("name=\"F\"\n[files]\ndirectory = \"#FF0000\"\n").unwrap();
        assert_eq!(t.files().directory, Rgba8::rgb(0xff, 0, 0));
        assert_eq!(t.files().executable, t.ansi[2]); // untouched → still derived
        // to_toml writes resolved values, so a round-trip keeps the override.
        let rt = Theme::from_toml(&t.to_toml()).unwrap();
        assert_eq!(rt.files().directory, Rgba8::rgb(0xff, 0, 0));
    }

    #[test]
    fn collection_themes_serialize() {
        // Every shipped theme round-trips through TOML (the form themes are shipped +
        // loaded as on disk) and is internally coherent.
        let c = crate::theme::collection();
        assert!(c.len() >= 8, "the theme collection ships several themes");
        assert_eq!(c[0].name, "Midnight"); // the default
        for t in &c {
            assert!(Theme::from_toml(&t.to_toml()).is_ok(), "{} must round-trip", t.name);
        }
    }
}
