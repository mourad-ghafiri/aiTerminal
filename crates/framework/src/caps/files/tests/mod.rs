use super::*;

/// A unique, self-cleaning scratch directory under the system temp dir.
struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ttfiles-{}-{}-{tag}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn guard_blocks_secrets_and_system_allows_safe_roots() {
    // System path → denied.
    assert!(user_write_guard(Path::new("/usr/bin/whatever")).is_err());
    assert!(user_write_guard(Path::new("/System/x")).is_err());
    // `..` traversal → denied.
    assert!(user_write_guard(Path::new("/tmp/../etc/passwd")).is_err());
    // A temp path (an allowed root) → permitted.
    let ok = std::env::temp_dir().join("ttfiles-guard-ok");
    assert!(user_write_guard(&ok).is_ok());
    // A secret under home → denied even though home is an allowed root.
    if let Some(home) = platform::os::home_dir() {
        assert!(user_write_guard(&home.join(".ssh/id_rsa")).is_err());
    }
}

#[test]
fn mkdir_create_rename_duplicate_roundtrip() {
    let s = Scratch::new("crud");
    let dir = s.join("project");
    std::fs::create_dir(&dir).unwrap();

    // create an empty file
    let f = dir.join("notes.txt");
    std::fs::OpenOptions::new().write(true).create_new(true).open(&f).unwrap();
    std::fs::write(&f, b"hello").unwrap();

    // rename
    let renamed = do_rename(&f, "todo.txt").unwrap();
    assert!(!f.exists() && renamed.exists());
    assert_eq!(std::fs::read(&renamed).unwrap(), b"hello");

    // duplicate → "todo copy.txt"
    let dup = duplicate_target(&renamed);
    assert_eq!(dup.file_name().unwrap().to_string_lossy(), "todo copy.txt");
    copy_recursive(&renamed, &dup).unwrap();
    assert_eq!(std::fs::read(&dup).unwrap(), b"hello");

    // duplicate again bumps the counter
    let dup2 = duplicate_target(&renamed);
    assert_eq!(dup2.file_name().unwrap().to_string_lossy(), "todo copy 2.txt");
}

#[test]
fn copy_and_move_directory_trees() {
    let s = Scratch::new("tree");
    let src = s.join("a");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("sub/x.txt"), b"deep").unwrap();

    let copied = s.join("a-copy");
    copy_recursive(&src, &copied).unwrap();
    assert_eq!(std::fs::read(copied.join("sub/x.txt")).unwrap(), b"deep");
    assert!(src.exists(), "copy keeps the source");

    let moved = s.join("a-moved");
    move_path(&src, &moved).unwrap();
    assert!(!src.exists(), "move removes the source");
    assert_eq!(std::fs::read(moved.join("sub/x.txt")).unwrap(), b"deep");
}

#[test]
fn trash_moves_into_the_given_dir_and_avoids_collisions() {
    let s = Scratch::new("trash");
    let trash = s.join("Trash");

    let a = s.join("doc.txt");
    std::fs::write(&a, b"one").unwrap();
    let landed = trash_to(&a, &trash).unwrap();
    assert!(!a.exists(), "trashed file leaves its source");
    assert_eq!(landed, trash.join("doc.txt"));
    assert_eq!(std::fs::read(&landed).unwrap(), b"one");

    // a second file with the same name gets a distinct trashed name
    let b = s.join("doc.txt");
    std::fs::write(&b, b"two").unwrap();
    let landed2 = trash_to(&b, &trash).unwrap();
    assert_ne!(landed2, landed);
    assert!(landed2.exists());
}
