use super::*;

#[test]
fn a_record_lives_exactly_as_long_as_its_gate() {
    let (_home, _dir) = crate::test_home::lock_home("gate-record-life");
    let rec = GateRecord::create("telegram").unwrap();
    let id = rec.id().to_string();
    assert_eq!(read(&id).unwrap().status, RUNNING);
    assert_eq!(read(&id).unwrap().channel, "telegram");
    drop(rec);
    assert!(read(&id).is_none(), "the record must not outlive the gate");
}

#[test]
fn another_pane_stops_a_gate_by_writing_not_signalling() {
    // Signalling would skip the Drop guards that restore the user's terminal.
    let (_home, _dir) = crate::test_home::lock_home("gate-record-stop");
    let rec = GateRecord::create("telegram").unwrap();
    assert!(!rec.stop_requested());
    let flagged = request_stop(rec.id()).expect("record found");
    assert_eq!(flagged.status, STOPPING);
    assert!(rec.stop_requested(), "the running gate notices on its next poll");
}

#[test]
fn a_vanished_record_also_means_stop() {
    let (_home, _dir) = crate::test_home::lock_home("gate-record-gone");
    let rec = GateRecord::create("telegram").unwrap();
    std::fs::remove_file(Config::gates_dir().join(format!("{}.toml", rec.id()))).unwrap();
    assert!(rec.stop_requested(), "a gate whose record was deleted must not keep running");
}

#[test]
fn the_paired_chat_is_published_for_other_panes() {
    let (_home, _dir) = crate::test_home::lock_home("gate-record-peer");
    let mut rec = GateRecord::create("telegram").unwrap();
    rec.set_peer("Mourad (51234903)");
    assert_eq!(read(rec.id()).unwrap().peer, "Mourad (51234903)");
    assert_eq!(read(rec.id()).unwrap().status, RUNNING, "publishing a peer must not disturb the status");
}

#[test]
fn listing_prunes_records_whose_process_is_gone() {
    let (_home, _dir) = crate::test_home::lock_home("gate-record-list");
    let rec = GateRecord::create("telegram").unwrap();
    assert_eq!(list().len(), 1, "our own live gate is listed");

    // A crashed gate's leftover file, pointing at a pid that cannot be running.
    let stale = Info {
        id: "1-4294967200".into(),
        channel: "telegram".into(),
        status: RUNNING.into(),
        pid: 4_294_967_200,
        started: 1,
        peer: String::new(),
    };
    write(&Config::gates_dir().join("1-4294967200.toml"), &stale);
    let listed = list();
    assert_eq!(listed.len(), 1, "the stale record is swept, not shown");
    assert_eq!(listed[0].id, rec.id());
}

#[test]
fn a_hostile_id_cannot_escape_the_gates_directory() {
    assert!(path_for("../../etc/passwd").is_none());
    assert!(path_for("a/b").is_none());
    assert!(path_for("").is_none());
    assert!(path_for("1234-99").is_some());
}
