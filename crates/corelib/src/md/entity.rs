//! HTML entities and `:emoji:` shortcodes — the two substitutions GitHub applies to plain
//! text before anything else sees it.
//!
//! Both are small, closed tables rather than a general parser: a README uses a couple of
//! dozen entities and a familiar handful of shortcodes, and an unknown name is left exactly
//! as written (`&foo;` stays `&foo;`), which is also what GitHub does.

/// The named entities that actually appear in prose and READMEs.
const NAMED: &[(&str, &str)] = &[
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
    ("nbsp", "\u{a0}"),
    ("copy", "©"),
    ("reg", "®"),
    ("trade", "™"),
    ("hellip", "…"),
    ("mdash", "—"),
    ("ndash", "–"),
    ("lsquo", "‘"),
    ("rsquo", "’"),
    ("ldquo", "“"),
    ("rdquo", "”"),
    ("laquo", "«"),
    ("raquo", "»"),
    ("times", "×"),
    ("divide", "÷"),
    ("plusmn", "±"),
    ("deg", "°"),
    ("micro", "µ"),
    ("para", "¶"),
    ("sect", "§"),
    ("dagger", "†"),
    ("bull", "•"),
    ("middot", "·"),
    ("larr", "←"),
    ("rarr", "→"),
    ("uarr", "↑"),
    ("darr", "↓"),
    ("harr", "↔"),
    ("check", "✓"),
    ("cross", "✗"),
    ("star", "★"),
    ("euro", "€"),
    ("pound", "£"),
    ("yen", "¥"),
    ("cent", "¢"),
    ("frac12", "½"),
    ("frac14", "¼"),
    ("frac34", "¾"),
    ("infin", "∞"),
    ("ne", "≠"),
    ("le", "≤"),
    ("ge", "≥"),
    ("asymp", "≈"),
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("pi", "π"),
    ("sigma", "σ"),
    ("omega", "ω"),
    ("Delta", "Δ"),
    ("Omega", "Ω"),
    ("ensp", " "),
    ("emsp", " "),
    ("thinsp", " "),
    ("shy", ""),
    ("zwj", "\u{200d}"),
    ("zwnj", "\u{200c}"),
];

/// The shortcodes a README or an AI answer actually reaches for.
const EMOJI: &[(&str, &str)] = &[
    ("smile", "😄"), ("grin", "😁"), ("joy", "😂"), ("wink", "😉"), ("thinking", "🤔"),
    ("sunglasses", "😎"), ("cry", "😢"), ("scream", "😱"), ("tada", "🎉"), ("rocket", "🚀"),
    ("fire", "🔥"), ("sparkles", "✨"), ("star", "⭐"), ("star2", "🌟"), ("zap", "⚡"),
    ("boom", "💥"), ("bulb", "💡"), ("wrench", "🔧"), ("hammer", "🔨"), ("gear", "⚙️"),
    ("package", "📦"), ("books", "📚"), ("book", "📖"), ("memo", "📝"), ("pencil", "✏️"),
    ("clipboard", "📋"), ("chart", "📊"), ("bar_chart", "📊"), ("calendar", "📅"), ("clock", "🕐"),
    ("hourglass", "⏳"), ("lock", "🔒"), ("unlock", "🔓"), ("key", "🔑"), ("shield", "🛡️"),
    ("mag", "🔍"), ("bell", "🔔"), ("mute", "🔇"), ("computer", "💻"), ("desktop", "🖥️"),
    ("phone", "📱"), ("camera", "📷"), ("video_camera", "📹"), ("movie_camera", "🎬"), ("art", "🎨"),
    ("paintbrush", "🖌️"), ("mag_right", "🔎"), ("link", "🔗"), ("paperclip", "📎"), ("pushpin", "📌"),
    ("round_pushpin", "📍"), ("triangular_flag_on_post", "🚩"), ("checkered_flag", "🏁"),
    ("white_check_mark", "✅"), ("heavy_check_mark", "✔️"), ("x", "❌"), ("negative_squared_cross_mark", "❎"),
    ("warning", "⚠️"), ("no_entry", "⛔"), ("stop_sign", "🛑"), ("question", "❓"), ("exclamation", "❗"),
    ("bangbang", "‼️"), ("information_source", "ℹ️"), ("recycle", "♻️"), ("repeat", "🔁"), ("arrows_counterclockwise", "🔄"),
    ("arrow_right", "➡️"), ("arrow_left", "⬅️"), ("arrow_up", "⬆️"), ("arrow_down", "⬇️"),
    ("point_right", "👉"), ("point_left", "👈"), ("point_up", "👆"), ("point_down", "👇"),
    ("thumbsup", "👍"), ("+1", "👍"), ("thumbsdown", "👎"), ("-1", "👎"), ("clap", "👏"),
    ("wave", "👋"), ("pray", "🙏"), ("muscle", "💪"), ("eyes", "👀"), ("brain", "🧠"),
    ("heart", "❤️"), ("broken_heart", "💔"), ("sparkling_heart", "💖"), ("100", "💯"),
    ("bug", "🐛"), ("beetle", "🪲"), ("ant", "🐜"), ("snake", "🐍"), ("whale", "🐳"),
    ("dolphin", "🐬"), ("penguin", "🐧"), ("cat", "🐱"), ("dog", "🐶"), ("crab", "🦀"),
    ("coffee", "☕"), ("beer", "🍺"), ("pizza", "🍕"), ("cake", "🍰"), ("apple", "🍎"),
    ("seedling", "🌱"), ("evergreen_tree", "🌲"), ("earth_africa", "🌍"), ("sun", "☀️"), ("cloud", "☁️"),
    ("snowflake", "❄️"), ("umbrella", "☔"), ("rainbow", "🌈"), ("moon", "🌙"), ("comet", "☄️"),
    ("construction", "🚧"), ("truck", "🚚"), ("airplane", "✈️"), ("ship", "🚢"), ("car", "🚗"),
    ("house", "🏠"), ("office", "🏢"), ("factory", "🏭"), ("hospital", "🏥"), ("bank", "🏦"),
    ("trophy", "🏆"), ("medal", "🏅"), ("dart", "🎯"), ("game_die", "🎲"), ("crystal_ball", "🔮"),
    ("telescope", "🔭"), ("microscope", "🔬"), ("satellite", "🛰️"), ("battery", "🔋"), ("electric_plug", "🔌"),
    ("floppy_disk", "💾"), ("cd", "💿"), ("file_folder", "📁"), ("open_file_folder", "📂"), ("card_index", "📇"),
    ("inbox_tray", "📥"), ("outbox_tray", "📤"), ("envelope", "✉️"), ("email", "📧"), ("newspaper", "📰"),
    ("scroll", "📜"), ("label", "🏷️"), ("bookmark", "🔖"), ("ledger", "📒"), ("notebook", "📓"),
    ("robot", "🤖"), ("alien", "👽"), ("ghost", "👻"), ("skull", "💀"), ("wave_hand", "🖐️"),
    ("sos", "🆘"), ("new", "🆕"), ("free", "🆓"), ("ok", "🆗", ), ("up", "🆙"),
    ("heavy_plus_sign", "➕"), ("heavy_minus_sign", "➖"), ("heavy_multiplication_x", "✖️"),
    ("hourglass_flowing_sand", "⏳"), ("zzz", "💤"), ("dizzy", "💫"), ("speech_balloon", "💬"),
    ("thought_balloon", "💭"), ("loudspeaker", "📢"), ("mega", "📣"), ("musical_note", "🎵"),
];

/// Decode HTML entities in `s`. Unknown names are left as written.
pub fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'&' {
            let len = utf8_len(b[i]);
            out.push_str(&s[i..(i + len).min(s.len())]);
            i += len;
            continue;
        }
        // An entity is short; anything longer is just an ampersand in prose.
        match s[i + 1..].find(';').filter(|&p| p <= 10) {
            Some(p) => {
                let name = &s[i + 1..i + 1 + p];
                match resolve(name) {
                    Some(text) => {
                        out.push_str(&text);
                        i += p + 2;
                    }
                    None => {
                        out.push('&');
                        i += 1;
                    }
                }
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

/// One entity name (without `&` or `;`) → its text.
fn resolve(name: &str) -> Option<String> {
    if let Some(rest) = name.strip_prefix('#') {
        let code = match rest.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => rest.parse::<u32>().ok()?,
        };
        return char::from_u32(code).map(String::from);
    }
    NAMED.iter().find(|(n, _)| *n == name).map(|(_, v)| (*v).to_string())
}

/// `:rocket:` → 🚀. `None` for an unknown shortcode, which stays as written.
pub fn emoji(name: &str) -> Option<&'static str> {
    EMOJI.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

/// Replace every known `:shortcode:` in `s`.
pub fn emojify(s: &str) -> String {
    if !s.contains(':') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b':' {
            // A shortcode is `:word:` — letters, digits, `_`, `+`, `-`.
            let rest = &s[i + 1..];
            let end = rest.find(':').filter(|&p| p > 0 && p <= 32);
            if let Some(p) = end {
                let name = &rest[..p];
                if name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')) {
                    if let Some(e) = emoji(name) {
                        out.push_str(e);
                        i += p + 2;
                        continue;
                    }
                }
            }
        }
        let len = utf8_len(b[i]);
        out.push_str(&s[i..(i + len).min(s.len())]);
        i += len;
    }
    out
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_numeric_and_hex_entities() {
        assert_eq!(decode("a &amp; b"), "a & b");
        assert_eq!(decode("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode("&#65;&#x42;"), "AB");
        assert_eq!(decode("&mdash;"), "—");
    }

    #[test]
    fn unknown_entities_are_left_alone() {
        assert_eq!(decode("&nope; &"), "&nope; &");
        assert_eq!(decode("Tom & Jerry"), "Tom & Jerry");
        assert_eq!(decode("&verylongentityname;"), "&verylongentityname;");
    }

    #[test]
    fn shortcodes_become_emoji() {
        assert_eq!(emojify("ship it :rocket:"), "ship it 🚀");
        assert_eq!(emojify(":+1: :white_check_mark:"), "👍 ✅");
    }

    #[test]
    fn a_colon_in_prose_survives() {
        assert_eq!(emojify("note: this is 10:30, not a shortcode"), "note: this is 10:30, not a shortcode");
        assert_eq!(emojify(":unknown_thing:"), ":unknown_thing:");
    }

    #[test]
    fn multibyte_text_is_never_split() {
        assert_eq!(decode("héllo &amp; wörld"), "héllo & wörld");
        assert_eq!(emojify("héllo :fire: wörld"), "héllo 🔥 wörld");
    }
}
