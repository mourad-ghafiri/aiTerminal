//! `unicode` — character width and segmentation, the single width truth shared
//! by `term`, `md`, `ui`, and `text`.
//!
//! Phase 0 ships a compact, well-tested approximation of monospace display
//! width (the property a terminal grid needs): control/combining glyphs are
//! zero columns, CJK/wide and most emoji are two columns, everything else one.
//! A later phase replaces the hand-listed ranges with tables generated from a
//! vendored, version-pinned Unicode Character Database (UAX #11 / #29 / #14).
#![forbid(unsafe_code)]

/// Number of terminal columns a scalar occupies: 0, 1, or 2.
pub fn char_width(c: char) -> u8 {
    let cp = c as u32;

    // C0/C1 controls and NUL occupy no columns in a grid.
    if cp == 0 || (cp < 0x20) || (0x7f..0xa0).contains(&cp) {
        return 0;
    }
    if is_zero_width(cp) {
        return 0;
    }
    if is_wide(cp) {
        return 2;
    }
    1
}

/// Display width of a string in monospace columns.
pub fn str_width(s: &str) -> usize {
    s.chars().map(|c| char_width(c) as usize).sum()
}

fn in_ranges(cp: u32, ranges: &[(u32, u32)]) -> bool {
    // ranges are sorted, non-overlapping → binary search.
    ranges
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                core::cmp::Ordering::Greater
            } else if cp > hi {
                core::cmp::Ordering::Less
            } else {
                core::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Combining marks and other zero-advance scalars (approximate).
fn is_zero_width(cp: u32) -> bool {
    const ZERO: &[(u32, u32)] = &[
        (0x0300, 0x036F), // combining diacritical marks
        (0x0483, 0x0489),
        (0x0591, 0x05BD),
        (0x0610, 0x061A),
        (0x064B, 0x065F),
        (0x0670, 0x0670),
        (0x06D6, 0x06DC),
        (0x0E31, 0x0E31),
        (0x0E34, 0x0E3A),
        (0x200B, 0x200F), // zero-width space / joiners / marks
        (0x202A, 0x202E),
        (0x2060, 0x2064),
        (0xFE00, 0xFE0F), // variation selectors
        (0xFEFF, 0xFEFF), // BOM / ZWNBSP
    ];
    in_ranges(cp, ZERO)
}

/// East-Asian Wide / Fullwidth and the common 2-column emoji blocks (approximate).
fn is_wide(cp: u32) -> bool {
    const WIDE: &[(u32, u32)] = &[
        (0x1100, 0x115F),   // Hangul Jamo
        (0x2329, 0x232A),   // angle brackets
        (0x2E80, 0x303E),   // CJK radicals … Kangxi
        (0x3041, 0x33FF),   // Hiragana … CJK compatibility
        (0x3400, 0x4DBF),   // CJK Ext A
        (0x4E00, 0x9FFF),   // CJK Unified Ideographs
        (0xA000, 0xA4CF),   // Yi
        (0xAC00, 0xD7A3),   // Hangul Syllables
        (0xF900, 0xFAFF),   // CJK Compatibility Ideographs
        (0xFE10, 0xFE19),   // vertical forms
        (0xFE30, 0xFE6F),   // CJK compat / small forms
        (0xFF00, 0xFF60),   // Fullwidth forms
        (0xFFE0, 0xFFE6),   // Fullwidth signs
        (0x1F300, 0x1FAFF), // Emoji: pictographs, emoticons, transport, supplemental, symbols-ext
        (0x20000, 0x3FFFD), // CJK Ext B..F + Supplementary Ideographic Plane
    ];
    in_ranges(cp, WIDE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_one() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('Z'), 1);
        assert_eq!(char_width(' '), 1);
        assert_eq!(char_width('~'), 1);
    }

    #[test]
    fn controls_are_zero() {
        assert_eq!(char_width('\u{0}'), 0);
        assert_eq!(char_width('\t'), 0);
        assert_eq!(char_width('\u{7f}'), 0);
    }

    #[test]
    fn cjk_is_wide() {
        assert_eq!(char_width('世'), 2);
        assert_eq!(char_width('界'), 2);
        assert_eq!(char_width('한'), 2);
        assert_eq!(char_width('あ'), 2);
    }

    #[test]
    fn emoji_is_wide() {
        assert_eq!(char_width('😀'), 2);
        assert_eq!(char_width('🚀'), 2);
    }

    #[test]
    fn combining_is_zero() {
        assert_eq!(char_width('\u{0301}'), 0); // combining acute accent
    }

    #[test]
    fn str_width_mixes() {
        assert_eq!(str_width("ab"), 2);
        assert_eq!(str_width("a世"), 3);
        assert_eq!(str_width("世界"), 4);
    }
}
