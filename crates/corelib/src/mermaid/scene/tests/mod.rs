use super::*;

#[test]
fn builder_sizes_to_the_furthest_item_plus_margin() {
    let mut b = Builder::new(10.0);
    b.shape(Shape::Rect, Rect::new(0.0, 0.0, 40.0, 20.0), "A", Role::Node);
    b.shape(Shape::Rect, Rect::new(60.0, 30.0, 40.0, 20.0), "B", Role::Node);
    let s = b.build();
    assert_eq!((s.width, s.height), (110, 60));
    assert_eq!(s.node_labels(), vec!["A", "B"]);
}

#[test]
fn an_empty_builder_is_a_zero_scene() {
    assert_eq!(Builder::new(8.0).build(), Scene::default());
}

#[test]
fn groups_sit_behind_the_items_they_frame() {
    let mut b = Builder::new(4.0);
    b.shape(Shape::Rect, Rect::new(10.0, 10.0, 20.0, 10.0), "inner", Role::Node);
    b.group(Rect::new(0.0, 0.0, 40.0, 30.0), "frame", Role::Muted);
    let s = b.build();
    assert!(matches!(s.items[0], Item::Group { .. }), "the frame draws first");
}

#[test]
fn paths_extend_the_extent() {
    let mut b = Builder::new(0.0);
    b.path(vec![(0.0, 0.0), (50.0, 25.0)], Stroke::Solid, Cap::None, Cap::Arrow, "", Role::Edge);
    let s = b.build();
    assert_eq!((s.width, s.height), (50, 25));
    assert_eq!(s.paths().count(), 1);
}
