use super::*;

#[test]
fn config_stamp_moves_when_either_config_file_changes() {
    let (_h, _home) = crate::test_home::lock_home("config-stamp");
    Config::ensure_default();
    let id = crate::profile::active_id();
    let a = config_stamp(&id);
    // Rewrite the profile overlay with a different mtime → the stamp moves.
    let overlay = crate::profile::config_path(&id).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&overlay, "[appearance]\ntheme = \"graphite\"\n").unwrap();
    let b = config_stamp(&id);
    assert_ne!(a, b, "an overlay edit is detected");
    // Touch the GLOBAL config too.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let text = std::fs::read_to_string(Config::path()).unwrap();
    std::fs::write(Config::path(), text).unwrap();
    assert_ne!(b, config_stamp(&id), "a global config edit is detected");
}
