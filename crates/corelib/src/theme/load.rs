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
mod tests;
