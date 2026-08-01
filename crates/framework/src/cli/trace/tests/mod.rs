use super::*;

fn args(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn a_call_names_what_it_is_acting_on_not_its_wire_format() {
    // The whole complaint: the trace printed the model's raw argument JSON, so the one
    // thing you wanted to know — which file — was usually the part that got truncated.
    assert_eq!(call("fs.read", &args(&[("path", "src/cli.rs"), ("max", "2000")])).line(), "fs.read src/cli.rs");
    assert_eq!(call("sys.run", &args(&[("cmd", "cargo test")])).line(), "sys.run cargo test");
    assert_eq!(call("web.read", &args(&[("url", "https://example.com/a")])).line(), "web.read https://example.com/a");
}

#[test]
fn the_identifying_argument_wins_over_the_ones_that_configure_the_call() {
    // `max` and `all` are settings; `path` is the subject. Argument ORDER must not decide
    // it — a model is free to serialize its object however it likes.
    let out = call("fs.edit", &args(&[("all", "true"), ("max", "10"), ("path", "src/lib.rs")])).line();
    assert_eq!(out, "fs.edit src/lib.rs");
}

#[test]
fn a_phrase_is_quoted_and_a_path_is_not() {
    // Quotes group a value the eye would otherwise read as several. A path or a command
    // is already unambiguous, so quoting it is noise.
    assert_eq!(call("web.search", &args(&[("query", "LLM memory architectures")])).line(), "web.search \"LLM memory architectures\"");
    assert_eq!(call("fs.read", &args(&[("path", "a/b c/d.rs")])).line(), "fs.read a/b c/d.rs");
    assert_eq!(call("sys.run", &args(&[("cmd", "-la")])).line(), "sys.run -la");
}

#[test]
fn a_long_path_keeps_the_end_you_were_looking_for() {
    // A plain truncation always takes the file name, which is the only part anybody was
    // reading. Both ends survive instead.
    let long = "crates/framework/src/cli/flow/exec/some/deeply/nested/runner.rs";
    let out = call("fs.read", &args(&[("path", long)])).line();
    assert!(out.ends_with("runner.rs"), "the end is the point: {out}");
    assert!(out.contains("crates/"), "and so is the start: {out}");
    assert!(out.contains('\u{2026}'), "with the middle elided: {out}");
    assert!(out.chars().count() <= SUBJECT_MAX + "fs.read ".len(), "bounded: {out}");
}

#[test]
fn a_whole_file_of_content_shows_its_first_line_and_stays_one_line() {
    let body = "fn main() {\n    println!(\"hello\");\n}\n";
    let out = call("fs.write", &args(&[("path", "src/main.rs"), ("content", body)])).line();
    assert_eq!(out, "fs.write src/main.rs", "the path identifies it, not the body");
    // And when content is all there is, it is still one line.
    let only = call("clip.set", &args(&[("content", body)])).line();
    assert!(!only.contains('\n'), "a trace line is a line: {only:?}");
}

#[test]
fn a_tool_nobody_here_has_heard_of_still_says_something() {
    // MCP servers expose tools this build has never seen. Falling back to the first
    // argument that carries anything beats printing the name alone.
    let out = call("mcp.jira.issue", &args(&[("ticket", "PROJ-1421")])).line();
    assert_eq!(out, "mcp.jira.issue PROJ-1421");
    // And a call with nothing to say says only its name, rather than an empty quote.
    assert_eq!(call("clock.now", &[]).line(), "clock.now");
    assert_eq!(call("fs.list", &args(&[("path", "   ")])).line(), "fs.list");
}

#[test]
fn a_result_is_read_off_its_shape_not_off_the_tool_that_produced_it() {
    // A per-tool table would be a second registry to keep in step with `caps`, and wrong
    // the day an MCP server adds a tool. JSON shape is the thing both ends already agree on.
    assert_eq!(result(&Json::Arr(vec![Json::Num(1.0), Json::Num(2.0)])), "2 results");
    assert_eq!(result(&Json::Arr(vec![Json::Num(1.0)])), "1 result", "and it counts in English");
    assert_eq!(result(&Json::Obj(vec![("replaced".into(), Json::Num(3.0))])), "3 replaced");
    assert_eq!(result(&Json::Bool(true)), "true");
}

#[test]
fn a_listing_counts_its_entries_in_english() {
    // `fs.list` returns `{path, entries: [...]}`. "1 entries" is the kind of detail that
    // makes a tool feel unfinished.
    let one = Json::Obj(vec![("entries".into(), Json::Arr(vec![Json::Num(1.0)]))]);
    let two = Json::Obj(vec![("entries".into(), Json::Arr(vec![Json::Num(1.0), Json::Num(2.0)]))]);
    assert_eq!(result(&one), "1 entry");
    assert_eq!(result(&two), "2 entries");
}

#[test]
fn command_output_is_counted_in_lines_and_a_value_is_not() {
    // `sys.run` hands back the combined output as a string. How many lines of it is the
    // question you ask of a command; how many bytes is the question you ask of a value.
    assert_eq!(result(&Json::Str("a\nb\nc\n".into())), "3 lines");
    assert_eq!(result(&Json::Str("one line".into())), "8B");
}

#[test]
fn only_shapes_the_shipped_tools_actually_return_are_named() {
    // An earlier draft reported `exit 0` for a command, which reads well and is a lie —
    // `sys.run` throws the status away. Nothing in `caps` returns an `exit` field, so the
    // trace must not claim to know one.
    let made_up = Json::Obj(vec![("exit".into(), Json::Num(0.0))]);
    assert!(!result(&made_up).contains("exit"), "an invented fact is worse than a smaller one");
}

#[test]
fn a_duration_is_shown_in_the_unit_a_person_would_have_used() {
    // `2100ms` is a number you convert before you can react to it.
    assert_eq!(took(9), "9ms");
    assert_eq!(took(999), "999ms");
    assert_eq!(took(1000), "1.0s");
    assert_eq!(took(2149), "2.1s");
}

#[test]
fn anything_else_falls_back_to_how_much_came_back() {
    // Not every result has a shape worth naming, and "how much" is always true.
    let big = result(&Json::Str("x".repeat(2048)));
    assert!(big.contains("KB"), "{big}");
}
