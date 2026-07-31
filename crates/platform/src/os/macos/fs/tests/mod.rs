use super::*;

#[test]
fn root_volume_has_capacity() {
    let (total, free) = capacity("/");
    assert!(total > 0, "root volume reports a total size");
    assert!(free <= total);
}

#[test]
fn bad_path_is_zero() {
    assert_eq!(capacity("/no/such/path/zzz"), (0, 0));
}
