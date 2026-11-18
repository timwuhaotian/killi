use super::*;

#[test]
fn a_draft_defers_a_due_task_without_consuming_it() {
    let mut app = crate::tui::tests::test_app();
    let past = Instant::now().checked_sub(MIN_INTERVAL).unwrap();
    let id = app
        .schedules
        .add(MIN_INTERVAL, "check status".into(), past)
        .unwrap();
    app.editor.insert("unfinished prompt");
    assert!(app.take_scheduled().is_none());
    assert!(app.schedules.next_due().is_some());
    app.editor.take();
    assert!(matches!(
        app.take_scheduled(),
        Some(Input::Schedule(actual, task)) if actual == id && task == "check status"
    ));
    assert!(app.take_scheduled().is_none());
}

#[test]
fn model_fixtures_keep_the_original_task() {
    for (arg, reply, seconds, task) in [
        (
            "2 hours check status",
            r#"{"interval":"2 hours","seconds":7200}"#,
            7200,
            "check status",
        ),
        (
            "1h check builds",
            r#"{"interval":"1h","seconds":3600}"#,
            3600,
            "check builds",
        ),
        (
            "30 mins check status",
            r#"{"interval":"30 mins","seconds":1800}"#,
            1800,
            "check status",
        ),
        (
            "half an hour check logs\n  Preserve indent.",
            r#"{"interval":"half an hour","seconds":1800}"#,
            1800,
            "check logs\n  Preserve indent.",
        ),
        (
            "每隔两小时\u{3000}检查发布\n  汇报状态 🐙  ",
            r#"{"interval":"每隔两小时","seconds":7200}"#,
            7200,
            "检查发布\n  汇报状态 🐙  ",
        ),
    ] {
        assert_eq!(
            parse_reply(arg, reply).unwrap(),
            (Duration::from_secs(seconds), task.to_string())
        );
    }
}

#[test]
fn rejects_malformed_replies_and_bad_prefixes() {
    for reply in [
        "",
        "null",
        "```json\n{\"interval\":\"1 hour\",\"seconds\":3600}\n```",
        r#"["1 hour",3600]"#,
        r#"{"interval":"1 hour"}"#,
        r#"{"seconds":3600}"#,
        r#"{"interval":"1 hour","seconds":"3600"}"#,
        r#"{"interval":"1 hour","seconds":-3600}"#,
        r#"{"interval":"1 hour","seconds":3600.0}"#,
        r#"{"interval":"1 hour","seconds":18446744073709551616}"#,
        r#"{"interval":"1 hour","seconds":3600,"task":"rewritten"}"#,
        r#"{"interval":"1 hour","seconds":3600,"seconds":7200}"#,
        r#"{"interval":"","seconds":3600}"#,
        r#"{"interval":"1 hour ","seconds":3600}"#,
        r#"{"interval":"an hour","seconds":3600}"#,
        r#"{"interval":"1 hou","seconds":3600}"#,
    ] {
        assert!(
            parse_reply("1 hour check status", reply).is_err(),
            "{reply}"
        );
    }
    let reply = r#"{"interval":"1 hour","seconds":3600}"#;
    for arg in [
        "",
        "1 hour",
        "1 hour \n\t",
        "check status 1 hour",
        "1 hourcheck status",
    ] {
        assert!(parse_reply(arg, reply).is_err(), "{arg:?}");
    }
}

#[test]
fn parsing_enforces_interval_limits() {
    for (prefix, seconds, allowed) in [
        ("0 seconds", 0, false),
        ("59 seconds", 59, false),
        ("1 minute", 60, true),
        ("7 days", 604_800, true),
        ("604801 seconds", 604_801, false),
    ] {
        let arg = format!("{prefix} check status");
        let reply = json!({"interval": prefix, "seconds": seconds}).to_string();
        assert_eq!(parse_reply(&arg, &reply).is_ok(), allowed, "{prefix}");
    }
}

#[test]
fn dispatches_by_due_time_then_id() {
    let mut schedule = Schedule::default();
    let now = Instant::now();
    assert_eq!(schedule.next_due(), None);
    assert_eq!(schedule.take_due(now), None);
    let later = schedule.add(MIN_INTERVAL * 2, "later".into(), now).unwrap();
    let first = schedule.add(MIN_INTERVAL, "first".into(), now).unwrap();
    let second = schedule.add(MIN_INTERVAL, "second".into(), now).unwrap();
    let due = now + MIN_INTERVAL;
    assert_eq!(schedule.next_due(), Some(due));
    assert_eq!(schedule.take_due(due - Duration::from_millis(1)), None);
    assert_eq!(schedule.take_due(due), Some((first, "first".into())));
    assert_eq!(schedule.take_due(due), Some((second, "second".into())));
    assert_eq!(schedule.take_due(due), None);
    // At the next tie, the job registered first wins despite having a longer interval.
    assert_eq!(
        schedule.take_due(due + MIN_INTERVAL),
        Some((later, "later".into()))
    );
}

#[test]
fn coalesces_missed_ticks_from_actual_dispatch() {
    let mut schedule = Schedule::default();
    let now = Instant::now();
    let id = schedule.add(MIN_INTERVAL, "check".into(), now).unwrap();
    let late = now + MIN_INTERVAL * 9 + Duration::from_secs(30);
    assert_eq!(schedule.take_due(late), Some((id, "check".into())));
    assert_eq!(schedule.next_due(), Some(late + MIN_INTERVAL));
    assert_eq!(schedule.take_due(late), None);
    assert_eq!(
        schedule.take_due(late + MIN_INTERVAL - Duration::from_secs(1)),
        None
    );
    assert_eq!(
        schedule.take_due(late + MIN_INTERVAL),
        Some((id, "check".into()))
    );
}

#[test]
fn cancel_frees_capacity_without_reusing_ids() {
    let mut schedule = Schedule::default();
    let now = Instant::now();
    for id in 1..=CAPACITY as u64 {
        assert!(!schedule.is_full());
        assert_eq!(schedule.add(MIN_INTERVAL, "check".into(), now).unwrap(), id);
    }
    assert!(schedule.is_full());
    assert!(schedule.add(MIN_INTERVAL, "extra".into(), now).is_err());
    assert!(schedule.cancel(1));
    assert!(!schedule.cancel(1));
    assert!(!schedule.is_full());
    let id = schedule
        .add(MAX_INTERVAL, "replacement".into(), now)
        .unwrap();
    assert_eq!(id, CAPACITY as u64 + 1);
    assert_eq!(schedule.jobs().count(), CAPACITY);
    assert_eq!(
        schedule.jobs().map(|job| job.id).collect::<Vec<_>>(),
        (2..=id).collect::<Vec<_>>()
    );
    assert_eq!(
        schedule.take_due(now + MIN_INTERVAL),
        Some((2, "check".into()))
    );
}

#[test]
fn add_rejects_invalid_values_without_advancing_ids() {
    let mut schedule = Schedule::default();
    let now = Instant::now();
    for interval in [
        MIN_INTERVAL - Duration::from_millis(1),
        MAX_INTERVAL + Duration::from_millis(1),
    ] {
        assert!(schedule.add(interval, "check".into(), now).is_err());
    }
    assert!(
        schedule
            .add(MIN_INTERVAL, " \n\u{3000}".into(), now)
            .is_err()
    );
    assert_eq!(schedule.jobs().count(), 0);
    assert_eq!(schedule.add(MIN_INTERVAL, "check".into(), now).unwrap(), 1);
}

#[test]
fn job_id_cannot_wrap() {
    let mut schedule = Schedule {
        last_id: u64::MAX - 1,
        ..Schedule::default()
    };
    let now = Instant::now();
    assert_eq!(
        schedule.add(MIN_INTERVAL, "last".into(), now).unwrap(),
        u64::MAX
    );
    assert!(schedule.cancel(u64::MAX));
    assert!(schedule.add(MIN_INTERVAL, "extra".into(), now).is_err());
    assert_eq!(schedule.next_due(), None);
}

#[test]
fn a_finished_turn_restarts_the_interval_and_an_interrupt_defers_due_jobs() {
    let mut schedule = Schedule::default();
    let now = Instant::now();
    let long = schedule.add(MIN_INTERVAL, "long".into(), now).unwrap();
    let other = schedule.add(MIN_INTERVAL * 2, "other".into(), now).unwrap();
    let due = now + MIN_INTERVAL;
    assert_eq!(schedule.take_due(due), Some((long, "long".into())));
    // The turn took three intervals: it doesn't run again the moment it ends.
    let ended = due + MIN_INTERVAL * 3;
    schedule.rearm(long, ended);
    assert_eq!(schedule.take_due(ended), Some((other, "other".into())));
    assert_eq!(schedule.take_due(ended), None);
    assert_eq!(schedule.next_due(), Some(ended + MIN_INTERVAL));
    // Esc after both are due again: neither starts in the interrupted turn's place.
    let stopped = ended + MIN_INTERVAL * 2;
    schedule.defer_due(stopped);
    assert_eq!(schedule.take_due(stopped), None);
    assert_eq!(
        schedule.take_due(stopped + MIN_INTERVAL),
        Some((long, "long".into()))
    );
    schedule.rearm(99, stopped);
}

#[test]
fn commands_and_shell_lines_cannot_be_scheduled() {
    let mut schedule = Schedule::default();
    let now = Instant::now();
    for task in ["/review the diff", "/compact", "!cargo test", "  /new"] {
        assert!(
            schedule.add(MIN_INTERVAL, task.into(), now).is_err(),
            "{task}"
        );
    }
    assert_eq!(schedule.jobs().count(), 0);
    assert!(
        schedule
            .add(MIN_INTERVAL, "check /tmp and say what grew".into(), now)
            .is_ok()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn esc_on_a_scheduled_turn_skips_it_without_filling_the_prompt() {
    let mut agent =
        crate::agent::tests::unanswered_agent("schedule-take-back", "check status").await;
    let mut app = crate::tui::tests::test_app();
    let past = Instant::now().checked_sub(MIN_INTERVAL).unwrap();
    let other = app
        .schedules
        .add(MIN_INTERVAL, "other".into(), past)
        .unwrap();
    app.begin_turn("check status", &[]);
    app.scheduled = Some(7);
    assert!(app.take_back(&mut agent).unwrap());
    assert!(app.editor.is_empty(), "{:?}", app.editor.text());
    assert!(
        app.take_scheduled().is_none(),
        "Esc doesn't start the next job"
    );
    assert!(app.schedules.next_due().unwrap() > Instant::now());
    assert!(app.schedules.cancel(other));
}
