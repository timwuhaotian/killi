use super::*;

#[test]
fn pair_turns_stand_apart_and_keep_their_details_at_narrow_widths() {
    for width in [60, 80] {
        let mut app = test_app();
        app.width = width;
        app.on_agent(Event::Text("Previous report".into()));
        app.on_agent(Event::PairTurn {
            role: Role::Executor,
            model: "commandcode/meta/muse-spark-1.3-contributor".into(),
            effort: "high".into(),
            what: "round 1".into(),
        });
        let rows: Vec<String> = app.pending.iter().map(|row| unstyled(row)).collect();
        assert!(
            rows.iter().any(|row| row.contains("→ Executor · round 1")),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("commandcode/meta/muse-spark-1.3-contributor (high)")),
            "{rows:?}"
        );
        assert!(app.pending.iter().all(|row| text::width(row) <= width));
        assert!(
            app.pending
                .iter()
                .any(|row| row.contains(BOLD) && row.contains("Executor"))
        );
    }
}

#[test]
fn typed_text_interjects_to_the_driving_pair() {
    let mut app = test_app();
    app.pair_ctl = Some(PairControls::default());
    app.editor.insert("go left");
    app.send_busy();
    assert!(
        app.controls.steer.borrow().len() == 1,
        "a paused pair leaves typing to the session"
    );
    assert!(!app.take_pair_interrupt());
    app.pair_driving = true;
    app.editor.insert("go right");
    app.send_busy();
    let ctl = app.pair_ctl.as_ref().unwrap();
    assert_eq!(ctl.interject.borrow().len(), 1);
    assert_eq!(ctl.interject.borrow()[0].0, "go right");
    assert!(app.take_pair_interrupt(), "the turn in flight is interrupted");
    assert_eq!(
        app.controls.steer.borrow().len(),
        1,
        "nothing more queues for the session"
    );
}

#[test]
fn pair_picks_advance_the_setup_to_the_task() {
    let mut app = test_app();
    let config = Config::default();
    app.pair_setup = Some(PairSetup {
        step: PairStep::MentorModel,
        mentor_model: String::new(),
        mentor_effort: String::new(),
        executor_model: String::new(),
        executor_effort: String::new(),
        config_only: None,
    });
    let step = |app: &App| app.pair_setup.as_ref().unwrap().step;
    pair_pick(&mut app, &config, Role::Mentor, "m", false);
    assert_eq!(step(&app), PairStep::MentorEffort);
    assert!(app.picker.is_some());
    pair_pick(&mut app, &config, Role::Mentor, "high", true);
    assert_eq!(step(&app), PairStep::ExecutorModel);
    pair_pick(&mut app, &config, Role::Executor, "e", false);
    assert_eq!(step(&app), PairStep::ExecutorEffort);
    pair_pick(&mut app, &config, Role::Executor, "low", true);
    assert_eq!(step(&app), PairStep::Task);
    assert_eq!(app.pair_cfg.mentor_model, "m");
    assert_eq!(app.pair_cfg.mentor_effort, "high");
    assert_eq!(app.pair_cfg.executor_model, "e");
    assert_eq!(app.pair_cfg.executor_effort, "low");
    pair_pick(&mut app, &config, Role::Mentor, "x", false);
    assert_eq!(step(&app), PairStep::Task, "a stale pick is dropped");
}

#[test]
fn a_config_only_pick_saves_the_role_and_ends_the_setup() {
    let mut app = test_app();
    let config = Config::default();
    app.pair_setup = Some(PairSetup {
        step: PairStep::ExecutorModel,
        mentor_model: "m".into(),
        mentor_effort: "high".into(),
        executor_model: String::new(),
        executor_effort: String::new(),
        config_only: Some(Role::Executor),
    });
    pair_pick(&mut app, &config, Role::Executor, "e", false);
    pair_pick(&mut app, &config, Role::Executor, "low", true);
    assert!(app.pair_setup.is_none());
    assert_eq!(app.pair_cfg.executor_model, "e");
    assert_eq!(app.pair_cfg.executor_effort, "low");
    assert_eq!(app.pair_cfg.mentor_model, "m");
}
