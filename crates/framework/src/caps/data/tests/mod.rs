use super::*;

fn rows() -> Vec<Json> {
    let mk = |id: &str, n: f64, done: bool| {
        Json::Obj(vec![
            ("id".into(), Json::Str(id.into())),
            ("n".into(), Json::Num(n)),
            ("done".into(), Json::Bool(done)),
            ("text".into(), Json::Str(format!("item {id}"))),
        ])
    };
    vec![mk("a", 3.0, false), mk("b", 1.0, true), mk("c", 2.0, false)]
}

#[test]
fn where_equality_and_ops() {
    let r = rows();
    // equality
    assert_eq!(filter(&r, Some(&Json::parse("{\"done\":false}").unwrap())).len(), 2);
    // operator object: n >= 2
    assert_eq!(filter(&r, Some(&Json::parse("{\"n\":{\"gte\":2}}").unwrap())).len(), 2);
    // contains (case-insensitive)
    assert_eq!(filter(&r, Some(&Json::parse("{\"text\":{\"contains\":\"ITEM A\"}}").unwrap())).len(), 1);
    // in
    assert_eq!(filter(&r, Some(&Json::parse("{\"id\":{\"in\":[\"a\",\"c\"]}}").unwrap())).len(), 2);
    // no clause → all
    assert_eq!(filter(&r, None).len(), 3);
}

#[test]
fn order_ascending_and_descending() {
    let mut r = rows();
    order_rows(&mut r, "n");
    assert_eq!(r.iter().map(|x| x.get("id").and_then(Json::as_str).unwrap()).collect::<Vec<_>>(), vec!["b", "c", "a"]);
    order_rows(&mut r, "-n");
    assert_eq!(r.iter().map(|x| x.get("id").and_then(Json::as_str).unwrap()).collect::<Vec<_>>(), vec!["a", "c", "b"]);
}

fn svc() -> (DataObj, CapCtx, PathBuf) {
    let dir = std::env::temp_dir().join(format!("tt-data-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = CapCtx {
        guard: std::sync::Arc::new(crate::guard::Guard::default()),
        app_data: Some(dir.clone()),
        remote_enabled: true,
        origin: "terminal://ai/".into(),
        sandbox: None,
        memory_dir: None, approver: std::sync::Arc::new(crate::guard::NobodyToAsk), asker: std::sync::Arc::new(crate::caps::ask::NobodyToAnswer),
    };
    (DataObj, ctx, dir)
}

#[test]
fn insert_query_update_delete_round_trip() {
    let (o, ctx, dir) = svc();
    let run = |m: &str, a: &[(&str, &str)]| {
        let args: Vec<(String, String)> = a.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        o.invoke(m, &args, &ctx, &mut crate::caps::host::NullHost)
    };
    run("data.insert", &[("table", "todos"), ("row", "{\"text\":\"ship\",\"done\":false}")]).unwrap();
    run("data.insert", &[("table", "todos"), ("row", "{\"text\":\"sleep\",\"done\":true}")]).unwrap();
    // query open todos
    let open = run("data.query", &[("table", "todos"), ("where", "{\"done\":false}")]).unwrap();
    assert_eq!(open.as_array().unwrap().len(), 1);
    assert_eq!(open.as_array().unwrap()[0].get("text").and_then(Json::as_str), Some("ship"));
    // count + tables
    assert_eq!(run("data.count", &[("table", "todos")]).unwrap().as_f64(), Some(2.0));
    assert_eq!(run("data.tables", &[]).unwrap().as_array().unwrap().len(), 1);
    // update all done=true
    let n = run("data.update", &[("table", "todos"), ("set", "{\"done\":true}")]).unwrap();
    assert_eq!(n.as_f64(), Some(2.0));
    assert_eq!(run("data.query", &[("table", "todos"), ("where", "{\"done\":false}")]).unwrap().as_array().unwrap().len(), 0);
    // delete done
    let d = run("data.delete", &[("table", "todos"), ("where", "{\"done\":true}")]).unwrap();
    assert_eq!(d.as_f64(), Some(2.0));
    assert_eq!(run("data.count", &[("table", "todos")]).unwrap().as_f64(), Some(0.0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn insert_stamps_id_created_updated() {
    let (o, ctx, dir) = svc();
    let args: Vec<(String, String)> = [("table", "t"), ("row", "{\"x\":1}")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let row = o.invoke("data.insert", &args, &ctx, &mut crate::caps::host::NullHost).unwrap();
    assert!(row.get("id").and_then(Json::as_str).is_some());
    assert!(row.get("created").and_then(Json::as_f64).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn data_requires_app_sandbox() {
    let ctx = CapCtx {
        guard: std::sync::Arc::new(crate::guard::Guard::default()),
        app_data: None,
        remote_enabled: true,
        origin: String::new(),
        sandbox: None,
        memory_dir: None, approver: std::sync::Arc::new(crate::guard::NobodyToAsk), asker: std::sync::Arc::new(crate::caps::ask::NobodyToAnswer),
    };
    assert!(DataObj.invoke("data.tables", &[], &ctx, &mut crate::caps::host::NullHost).is_err());
}
