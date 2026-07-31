use super::*;

fn svc() -> (MemoryService, PathBuf) {
    let dir = std::env::temp_dir().join(format!("aiterm-mem-{}-{:?}", std::process::id(), std::thread::current().id()));
    let _ = std::fs::remove_dir_all(&dir);
    (MemoryService::with_dirs(dir.clone(), vec![dir.clone()]), dir)
}

#[test]
fn add_round_trips_through_frontmatter() {
    let (s, dir) = svc();
    let e = s.add("fact", vec!["api".into()], "API base is /v2; auth via X-Token").unwrap();
    let loaded = s.get(&e.id).unwrap();
    assert_eq!(loaded.body, "API base is /v2; auth via X-Token");
    assert_eq!(loaded.kind, "fact");
    assert_eq!(loaded.tags, vec!["api".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn search_finds_relevant_and_recall_filters_noise() {
    let (s, dir) = svc();
    s.add("fact", vec![], "Deploy runs on push to main").unwrap();
    s.add("fact", vec![], "Prod region is us-east-1").unwrap();
    s.add("fact", vec![], "The office plant needs watering on Fridays").unwrap();
    let hits = s.search("how to deploy", 5);
    assert_eq!(hits[0].0.body, "Deploy runs on push to main");
    let recalled = s.recall("deploy to production", 5);
    assert!(recalled.iter().any(|m| m.body.contains("Deploy")));
    assert!(!recalled.iter().any(|m| m.body.contains("plant")), "noise filtered out of recall");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn get_reinforces_salience_and_recalls() {
    let (s, dir) = svc();
    let e = s.add("fact", vec![], "rate limit is 100 rpm").unwrap();
    assert_eq!(e.recalls, 0);
    let r1 = s.get(&e.id).unwrap();
    assert_eq!(r1.recalls, 1);
    assert!(r1.salience > e.salience, "salience reinforced on recall");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn learning_a_fact_twice_reinforces_it_instead_of_writing_it_twice() {
    // An agent re-learns the same thing constantly — it reads a config, saves what
    // it found, and does it again next run. Two files both ranked, so the model
    // paid twice to be told one thing.
    let (s, dir) = svc();
    let first = s.add("fact", vec!["deploy".into()], "Deploys go through `make ship`, never push to main").unwrap();
    let again = s.add("decision", vec!["ci".into()], "Deploys go through `make ship` and never push to main").unwrap();

    assert_eq!(again.id, first.id, "the same fact is one memory");
    assert_eq!(s.list().len(), 1, "no near-duplicate file was written");
    assert!(again.salience > first.salience, "learning it twice is evidence it matters");
    assert!(again.tags.contains(&"deploy".to_string()) && again.tags.contains(&"ci".to_string()), "tags merge: {:?}", again.tags);

    // A genuinely different fact is still a new memory.
    let other = s.add("fact", vec![], "The staging database resets every Sunday").unwrap();
    assert_ne!(other.id, first.id);
    assert_eq!(s.list().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recall_follows_links_one_hop() {
    // A decision ranks because it shares words with the question; the reason it was
    // made usually does not. Following the relation retrieves what lexical
    // matching structurally cannot.
    let (s, dir) = svc();
    let decision = s.add("decision", vec!["deploy".into()], "Deploys go through `make ship`").unwrap();
    let why = s.add("fact", vec![], "Direct pushes skipped the migration step and corrupted two tenants").unwrap();
    assert!(s.link(&decision.id, &why.id));

    let recalled = s.recall("how do we deploy", 3);
    assert!(recalled.iter().any(|m| m.id == decision.id), "the decision ranks on its own words");
    assert!(recalled.iter().any(|m| m.id == why.id), "and brings its reason, which shares no query words");

    // The relation is followable in both directions.
    assert!(s.get(&why.id).unwrap().links.contains(&decision.id));
    // Linking is refused when an id is wrong, rather than silently doing nothing.
    assert!(!s.link(&decision.id, "no-such-id"));
    assert!(!s.link(&decision.id, &decision.id), "a memory cannot link to itself");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn links_survive_the_file_and_can_be_written_by_hand() {
    let (s, dir) = svc();
    let a = s.add("fact", vec![], "the first note").unwrap();
    let b = s.add("fact", vec![], "a completely unrelated second note").unwrap();
    s.link(&a.id, &b.id);
    // Round-trips through the frontmatter.
    assert!(s.get(&a.id).unwrap().links.contains(&b.id));

    // And someone editing the file by hand can write `[[id]]` in the body instead
    // of maintaining the frontmatter list.
    let hand_written = format!("---\nkind = \"fact\"\n---\nSee [[{}]] for the reason.\n", b.id);
    std::fs::write(dir.join("hand-written.md"), hand_written).unwrap();
    let parsed = MemoryService::with_dirs(dir.clone(), vec![dir.clone()])
        .list()
        .into_iter()
        .find(|e| e.id == "hand-written")
        .expect("the hand-written note loaded");
    assert!(parsed.links.contains(&b.id), "a [[link]] in the body counts: {:?}", parsed.links);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_exact_tag_beats_the_same_word_in_prose() {
    // A tag is a deliberate act; the same word in a body may be an aside. BM25
    // alone cannot tell them apart, because it sees one flat bag of words.
    // Same word, same number of times, in bodies of the same length — the ONLY
    // difference is that somebody tagged one of them.
    let (s, dir) = svc();
    let tagged = s.add("fact", vec!["release".into()], "We cut a release from a dated branch").unwrap();
    let untagged = s.add("fact", vec![], "Ada brought cake to the release party").unwrap();
    let hits = s.search("release", 5);
    let rank = |id: &str| hits.iter().position(|(e, _)| e.id == id).expect("both matched");
    assert!(
        rank(&tagged.id) < rank(&untagged.id),
        "the deliberately tagged note wins: {:?}",
        hits.iter().map(|(e, s)| (&e.body, s)).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn forget_removes() {
    let (s, dir) = svc();
    let e = s.add("fact", vec![], "ephemeral note").unwrap();
    assert!(s.forget(&e.id));
    assert!(s.get(&e.id).is_none());
    assert!(!s.forget(&e.id), "forgetting a missing id is false");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn consolidate_merges_duplicates() {
    // `add` now refuses to write a near-duplicate, so duplicates only reach the
    // store the other ways: a file written by hand, or a note that predates the
    // check. That is still `consolidate`'s job, and this is now the honest fixture
    // for it — going through `add` twice would silently test nothing.
    let (s, dir) = svc();
    s.add("fact", vec![], "Deploy runs on push to the main branch").unwrap();
    std::fs::write(
        dir.join("hand-written-copy.md"),
        "---\nkind = \"fact\"\n---\nDeploy runs on push to the main branch\n",
    )
    .unwrap();
    assert_eq!(s.list().len(), 2, "two files really are on disk");

    let (merged, _pruned) = s.consolidate();
    assert!(merged >= 1, "near-duplicate merged");
    assert_eq!(s.list().len(), 1, "one survives");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn first_read_dir_wins_on_same_id() {
    let base = std::env::temp_dir().join(format!("aiterm-mem-shadow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let proj = base.join("proj");
    let global = base.join("global");
    let g = MemoryService::with_dirs(global.clone(), vec![global.clone()]);
    let e = g.add("fact", vec![], "global value").unwrap();
    // Write an entry in the FIRST dir with the SAME id but different body.
    let mut pe = e.clone();
    pe.body = "project value".into();
    MemoryStore::save(&proj, &pe).unwrap();
    let s = MemoryService::with_dirs(proj.clone(), vec![proj, global]);
    assert_eq!(s.list().len(), 1, "deduped by id");
    assert_eq!(s.get(&e.id).unwrap().body, "project value", "project shadows global");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn for_folder_writes_folder_and_recalls_folder_then_global() {
    // A folder-scoped service writes to the folder store and recalls across BOTH the
    // folder and global stores — the mechanism behind per-folder AI memory.
    let base = std::env::temp_dir().join(format!("aiterm-mem-folder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let folder = base.join("proj-mem");
    let global = base.join("global-mem");
    // A global fact + a folder-scoped fact via the for_folder-style constructor.
    MemoryService::with_dirs(global.clone(), vec![global.clone()])
        .add("fact", vec![], "the org standard formatter is rustfmt").unwrap();
    let svc = MemoryService::with_dirs(folder.clone(), vec![folder.clone(), global.clone()]);
    let added = svc.add("decision", vec![], "this project deploys via scripts/ship.sh").unwrap();
    // The folder write landed in the FOLDER dir, not global.
    assert!(folder.join(format!("{}.md", added.id)).exists(), "folder write goes to the folder store");
    assert!(!global.join(format!("{}.md", added.id)).exists());
    // Recall reaches both stores.
    assert!(svc.search("ship deploy", 5).iter().any(|(e, _)| e.body.contains("ship.sh")), "folder fact recalled");
    assert!(svc.search("formatter", 5).iter().any(|(e, _)| e.body.contains("rustfmt")), "global fact still recalled");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn corpus_cache_skips_disk_until_the_store_changes() {
    let (svc, dir) = svc();
    svc.add("fact", vec![], "the deploy target is fly.io").unwrap();
    let base = DISK_LOADS.with(|c| c.get());
    assert_eq!(svc.search("deploy", 3).len(), 1);
    let after_first = DISK_LOADS.with(|c| c.get());
    assert!(after_first > base, "first search reads the store");
    // Unchanged store → the second and third searches are pure cache hits.
    assert_eq!(svc.search("deploy", 3).len(), 1);
    svc.list();
    assert_eq!(DISK_LOADS.with(|c| c.get()), after_first, "no re-read while the stamp is stable");
    // A write moves the stamp → the next search re-reads AND sees the new entry.
    svc.add("fact", vec![], "the cache invalidates on write").unwrap();
    assert!(svc.search("cache invalidates", 3).len() >= 1);
    assert!(DISK_LOADS.with(|c| c.get()) > after_first, "a write invalidates the cache");
    let _ = std::fs::remove_dir_all(&dir);
}
