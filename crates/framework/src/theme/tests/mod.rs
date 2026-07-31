use super::*;

#[test]
fn resolve_falls_back_to_default_then_prefers_user_file() {
    let dir = std::env::temp_dir().join(format!("tt-fwtheme-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // No file yet → the default (midnight) fallback (compare serialized form; not Eq).
    let default = corelib::theme::midnight().to_toml();
    assert_eq!(resolve(&dir, "whatever").to_toml(), default);
    // Materialize the collection, then each is listed + resolvable.
    ensure_default(&dir);
    let listed = names(&dir);
    assert!(listed.contains(&"midnight".to_string()) && listed.contains(&"deep-purple".to_string()));
    assert_eq!(resolve(&dir, "midnight").to_toml(), default);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn slug_is_filesystem_safe() {
    assert_eq!(slug("Deep Purple"), "deep-purple");
    assert_eq!(slug("Product RED"), "product-red");
    assert_eq!(slug("Midnight"), "midnight");
}
