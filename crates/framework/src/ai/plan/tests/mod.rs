use super::*;

const NOW: u64 = 1_700_000_000;

fn plan(reply: &str) -> Option<Plan> {
    decode(reply, "the original request", NOW)
}

#[test]
fn a_clock_repeat_becomes_cron() {
    let p = plan(r#"{"when":{"kind":"cron","cron":"0 0 * * *"},"task":"check the logs","says":"every day at 00:00 — check the logs"}"#).unwrap();
    assert!(matches!(p.schedule, Some(Schedule::Cron(_))));
    assert_eq!(p.task, "check the logs");
    assert!(p.says.contains("00:00"));
    assert!(p.cmd.is_none());
}

#[test]
fn an_interval_and_a_one_shot() {
    let every = plan(r#"{"when":{"kind":"every","every_seconds":900},"task":"sync"}"#).unwrap();
    assert_eq!(every.schedule, Some(Schedule::Every(900)));
    let once = plan(r#"{"when":{"kind":"once","in_seconds":120},"task":"stretch"}"#).unwrap();
    assert_eq!(once.schedule, Some(Schedule::Once(NOW + 120)));
    let now = plan(r#"{"when":{"kind":"now"},"task":"tidy up"}"#).unwrap();
    assert_eq!(now.schedule, None);
}

#[test]
fn a_command_request_carries_its_command() {
    let p = plan(r#"{"when":{"kind":"cron","cron":"0 18 * * 1-5"},"task":"run the backup","command":"./backup.sh","says":"weekdays at 18:00 — ./backup.sh"}"#).unwrap();
    assert_eq!(p.cmd, Some(Cmd::Line("./backup.sh".into())));
}

#[test]
fn a_reply_wrapped_in_prose_or_a_fence_still_decodes() {
    let fenced = "Sure!\n```json\n{\"when\":{\"kind\":\"every\",\"every_seconds\":3600},\"task\":\"x\"}\n```\nHope that helps.";
    assert_eq!(plan(fenced).unwrap().schedule, Some(Schedule::Every(3600)));
    // Braces inside strings don't end the object early.
    let tricky = r#"{"when":{"kind":"now"},"task":"print {curly} braces"}"#;
    assert_eq!(plan(tricky).unwrap().task, "print {curly} braces");
}

#[test]
fn nonsense_is_refused_so_the_caller_can_fall_back() {
    for bad in [
        "I think we should run it every hour!",         // no object at all
        r#"{"when":{"kind":"cron","cron":"nope"}}"#,    // unreadable cron
        r#"{"when":{"kind":"every","every_seconds":5}}"#, // a 5-second "schedule"
        r#"{"when":{"kind":"once"}}"#,                  // no time given
        "{",                                             // truncated
    ] {
        assert!(plan(bad).is_none(), "{bad:?} must not decode");
    }
}

#[test]
fn a_missing_task_keeps_the_original_request() {
    let p = plan(r#"{"when":{"kind":"now"}}"#).unwrap();
    assert_eq!(p.task, "the original request");
    assert!(p.says.starts_with("now"));
}
