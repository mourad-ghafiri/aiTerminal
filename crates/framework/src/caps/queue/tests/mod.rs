use super::*;

fn svc() -> (QueueObj, CapCtx, PathBuf) {
    let dir = std::env::temp_dir().join(format!("tt-queue-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = CapCtx {
        guard: std::sync::Arc::new(crate::guard::Guard::default()),
        app_data: Some(dir.clone()),
        remote_enabled: true,
        origin: "terminal://ai/".into(),
        sandbox: None,
        memory_dir: None, approver: std::sync::Arc::new(crate::guard::NobodyToAsk),
    };
    (QueueObj, ctx, dir)
}

#[test]
fn fifo_push_pop_peek_size() {
    let (o, ctx, dir) = svc();
    let run = |m: &str, a: &[(&str, &str)]| {
        let args: Vec<(String, String)> = a.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        o.invoke(m, &args, &ctx, &mut crate::caps::host::NullHost)
    };
    run("queue.push", &[("q", "jobs"), ("item", "{\"id\":1}")]).unwrap();
    run("queue.push", &[("q", "jobs"), ("item", "{\"id\":2}")]).unwrap();
    assert_eq!(run("queue.size", &[("q", "jobs")]).unwrap().as_f64(), Some(2.0));
    // FIFO: first pushed pops first
    assert_eq!(run("queue.peek", &[("q", "jobs")]).unwrap().get("id").and_then(Json::as_f64), Some(1.0));
    assert_eq!(run("queue.pop", &[("q", "jobs")]).unwrap().get("id").and_then(Json::as_f64), Some(1.0));
    assert_eq!(run("queue.pop", &[("q", "jobs")]).unwrap().get("id").and_then(Json::as_f64), Some(2.0));
    assert_eq!(run("queue.pop", &[("q", "jobs")]).unwrap(), Json::Null); // empty
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn priority_pops_first_then_fifo() {
    let (o, ctx, dir) = svc();
    let run = |m: &str, a: &[(&str, &str)]| {
        let args: Vec<(String, String)> = a.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        o.invoke(m, &args, &ctx, &mut crate::caps::host::NullHost)
    };
    run("queue.push", &[("q", "q"), ("item", "\"low1\""), ("priority", "0")]).unwrap();
    run("queue.push", &[("q", "q"), ("item", "\"hi\""), ("priority", "5")]).unwrap();
    run("queue.push", &[("q", "q"), ("item", "\"low2\""), ("priority", "0")]).unwrap();
    assert_eq!(run("queue.pop", &[("q", "q")]).unwrap().as_str(), Some("hi")); // priority first
    assert_eq!(run("queue.pop", &[("q", "q")]).unwrap().as_str(), Some("low1")); // then FIFO
    assert_eq!(run("queue.queues", &[]).unwrap().as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
