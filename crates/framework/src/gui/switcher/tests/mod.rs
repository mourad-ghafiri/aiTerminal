use super::*;

fn entry(index: usize, title: &str, detail: &str) -> SwitcherEntry {
    SwitcherEntry { index, icon: "\u{276F}".into(), title: title.into(), detail: detail.into() }
}

#[test]
fn digit_query_matches_tab_number_and_selects_exact() {
    let entries = (1..=20).map(|i| entry(i, &format!("tab {i}"), "")).collect();
    let mut s = SwitcherState::new(entries);
    s.type_text("1"); // 1, 10..19 (prefix), 1 itself
    assert!(s.filtered.iter().all(|&i| s.entries[i].index.to_string().starts_with('1')));
    assert_eq!(s.chosen_tab(), Some(0)); // exact "1" → tab index 0
    s.type_text("5"); // "15"
    assert_eq!(s.chosen_tab(), Some(14)); // tab 15 → 0-based 14
}

#[test]
fn text_query_substring_filters_title_and_detail() {
    let entries = vec![entry(1, "Terminal [zsh]", "~/proj"), entry(2, "vim main.rs", "~/src"), entry(3, "htop", "~")];
    let mut s = SwitcherState::new(entries);
    s.type_text("main");
    assert_eq!(s.filtered.len(), 1);
    assert_eq!(s.chosen_tab(), Some(1));
    s.backspace();
    s.backspace();
    s.backspace();
    s.backspace(); // cleared → all visible again
    assert_eq!(s.filtered.len(), 3);
}

#[test]
fn arrow_navigation_wraps() {
    let entries = (1..=3).map(|i| entry(i, &format!("t{i}"), "")).collect();
    let mut s = SwitcherState::new(entries);
    assert_eq!(s.chosen_tab(), Some(0));
    s.move_sel(-1); // wrap to last
    assert_eq!(s.chosen_tab(), Some(2));
    s.move_sel(1); // wrap to first
    assert_eq!(s.chosen_tab(), Some(0));
}
