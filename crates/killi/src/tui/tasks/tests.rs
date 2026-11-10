use super::*;
use crate::tui::tests::{test_app, unstyled};

/// A task as the session keeps it, in `state`, that ended at `ended`.
fn kept(id: &str, state: live::TaskState, ended: Option<u64>) -> PeerTask {
    PeerTask {
        view: Rc::new(RefCell::new(live::TaskView {
            id: id.into(),
            state,
            ..live::TaskView::default()
        })),
        waiting: None,
        watchers: Rc::default(),
        cancel: None,
        stopped: false,
        ended,
    }
}

#[test]
fn a_final_line_says_where_the_task_stands() {
    assert_eq!(
        outcome_of("All done.\nStatus: ready").0,
        live::TaskState::Completed
    );
    assert_eq!(
        outcome_of("I need the branch name.\nStatus: blocked which branch?").0,
        live::TaskState::InputRequired
    );
    assert_eq!(
        outcome_of("Waiting on the build.\nStatus: waiting")
            .1
            .as_deref(),
        Some("it reported `Status: waiting`")
    );
    assert_eq!(
        outcome_of("It blew up.\nStatus: failed cargo test exits 101").0,
        live::TaskState::Failed
    );
    assert_eq!(
        outcome_of("It blew up.\nStatus: failed cargo test exits 101")
            .1
            .as_deref(),
        Some("cargo test exits 101")
    );
    // No status line: the report is the answer.
    assert_eq!(
        outcome_of("Done, all three files changed.").0,
        live::TaskState::Completed
    );
}

#[test]
fn an_event_line_carries_the_step_it_is_on() {
    // serde_json writes a map's keys in order, so `message` comes before `state`.
    assert_eq!(
        live::event_line(live::TaskState::Working, Some("read src/main.rs")),
        r#"{"message":"read src/main.rs","state":"working"}"#
    );
    assert_eq!(
        live::event_line(live::TaskState::Submitted, None),
        r#"{"state":"submitted"}"#
    );
}

#[test]
fn a_dressed_up_final_line_still_says_where_the_task_stands() {
    use live::TaskState::{Completed, Failed, InputRequired};
    for (report, state, why) in [
        (
            "It blew up.\n`Status: failed` cargo test exits 101",
            Failed,
            Some("cargo test exits 101"),
        ),
        (
            "Stuck.\n**Status: blocked**",
            InputRequired,
            Some("it reported `Status: blocked`"),
        ),
        (
            "Stuck.\n**Status:** blocked — which branch?",
            InputRequired,
            Some("which branch?"),
        ),
        ("Stuck.\nStatus: blocked: need X", InputRequired, Some("need X")),
        (
            "It blew up.\nStatus: Failed. The build broke\n\n",
            Failed,
            Some("The build broke"),
        ),
        ("Done.\nStatus: ready.", Completed, None),
        ("Done.\nstatus: Ready\n", Completed, None),
        ("Done.\n```\nStatus: ready\n```", Completed, None),
        // A word the convention doesn't know is not a finished task.
        (
            "Hm.\nStatus: unsure",
            Failed,
            Some("it reported `Status: unsure`"),
        ),
        // Text that only mentions a status is a report like any other.
        ("The status: all three pass — é", Completed, None),
    ] {
        let (found, said) = outcome_of(report);
        assert_eq!(found, state, "{report:?}");
        assert_eq!(said.as_deref(), why, "{report:?}");
    }
}

#[test]
fn undo_names_the_tasks_that_worked_since_its_snapshot() {
    let mut app = test_app();
    app.peer_tasks = vec![
        kept("7-1", live::TaskState::Completed, Some(100)),
        kept("7-2", live::TaskState::Canceled, Some(300)),
        kept("7-3", live::TaskState::Working, None),
    ];
    assert_eq!(
        app.tasks_since(Some(200)),
        ["task 7-2", "task 7-3"],
        "the one that ended before the snapshot changed nothing since"
    );
    assert!(
        app.tasks_since(None).is_empty(),
        "no snapshot, nothing to undo"
    );
    assert_eq!(app.tasks_running(), 1);
}

#[test]
fn a_task_line_waits_for_the_paragraph_the_model_is_writing() {
    let mut app = test_app();
    let shown = |app: &App, text: &str| app.pending.iter().any(|row| unstyled(row) == text);
    app.busy = Some(Instant::now());
    app.on_agent(Event::Text("The fix is in pla".to_string()));
    app.task_notice("Task 7-1 completed.");
    assert!(
        !shown(&app, "Task 7-1 completed."),
        "held: {:?}",
        app.pending
    );
    app.on_agent(Event::Text("ce.\n\nNext".to_string()));
    let rows: Vec<String> = app.pending.iter().map(|row| unstyled(row)).collect();
    let paragraph = rows.iter().position(|row| row == "The fix is in place.");
    let line = rows.iter().position(|row| row == "Task 7-1 completed.");
    assert!(
        paragraph.is_some() && paragraph < line,
        "the line comes under the whole paragraph: {rows:?}"
    );

    // Nothing is being written: the line is printed at once.
    let mut idle = test_app();
    idle.task_notice("Task 7-2 completed.");
    assert!(shown(&idle, "Task 7-2 completed."), "{:?}", idle.pending);
}
