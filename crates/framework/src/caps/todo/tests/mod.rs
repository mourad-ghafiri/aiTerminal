use super::*;
use std::path::PathBuf;

fn svc() -> (TodoObj, CapCtx, PathBuf) {
    let dir = std::env::temp_dir().join(format!("tt-todo-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = CapCtx {
        guard: std::sync::Arc::new(crate::guard::Guard::default()),
        app_data: Some(dir.clone()),
        remote_enabled: true,
        origin: "terminal://ai/".into(),
        sandbox: None,
        memory_dir: None, approver: std::sync::Arc::new(crate::guard::NobodyToAsk), asker: std::sync::Arc::new(crate::caps::ask::NobodyToAnswer),
    };
    (TodoObj, ctx, dir)
}

#[test]
fn set_add_done_list_clear_roundtrip() {
    let (o, ctx, dir) = svc();
    let run = |m: &str, a: &[(&str, &str)]| {
        let args: Vec<(String, String)> = a.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        o.invoke(m, &args, &ctx, &mut crate::caps::host::NullHost)
    };
    // set a plan of two tasks (one a bare string, one already done)
    run("todo.set", &[("items", "[\"read the test\", {\"text\":\"write the fix\",\"done\":false}]")]).unwrap();
    let list = run("todo.list", &[]).unwrap();
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0].get("text").and_then(Json::as_str), Some("read the test"));
    assert_eq!(arr[0].get("done"), Some(&Json::Bool(false)));
    // add a third
    run("todo.add", &[("text", "run the suite")]).unwrap();
    assert_eq!(run("todo.list", &[]).unwrap().as_array().unwrap().len(), 3);
    // mark done by index, then by matching text
    run("todo.done", &[("index", "0")]).unwrap();
    run("todo.done", &[("text", "run the suite")]).unwrap();
    let arr = run("todo.list", &[]).unwrap();
    let arr = arr.as_array().unwrap();
    assert_eq!(arr[0].get("done"), Some(&Json::Bool(true)));
    assert_eq!(arr[2].get("done"), Some(&Json::Bool(true)));
    assert_eq!(arr[1].get("done"), Some(&Json::Bool(false)));
    // a non-matching done errors; clear empties
    assert!(run("todo.done", &[("text", "nope")]).is_err());
    run("todo.clear", &[]).unwrap();
    assert!(run("todo.list", &[]).unwrap().as_array().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
