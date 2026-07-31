use super::*;

fn temp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("tt-store-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn install_list_disable_remove() {
    let work = temp("a");
    let store = PluginStore::at(work.join("store"));

    // author a tiny plugin file and install it
    let src = work.join("hello.toml");
    std::fs::write(&src, "name = \"hello\"\nversion = \"0.1.0\"\ndescription = \"hi\"\n[aliases]\nh = \"echo hi\"\n").unwrap();
    let name = store.install(&src).unwrap();
    assert_eq!(name, "hello");

    let list = store.installed();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "hello");
    assert!(list[0].enabled);
    assert_eq!(store.manifests().len(), 1);
    assert!(store.manifests()[0].1, "and it is on");

    // disable → still listed, still RETURNED, just marked off. It has to stay
    // visible: a plugin the loader forgets about cannot be turned back on.
    store.set_enabled("hello", false).unwrap();
    assert!(!store.is_enabled("hello"));
    assert!(!store.installed()[0].enabled);
    assert_eq!(store.manifests().len(), 1, "still known");
    assert!(!store.manifests()[0].1, "and reported off");

    // re-enable
    store.set_enabled("hello", true).unwrap();
    assert!(store.manifests()[0].1);

    // remove
    assert!(store.remove("hello"));
    assert!(store.installed().is_empty());

    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn install_tplugin_bundle() {
    let work = temp("b");
    let store = PluginStore::at(work.join("store"));
    let bundle = work.join("kube.tplugin");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("plugin.toml"), "name = \"demo\"\nversion = \"1.0.0\"\n").unwrap();
    let name = store.install(&bundle).unwrap();
    assert_eq!(name, "demo");
    assert!(store.dir.join("demo.tplugin").join("plugin.toml").exists());
    let _ = std::fs::remove_dir_all(&work);
}
