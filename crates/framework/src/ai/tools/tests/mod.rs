use super::*;

#[test]
fn default_safe_excludes_write_and_exec() {
    for t in DEFAULT_SAFE_TOOLS {
        assert!(!matches!(*t, "sys.run" | "fs.write" | "fs.mkdir" | "fs.edit" | "fs.delete" | "task.run"), "{t} must not be implicitly safe");
    }
    // The coder set, by contrast, includes the dangerous tools (opt-in only).
    assert!(DEFAULT_CODER_TOOLS.contains(&"sys.run"));
    assert!(DEFAULT_CODER_TOOLS.contains(&"fs.write"));
    assert!(DEFAULT_CODER_TOOLS.contains(&"task.run"));
}
