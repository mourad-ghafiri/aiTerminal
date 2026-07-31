use super::*;

fn cat() -> Catalog {
    let mut tables = BTreeMap::new();
    let mut en = Table::new();
    en.insert("ui.files".into(), "Files".into());
    en.insert("greet".into(), "Hi {0}".into());
    en.insert("items_one".into(), "{n} item".into());
    en.insert("items_other".into(), "{n} items".into());
    tables.insert("en".to_string(), en);
    let mut fr = Table::new();
    fr.insert("ui.files".into(), "Fichiers".into());
    tables.insert("fr".to_string(), fr);
    Catalog { locale: "en".into(), default_locale: "en".into(), tables }
}

#[test]
fn lookup_with_fallback_and_args() {
    let mut c = cat();
    assert_eq!(c.t("ui.files", &[]), "Files");
    assert_eq!(c.t("greet", &["Ada".into()]), "Hi Ada");
    assert_eq!(c.t("missing.key", &[]), "missing.key"); // visible fallback
    c.locale = "fr".into();
    assert_eq!(c.t("ui.files", &[]), "Fichiers");
    assert_eq!(c.t("greet", &["Ada".into()]), "Hi Ada"); // falls back to default (en)
}

#[test]
fn later_dirs_override_earlier_fallback() {
    // The mechanism `Config::i18n_dirs` relies on: bundled locales are the FALLBACK
    // (so shipped keys always resolve) and the installed copy OVERRIDES.
    let base = std::env::temp_dir().join(format!("tt-i18n-{}", std::process::id()));
    let (fallback, over) = (base.join("bundled"), base.join("installed"));
    std::fs::create_dir_all(&fallback).unwrap();
    std::fs::create_dir_all(&over).unwrap();
    std::fs::write(fallback.join("en.toml"), "[ai]\nsend = \"Send\"\nstop = \"Stop\"\n").unwrap();
    std::fs::write(over.join("en.toml"), "[ai]\nsend = \"Go\"\n").unwrap();
    let c = Catalog::load(&[fallback.as_path(), over.as_path()], "en");
    assert_eq!(c.t("ai.send", &[]), "Go", "later (installed) dir overrides");
    assert_eq!(c.t("ai.stop", &[]), "Stop", "bundled fallback fills keys the override omits");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn pluralization_by_count() {
    let c = cat();
    assert_eq!(c.t("items", &["1".into()]), "1 item");
    assert_eq!(c.t("items", &["5".into()]), "5 items");
}

#[test]
fn flatten_nests_to_dotted_keys() {
    let doc = Toml::parse("[ui]\nfiles = \"Files\"\nsend = \"Send\"\n").unwrap();
    let mut t = Table::new();
    flatten(&doc, String::new(), &mut t);
    assert_eq!(t.get("ui.files").map(String::as_str), Some("Files"));
    assert_eq!(t.get("ui.send").map(String::as_str), Some("Send"));
}

// ── i18n completeness (every shipped string is translatable) ──────────────
// These guard the invariant: every key the CODE references resolves to a real
// string in en.toml, every shipped locale defines the same key set, and no
// locale accumulates dead keys — so a hardcoded string or a forgotten
// translation fails CI instead of silently showing a raw key.

const BUILTIN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../builtin");
const SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src");

/// The flat key set of a bundled locale (`builtin/i18n/<loc>.toml`).
fn locale_keys(loc: &str) -> std::collections::BTreeSet<String> {
    let text = std::fs::read_to_string(format!("{BUILTIN}/i18n/{loc}.toml")).expect("locale file");
    let doc = Toml::parse(&text).expect("locale parses");
    let mut t = Table::new();
    flatten(&doc, String::new(), &mut t);
    t.into_keys().collect()
}

/// Every dotted i18n key the framework source passes to the translate call.
fn referenced_keys() -> std::collections::BTreeSet<String> {
    fn walk(dir: &Path, out: &mut std::collections::BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                let Ok(text) = std::fs::read_to_string(&p) else { continue };
                let mut rest = text.as_str();
                while let Some(i) = rest.find("translate(\"") {
                    rest = &rest[i + 11..];
                    if let Some(j) = rest.find('"') {
                        out.insert(rest[..j].to_string());
                        rest = &rest[j + 1..];
                    }
                }
            }
        }
    }
    let mut out = std::collections::BTreeSet::new();
    walk(Path::new(SRC), &mut out);
    out
}

#[test]
fn i18n_referenced_keys_are_defined() {
    let en = locale_keys("en");
    // A plural call site references the base key; the file defines `_one`/`_other`.
    let defined = |k: &str| en.contains(k) || en.contains(&format!("{k}_one"));
    let missing: Vec<_> = referenced_keys().into_iter().filter(|k| !defined(k)).collect();
    assert!(missing.is_empty(), "code references i18n keys not defined in en.toml: {missing:?}");
}

#[test]
fn i18n_locales_share_en_keys() {
    let en = locale_keys("en");
    // Every sibling locale must define EXACTLY the en key set (no missing, no extra) —
    // parity is what lets a translator diff a file and lets the UI never fall back
    // unexpectedly.
    let Ok(entries) = std::fs::read_dir(format!("{BUILTIN}/i18n")) else { panic!("i18n dir") };
    for e in entries.flatten() {
        let p = e.path();
        let Some(loc) = p.file_stem().and_then(|s| s.to_str()) else { continue };
        if loc == "en" || p.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        let keys = locale_keys(loc);
        let missing: Vec<_> = en.difference(&keys).cloned().collect();
        let extra: Vec<_> = keys.difference(&en).cloned().collect();
        assert!(missing.is_empty(), "{loc}.toml is missing keys present in en.toml: {missing:?}");
        assert!(extra.is_empty(), "{loc}.toml has keys absent from en.toml: {extra:?}");
    }
}

#[test]
fn i18n_no_dead_keys() {
    // Every key shipped in en.toml is actually referenced by the code — so the
    // locale files don't accumulate dead translations. A `_one`/`_other` pair
    // counts as referenced through its base key.
    let refs = referenced_keys();
    let dead: Vec<_> = locale_keys("en")
        .into_iter()
        .filter(|k| {
            let base = k.strip_suffix("_one").or_else(|| k.strip_suffix("_other")).unwrap_or(k);
            !refs.contains(base) && !refs.contains(k)
        })
        .collect();
    assert!(dead.is_empty(), "en.toml defines keys the code never references (remove them): {dead:?}");
}
