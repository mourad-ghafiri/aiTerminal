use super::*;

fn area() -> Rect {
    Rect::new(0.0, 0.0, 1000.0, 600.0)
}

#[test]
fn new_tree_has_one_focused_pane() {
    let t = PaneTree::new(7u32);
    assert_eq!(t.pane_ids().len(), 1);
    assert_eq!(t.focused(), PaneId(0));
    assert_eq!(t.focused_content(), Some(&7));
    assert_eq!(t.layout(area()).len(), 1);
}

#[test]
fn split_creates_and_focuses_new_pane() {
    let mut t = PaneTree::new(1u32);
    let id = t.split(Axis::Horizontal, 2u32);
    assert_eq!(t.pane_ids().len(), 2);
    assert_eq!(t.focused(), id);
    assert_eq!(t.focused_content(), Some(&2));
    let l = t.layout(area());
    assert_eq!(l.len(), 2);
    // side-by-side: same height, x offset
    assert_eq!(l[0].1.y, l[1].1.y);
    assert!(l[1].1.x > l[0].1.x);
}

#[test]
fn nested_splits_and_close_collapses() {
    let mut t = PaneTree::new(1u32);
    t.split(Axis::Horizontal, 2u32); // focus on 2
    let third = t.split(Axis::Vertical, 3u32); // split the right pane
    assert_eq!(t.pane_ids().len(), 3);
    assert_eq!(t.focused(), third);
    let closed = t.close_focused().unwrap();
    assert_eq!(closed, third);
    assert_eq!(t.pane_ids().len(), 2);
    // original content survives
    assert!(t.pane_ids().iter().any(|id| t.get(*id) == Some(&1)));
}

#[test]
fn cannot_close_last_pane() {
    let mut t = PaneTree::new(1u32);
    assert_eq!(t.close_focused(), None);
    assert_eq!(t.pane_ids().len(), 1);
}

#[test]
fn move_tab_reorders_and_keeps_dragged_active() {
    let mut tabs = Tabs::new(0u32);
    tabs.new_tab(1u32);
    tabs.new_tab(2u32);
    tabs.new_tab(3u32); // [0,1,2,3], active = 3
    let order = |t: &Tabs<u32>| t.iter().map(|p| *p.focused_content().unwrap()).collect::<Vec<u32>>();

    // Move the first tab to the end — it stays focused at its new slot.
    tabs.move_tab(0, 3);
    assert_eq!(order(&tabs), vec![1, 2, 3, 0]);
    assert_eq!(tabs.active_index(), 3);
    // Move the last tab back to the front.
    tabs.move_tab(3, 0);
    assert_eq!(order(&tabs), vec![0, 1, 2, 3]);
    assert_eq!(tabs.active_index(), 0);
    // Move a middle tab left one slot.
    tabs.move_tab(2, 1);
    assert_eq!(order(&tabs), vec![0, 2, 1, 3]);
    assert_eq!(tabs.active_index(), 1);
    // No-ops: equal indices, or either index out of range — order untouched.
    tabs.move_tab(1, 1);
    tabs.move_tab(0, 99);
    tabs.move_tab(99, 0);
    assert_eq!(order(&tabs), vec![0, 2, 1, 3]);
}

#[test]
fn zoom_fills_area() {
    let mut t = PaneTree::new(1u32);
    t.split(Axis::Horizontal, 2u32);
    t.toggle_zoom();
    let l = t.layout(area());
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].1, area());
    t.toggle_zoom();
    assert_eq!(t.layout(area()).len(), 2);
}

#[test]
fn focus_dir_moves_geometrically() {
    let mut t = PaneTree::new(1u32);
    t.split(Axis::Horizontal, 2u32); // focus right (2)
    // move focus left → should land on pane 1
    assert!(t.focus_dir(Dir::Left, area()));
    assert_eq!(t.focused_content(), Some(&1));
    assert!(t.focus_dir(Dir::Right, area()));
    assert_eq!(t.focused_content(), Some(&2));
}

/// A `u32`-content closure pair, so the snapshot/restore logic is tested free of any
/// pane/app knowledge (matching how `gui::workspace` plugs in the real Pane closures).
fn snap(n: &u32) -> Toml {
    Toml::Table(vec![("leaf".into(), Toml::Int(*n as i64))])
}
fn unsnap(t: &Toml) -> Option<u32> {
    t.get("leaf").and_then(|v| v.as_int()).map(|i| i as u32)
}

#[test]
fn pane_tree_snapshot_round_trips_structure_focus_and_zoom() {
    let mut t = PaneTree::new(1u32);
    t.split(Axis::Horizontal, 2u32); // focus on 2
    t.split(Axis::Vertical, 3u32); // split the right pane; focus on 3
    t.focus(t.pane_ids()[0]); // focus the first leaf (content 1)
    t.toggle_zoom(); // zoom the focused leaf

    let snap_toml = t.snapshot(&snap);
    // Survives a text round-trip too (this is what lands on disk).
    let text = snap_toml.to_string();
    let reparsed = Toml::parse(&text).unwrap();
    let mut g = unsnap;
    let mut r = PaneTree::restore(&reparsed, &mut g).expect("restore");

    // Same leaves, same layout geometry, same focus content, same zoom (1 visible).
    let area = area();
    assert_eq!(r.layout(area).len(), 1, "zoom restored → one visible pane");
    r.toggle_zoom();
    assert_eq!(r.layout(area).len(), 3, "unzoom → all three leaves");
    assert_eq!(r.focused_content(), Some(&1), "focused leaf content preserved");
    let mut contents: Vec<u32> = r.pane_ids().iter().filter_map(|id| r.get(*id).copied()).collect();
    contents.sort();
    assert_eq!(contents, vec![1, 2, 3]);
}

#[test]
fn tabs_snapshot_round_trips() {
    let mut tabs = Tabs::new(10u32);
    tabs.new_tab(20u32);
    tabs.active_mut().split(Axis::Horizontal, 21u32);
    tabs.new_tab(30u32);
    tabs.prev_tab(); // active on the middle tab

    let toml = tabs.snapshot(&snap);
    let mut g = unsnap;
    let r = Tabs::restore(&Toml::parse(&toml.to_string()).unwrap(), &mut g).expect("restore tabs");
    assert_eq!(r.len(), 3);
    assert_eq!(r.active_index(), 1, "active tab index preserved");
    assert_eq!(r.active().pane_ids().len(), 2, "the split in the active tab survived");
}

#[test]
fn restore_drops_a_tab_whose_leaf_fails() {
    let mut tabs = Tabs::new(1u32);
    tabs.new_tab(2u32);
    let toml = tabs.snapshot(&snap);
    // A closure that rejects content `1` → that tab is dropped, the other survives.
    let mut g = |t: &Toml| unsnap(t).filter(|n| *n != 1);
    let r = Tabs::restore(&toml, &mut g).expect("one tab survives");
    assert_eq!(r.len(), 1);
    assert_eq!(r.active().focused_content(), Some(&2));
}

#[test]
fn tabs_lifecycle() {
    let mut tabs = Tabs::new(10u32);
    assert_eq!(tabs.len(), 1);
    tabs.new_tab(20u32);
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs.active_index(), 1);
    tabs.prev_tab();
    assert_eq!(tabs.active_index(), 0);
    assert_eq!(tabs.active().focused_content(), Some(&10));
    let removed = tabs.close_tab().unwrap();
    assert_eq!(removed.focused_content(), Some(&10));
    assert_eq!(tabs.len(), 1);
    assert!(tabs.close_tab().is_none()); // last tab stays
}
