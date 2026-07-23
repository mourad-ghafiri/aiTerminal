//! The AI **tool catalog** — the default tool sets granted to agents. A "tool" is
//! just a `caps` method name (the `@tool <name> <json>` protocol is model-agnostic);
//! descriptions are injected at the framework layer (which owns `caps::describe`),
//! so the AI engine never reaches up into the capability registry.

/// The default SAFE toolset granted to an agent that declares no `tools` — read-only
/// exploration only (no exec, no write), so an under-specified agent can still browse +
/// reason without side effects. The loop still refuses any tool not in this set.
/// It is also the ceiling for delegated sub-agents (`task.run`).
pub const DEFAULT_SAFE_TOOLS: &[&str] = &[
    "fs.read", "fs.list", "fs.stat", "fs.home", "fs.glob", "fs.search", "web.read", "sec.check_command",
    "memory.search", "memory.get", "data.query", "data.get", "data.count", "queue.peek", "queue.size",
    "time.now", "time.relative", "codec.json_parse", "codec.sha256",
];

/// The full coding toolset a `coder`-style agent declares explicitly (read + write +
/// run + browse + remember + data/http + delegation). Never granted implicitly — an
/// agent must opt in via its frontmatter.
pub const DEFAULT_CODER_TOOLS: &[&str] = &[
    "fs.read", "fs.list", "fs.stat", "fs.glob", "fs.search", "fs.write", "fs.mkdir", "fs.edit", "fs.delete", "fs.copy",
    "fs.move", "fs.append", "sys.run", "web.read", "sec.check_command",
    "memory.search", "memory.get", "memory.add", "memory.update", "memory.forget",
    "data.query", "data.get", "data.count", "data.insert", "data.update", "data.delete",
    "queue.push", "queue.pop", "http.get", "http.post", "time.now", "codec.json_parse",
    "todo.set", "todo.add", "todo.done", "todo.list", "task.run",
];

#[cfg(test)]
mod tests {
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
}
