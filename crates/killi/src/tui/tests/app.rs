use super::*;

#[test]
fn replayed_items_print_as_they_ran() {
    let mut app = test_app();
    app.replay_items(vec![
        Replayed::Note("earlier prompts are folded".into()),
        Replayed::Prompt("do the thing".into()),
        Replayed::Text("done\n".into()),
    ]);
    let rows: Vec<String> = app.pending.iter().map(|row| unstyled(row)).collect();
    assert!(
        rows.iter().any(|row| row == "earlier prompts are folded"),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("do the thing")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row == "done"), "{rows:?}");
}

#[test]
fn ctrl_s_sends_the_typed_text_once_with_its_images() {
    let image = |n: u32| Image {
        label: format!("[Image #{n}]"),
        url: format!("data:image/png;base64,{n}"),
    };
    let mut app = test_app();
    assert!(app.send_now_prompt().is_none());
    app.controls.steer.borrow_mut().push_back((
        "first [Image #1]\n\n<system-reminder>expanded</system-reminder>".into(),
        vec![image(1), image(2)],
    ));
    app.queued.push("first [Image #1]".into());
    app.queued.push("second".into());
    let (text, images) = app.send_now_prompt().unwrap();
    assert_eq!(
        text, "first [Image #1]\n\nsecond",
        "the typed text, not the expansion"
    );
    assert_eq!(
        images,
        vec![image(1)],
        "only the images the text still names"
    );
    assert!(app.send_now_prompt().is_none(), "the queue is drained");
}

#[test]
fn a_new_or_resumed_session_starts_with_a_clean_slate() {
    let mut app = test_app();
    app.controls
        .steer
        .borrow_mut()
        .push_back(("queued".into(), Vec::new()));
    app.queued.push("queued".into());
    app.send_now = true;
    app.todos.push(Todo {
        content: "old".into(),
        status: "pending".into(),
    });
    app.status.prompt = 10;
    app.status.context = 10;
    app.details.push(("title".into(), "body".into()));
    app.controls
        .peer_notes
        .borrow_mut()
        .push_back("note".into());
    app.peer_tells = 2;
    let root = crate::test_temp::TempDir::new("killi-reset");
    let mut snapshots = Snapshots::new(&root);
    app.reset_conversation(&mut snapshots, &root);
    assert!(app.controls.steer.borrow().is_empty());
    assert!(app.queued.is_empty() && !app.send_now);
    assert!(app.todos.is_empty());
    assert_eq!((app.status.prompt, app.status.context), (0, 0));
    assert!(app.details.is_empty() && app.detail_back == 0);
    assert!(app.controls.peer_notes.borrow().is_empty() && app.peer_tells == 0);
    assert!(snapshots.last_taken().is_none());
}

#[test]
fn no_notification_for_a_turn_the_user_ended() {
    let mut app = test_app();
    app.last_end = Some(LastEnd::Interrupted);
    assert!(
        !app.wants_notify(Duration::from_secs(30)),
        "an Esc-ended turn stays quiet"
    );
    app.last_end = Some(LastEnd::Done);
    assert!(app.wants_notify(Duration::from_secs(30)));
    assert!(
        !app.wants_notify(Duration::from_secs(1)),
        "a quick turn stays quiet"
    );
    app.send_now = true;
    assert!(
        !app.wants_notify(Duration::from_secs(30)),
        "a Ctrl+S turn stays quiet"
    );
}

#[test]
fn queued_messages_come_back_with_their_images() {
    let mut app = test_app();
    let image = Image {
        label: "[Image #1]".into(),
        url: "data:image/png;base64,AA".into(),
    };
    restore_steered(
        &mut app.editor,
        vec![
            ("see this [Image #1] please".into(), vec![image]),
            ("second".into(), Vec::new()),
        ],
    );
    let (text, images) = app.editor.take_with_images();
    assert!(
        text.contains("see this") && text.contains("please") && text.contains("second"),
        "{text}"
    );
    assert!(text.contains("\n\n"), "{text}");
    assert_eq!(images.len(), 1);
    assert!(
        text.contains(&images[0].label),
        "the placeholder matches the kept image: {text}"
    );
}

#[test]
fn a_failed_command_shows_its_gist_and_the_hint_its_kind_earns() {
    let mut app = test_app();
    let busy = anyhow::Error::new(ProviderError {
        status: Some(503),
        kind: ErrorKind::Other,
        message: "busy".into(),
        attempts: 1,
        advice: String::new(),
    });
    app.error_from("Compaction failed", &busy);
    let shown = app
        .pending
        .iter()
        .map(|row| unstyled(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(shown.contains("Compaction failed: busy"), "{shown}");
    app.pending.clear();
    let overflow = anyhow::Error::new(ProviderError {
        status: None,
        kind: ErrorKind::Overflow,
        message: "too long".into(),
        attempts: 1,
        advice: String::new(),
    });
    app.error_from("Compaction failed", &overflow);
    let shown = app
        .pending
        .iter()
        .map(|row| unstyled(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(shown.contains("/compact summarizes it"), "{shown}");
}

#[test]
fn the_status_line_keeps_the_context_figure_before_swarm_mode() {
    let mut app = test_app();
    app.controls.swarm.set(true);
    app.status = Status {
        model: "qwen3.7-plus".into(),
        thinking: "high".into(),
        window: Some(200_000),
        context: 38_000,
        prompt: 38_000,
        cached: 30_000,
        completion: 2_000,
        ..Status::default()
    };
    let line = unstyled(&app.status_line(48));
    assert!(line.contains("ctx") && !line.contains("swarm"), "{line}");
}

#[test]
fn ctrl_l_starts_the_screen_over() {
    let mut app = test_app();
    app.content_rows = 24;
    app.edit_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert_eq!(app.content_rows, 0);
}

#[test]
fn ctrl_c_arms_an_exit_hint_instead_of_printing_one() {
    let mut app = test_app();
    app.on_key_idle(TermEvent::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));
    assert!(app.exit_armed.is_some());
    assert!(
        app.pending.is_empty(),
        "the transcript keeps only what was done"
    );
    let (rows, _, _) = app.live();
    assert!(
        rows.iter()
            .any(|row| unstyled(row).contains("Press Ctrl+C again to exit"))
    );
    app.exit_armed = Some(Instant::now() - Duration::from_secs(3));
    let (rows, _, _) = app.live();
    assert!(
        !rows
            .iter()
            .any(|row| unstyled(row).contains("Press Ctrl+C"))
    );
}

#[test]
fn bare_effort_while_busy_offers_the_levels() {
    let mut app = test_app();
    let item = |value: &str| Item {
        value: value.into(),
        description: String::new(),
    };
    app.effort_choices = vec![item("off"), item("high")];
    assert!(!app.command_while_busy("effort", ""));
    assert_eq!(app.editor.text(), "/effort ");
    assert!(app.command_while_busy("effort", "high"));
    assert_eq!(app.effort_later.as_deref(), Some("high"));
}

#[test]
fn a_plan_waiting_for_approval_reads_as_waiting_on_you() {
    let mut app = test_app();
    app.busy = Some(Instant::now());
    app.running.push(Running {
        id: "1".into(),
        summary: "exit_plan_mode the plan".into(),
        label: None,
        started: Instant::now(),
    });
    let (rows, _, _) = app.live();
    assert!(
        rows.iter()
            .any(|row| unstyled(row).contains("exit_plan_mode")),
        "the call ticks while it runs"
    );
    let (reply, _) = oneshot::channel::<String>();
    app.asking = Some(Asking {
        question: Question {
            text: "Plan?".into(),
            options: vec!["Approve and start".into()],
            plan: true,
            reply,
        },
        selected: 0,
        internal: false,
        secret: false,
    });
    let (rows, _, _) = app.live();
    let shown = rows
        .iter()
        .map(|row| unstyled(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !shown.contains("exit_plan_mode"),
        "approval waits on you, not on a ticking call: {shown}"
    );
    assert!(shown.contains("Approve and start"), "{shown}");
}

#[test]
fn a_long_ask_option_wraps_instead_of_truncating() {
    let mut app = test_app();
    app.width = 60;
    app.height = 24;
    let (reply, _) = oneshot::channel::<String>();
    app.asking = Some(Asking {
            question: Question {
                text: "Which resolution do you want?".into(),
                options: vec![
                    "Move both `subagent_call` and `subworkflow_call` from Logic to AI (smallest diff, 1 file)".into(),
                    "Something else - tell me what you meant.".into(),
                ],
                plan: false,
                reply,
            },
            selected: 0,
            internal: false,
            secret: false,
        });
    let (rows, _, _) = app.live();
    let shown = rows
        .iter()
        .map(|row| unstyled(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rows.iter().all(|row| text::width(row) <= 60), "{shown}");
    assert!(shown.contains("subworkflow_call"), "{shown}");
    assert!(
        !shown.contains('…'),
        "nothing is cut with an ellipsis: {shown}"
    );
}

#[test]
fn left_and_right_walk_answered_questions_and_a_change_rides_the_next_answer() {
    let mut app = test_app();
    app.width = 80;
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let ask = |text: &str, options: &[&str]| {
        let (reply, answer) = oneshot::channel::<String>();
        (
            Asking {
                question: Question {
                    text: text.into(),
                    options: options.iter().map(|o| o.to_string()).collect(),
                    plan: false,
                    reply,
                },
                selected: 0,
                internal: false,
                secret: false,
            },
            answer,
        )
    };
    let (asking, mut first) = ask("Delete all?", &["all", "biggest", "none"]);
    app.asking = Some(asking);
    app.on_key_asking(key(KeyCode::Down));
    app.on_key_asking(key(KeyCode::Tab));
    assert_eq!(first.try_recv().unwrap(), "biggest");
    let (asking, mut second) = ask("Keep logs?", &["yes", "no"]);
    app.asking = Some(asking);
    let (rows, _, _) = app.live();
    let shown: Vec<String> = rows.iter().map(|row| unstyled(row)).collect();
    assert!(
        shown.iter().any(|row| row.contains("← previous question")),
        "{shown:?}"
    );
    app.on_key_asking(key(KeyCode::Left));
    assert_eq!(app.ask_view, Some(0));
    let (rows, _, _) = app.live();
    let shown: Vec<String> = rows.iter().map(|row| unstyled(row)).collect();
    assert!(
        shown
            .iter()
            .any(|row| row.contains("question 1/2") && row.contains("Delete all?")),
        "{shown:?}"
    );
    assert!(
        shown.iter().any(|row| row.contains("answered: biggest")),
        "{shown:?}"
    );
    app.on_key_asking(key(KeyCode::Up));
    app.on_key_asking(key(KeyCode::Tab));
    assert_eq!(
        app.ask_view, None,
        "confirming a change returns to the waiting question"
    );
    assert_eq!(app.ask_corrections.len(), 1);
    app.on_key_asking(key(KeyCode::Left));
    app.on_key_asking(key(KeyCode::Right));
    assert_eq!(
        app.ask_view, None,
        "→ past the last answered question returns to the waiting one"
    );
    app.on_key_asking(key(KeyCode::Tab));
    let payload = second.try_recv().unwrap();
    assert!(
        payload.contains("更正之前的问题「Delete all?」：答案改为「all」（原为「biggest」）"),
        "{payload}"
    );
    assert!(payload.ends_with("本问答案：yes"), "{payload}");
    assert_eq!(app.ask_past.len(), 2);
    assert!(app.ask_corrections.is_empty());
}

#[test]
fn the_live_area_merges_the_thought_into_the_action_row() {
    let mut app = test_app();
    app.busy = Some(Instant::now());
    app.thinking = Some((
        Instant::now(),
        "The internal § references are relative.".into(),
    ));
    app.spinner = 30;
    app.width = 80;
    let (rows, _, _) = app.live();
    let shown: Vec<String> = rows.iter().map(|row| unstyled(row)).collect();
    assert!(
        shown
            .iter()
            .any(|row| row.ends_with("thinking The internal § references are relative.")),
        "{shown:?}"
    );
    assert!(
        shown.iter().any(|row| row == "0s · Esc to interrupt"),
        "{shown:?}"
    );
    assert!(
        !shown.iter().any(|row| row.contains('∴')),
        "the old thinking row is gone: {shown:?}"
    );
}

#[test]
fn a_running_snapshot_shows_a_whole_renaissance_word_not_snapshot() {
    let mut app = test_app();
    app.busy = Some(Instant::now());
    app.snapshotting = true;
    app.spinner = 8;
    app.width = 80;
    let (rows, _, _) = app.live();
    let shown: Vec<String> = rows.iter().map(|row| unstyled(row)).collect();
    assert!(
        shown.iter().any(|row| row.ends_with("chiaroscuro")),
        "{shown:?}"
    );
    assert!(
        !shown.iter().any(|row| row.contains("snapshot")),
        "{shown:?}"
    );
}

#[test]
fn a_full_live_area_shrinks_tasks_and_queue_before_the_running_call() {
    let mut app = test_app();
    app.height = 16;
    app.busy = Some(Instant::now());
    app.running.push(Running {
        id: "1".into(),
        summary: "bash cargo test".into(),
        label: None,
        started: Instant::now(),
    });
    app.queued = vec!["first".into(), "second".into()];
    app.todos = (0..8)
        .map(|n| Todo {
            content: format!("task {n}"),
            status: "pending".into(),
        })
        .collect();
    let (rows, _, _) = app.live();
    let shown = rows
        .iter()
        .map(|row| unstyled(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rows.len() <= 15, "{shown}");
    assert!(
        shown.contains("cargo test"),
        "the running call stays: {shown}"
    );
    assert!(
        shown.contains("Esc to interrupt"),
        "the spinner row stays: {shown}"
    );
    assert!(
        shown.contains("first"),
        "the queue stays while the task list shrinks: {shown}"
    );
    assert!(
        !shown.contains("task 7"),
        "the task list shrank first: {shown}"
    );
}

#[test]
fn parallel_subagents_with_the_same_call_id_keep_their_own_rows() {
    let mut app = test_app();
    let start = |id: &str| Event::ToolStart {
        id: id.into(),
        summary: "bash make".into(),
    };
    let end = |id: &str| Event::ToolEnd {
        id: id.into(),
        is_error: false,
        brief: "done".into(),
        display: None,
        output: "ok".into(),
    };
    app.on_sub("one".into(), None, start("call_1"));
    app.on_sub("two".into(), None, start("call_1"));
    assert_eq!(app.running.len(), 2);
    app.on_sub("two".into(), None, end("call_1"));
    assert_eq!(app.running.len(), 1);
    assert_eq!(app.running[0].label.as_deref(), Some("one"));
}

#[test]
fn model_text_cannot_move_the_cursor() {
    let evil = "a\x1b]0;pwned\x07b\x1b[2Jc\x1b[3;7Hd\x07e\x1bf\r\tg\n**ok**\x1b[0K";
    let mut stream = text::Sanitizer::default();
    let mut partial = stream.push(evil);
    partial.push_str(&stream.finish());
    let mut markdown = Markdown::default();
    let mut history = History::new(80);
    for line in partial.split('\n') {
        markdown
            .line(line)
            .iter()
            .for_each(|row| history.push(Line::Row(row.clone())));
    }
    let shown: Vec<String> = history.rows().to_vec();
    assert!(
        shown.iter().any(|row| unstyled(row) == "abcde   g"),
        "{shown:?}"
    );
    assert!(shown.iter().any(|row| unstyled(row) == "ok"), "{shown:?}");
    for row in &shown {
        let bare = without_own_sgr(row);
        assert!(
            !bare.contains('\x1b') && !bare.chars().any(char::is_control),
            "{row:?}"
        );
    }
}

#[test]
fn a_retry_ends_the_partial_reply_in_its_own_row() {
    let mut app = test_app();
    app.thinking = Some((Instant::now(), "hmm".to_string()));
    app.on_agent(Event::Text("Here is the fi".to_string()));
    assert!(
        app.pending
            .iter()
            .all(|row| unstyled(row) != "Here is the fi"),
        "no newline yet, nothing committed"
    );
    app.on_agent(Event::Retry {
        attempt: 2,
        wait: Duration::from_secs(1),
        reason: "busy".to_string(),
    });
    assert!(
        app.thinking.is_none(),
        "the failed attempt's thinking is dropped"
    );
    app.on_agent(Event::Text("Here is the fix\n".to_string()));
    let rows: Vec<String> = app.pending.iter().map(|row| unstyled(row)).collect();
    let first = rows
        .iter()
        .position(|row| row == "Here is the fi")
        .expect("the partial reply is committed: {rows:?}");
    let second = rows
        .iter()
        .position(|row| row == "Here is the fix")
        .expect("the retried reply follows: {rows:?}");
    assert!(
        first < second && rows[first + 1..second].iter().any(|row| !row.is_empty()),
        "two separate rows: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("discarded")),
        "the retry says the partial reply was dropped: {rows:?}"
    );
}

#[test]
fn a_notice_ends_the_streamed_line_first() {
    let mut app = test_app();
    app.on_agent(Event::Text("hello".to_string()));
    app.on_agent(Event::Notice("a notice".to_string()));
    app.on_agent(Event::Text("world\n".to_string()));
    let rows: Vec<String> = app.pending.iter().map(|row| unstyled(row)).collect();
    let hello = rows
        .iter()
        .position(|row| row == "hello")
        .expect("the streamed line is committed: {rows:?}");
    let note = rows
        .iter()
        .position(|row| row == "a notice")
        .expect("the notice follows: {rows:?}");
    let world = rows
        .iter()
        .position(|row| row == "world")
        .expect("the continued reply follows: {rows:?}");
    assert!(
        hello < note && note < world && hello + 1 < world,
        "separate blocks, never glued: {rows:?}"
    );
}

#[test]
fn the_branch_is_reread_at_most_every_few_seconds() {
    let mut app = test_app();
    app.status.branch = Some("main*".into());
    app.branch_at = Some(Instant::now());
    app.refresh_branch();
    assert_eq!(
        app.status.branch.as_deref(),
        Some("main*"),
        "a fresh reading is kept"
    );
    app.branch_at = Some(Instant::now() - Duration::from_secs(6));
    app.refresh_branch();
    assert!(
        app.branch_at
            .is_some_and(|at| at.elapsed() < Duration::from_secs(5)),
        "a stale reading runs again"
    );
}

#[test]
fn outside_a_repository_there_is_no_branch() {
    let root = crate::test_temp::TempDir::new("killi-norepo");
    assert_eq!(App::branch_of(&root), None);
}

#[test]
fn the_file_menu_filters_once_per_query() {
    let mut app = test_app();
    app.files = Some(vec!["src/main.rs".into(), "src/tui/mod.rs".into()]);
    app.files_gen = 1;
    app.editor.insert("see @srct");
    let first = app.suggestions().expect("the query matches");
    assert!(app.file_menu_cache.is_some(), "the filter is remembered");
    assert_eq!(
        app.suggestions(),
        Some(first.clone()),
        "the same query reuses it"
    );
    app.files = Some(vec!["docs/other.md".into()]);
    app.files_gen = 2;
    assert_ne!(
        app.suggestions(),
        Some(first),
        "a new listing filters again"
    );
}

#[test]
fn the_streaming_preview_is_styled_once_per_tail() {
    let mut app = test_app();
    app.on_agent(Event::Text("hello *world*".into()));
    let (first, _, _) = app.live();
    let cached = app
        .preview_cache
        .clone()
        .expect("the tail's rows are remembered");
    assert_eq!(cached.0, "hello *world*");
    let (second, _, _) = app.live();
    assert_eq!(first, second);
    assert_eq!(
        app.preview_cache.as_ref().map(|c| &c.3),
        Some(&cached.3),
        "the same tail reuses its rows"
    );
    app.on_agent(Event::Text("!\n".into()));
    let (third, _, _) = app.live();
    assert!(
        app.preview_cache.is_none(),
        "a committed line drops the preview"
    );
    assert_ne!(first, third);
}

#[test]
fn the_busy_menu_offers_skills_and_skills_queue_their_prompt() {
    let mut app = test_app();
    app.busy = Some(Instant::now());
    app.entries = vec![
        Entry {
            name: "new".into(),
            aliases: Vec::new(),
            description: String::new(),
        },
        Entry {
            name: "btw".into(),
            aliases: Vec::new(),
            description: String::new(),
        },
        Entry {
            name: "demo-skill".into(),
            aliases: Vec::new(),
            description: "does demos".into(),
        },
    ];
    app.editor.insert("/");
    let names = |app: &mut App| {
        let menu = app.suggestions().expect("the menu opens while busy");
        menu.items
            .iter()
            .map(|i| i.value.as_str().to_string())
            .collect::<Vec<_>>()
    };
    let all = names(&mut app);
    assert!(
        all.contains(&"demo-skill".to_string()),
        "skills are offered while busy: {all:?}"
    );
    assert!(
        !all.contains(&"new".to_string()),
        "a command that can't run now isn't: {all:?}"
    );
    assert!(
        all.contains(&"btw".to_string()),
        "a command that runs now still is: {all:?}"
    );
    app.editor.insert("demo");
    assert_eq!(
        names(&mut app),
        vec!["demo-skill".to_string()],
        "typing narrows to the skill"
    );

    let dir = crate::test_temp::TempDir::new("killi-busy-skill");
    for (name, body) in [("demo-skill", "Be brief."), ("other-skill", "Be kind.")] {
        let folder = dir.path().join(format!(".agents/skills/{name}"));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: does things\n---\n{body}"),
        )
        .unwrap();
    }
    app.skills = crate::skills::Skills::discover(dir.path(), &["no-such-dir".to_string()]);
    app.editor.clear();
    app.editor.insert("/demo-skill make one");
    app.send_busy();
    assert_eq!(app.queued, ["/demo-skill make one".to_string()]);
    let steered = app.controls.steer.borrow();
    let (sent, images) = &steered[0];
    assert!(
        sent.contains("Use the demo-skill skill for this.")
            && sent.contains("Be brief.")
            && sent.ends_with("make one"),
        "{sent}"
    );
    assert!(images.is_empty());
    // A second skill in the same prompt, on the next line or after the completed name's
    // space, becomes its own block.
    drop(steered);
    app.controls.steer.borrow_mut().clear();
    app.queued.clear();
    for text in [
        "/demo-skill make one\n/other-skill make two",
        "/demo-skill /other-skill both",
    ] {
        app.controls.steer.borrow_mut().clear();
        app.queued.clear();
        app.editor.clear();
        app.editor.insert(text);
        app.send_busy();
        let steered = app.controls.steer.borrow();
        let (sent, _) = steered.back().unwrap();
        assert!(
            sent.contains("Use the demo-skill skill for this.")
                && sent.contains("Use the other-skill skill for this.")
                && sent.contains("Be kind."),
            "from {text}: {sent}"
        );
    }
    assert!(app.editor.is_empty(), "the prompt was taken");
}

#[test]
fn a_held_prompt_is_printed_before_anything_its_turn_prints() {
    let mut app = test_app();
    app.begin_turn("fix the slug", &[]);
    let shows = |rows: &[String]| rows.iter().any(|r| unstyled(r).contains("fix the slug"));
    assert!(!shows(&app.pending), "held, not printed yet");
    assert!(shows(&app.live().0), "the live area draws it meanwhile");

    app.notice("Compacted.");
    let printed: Vec<String> = app.pending.iter().map(|l| unstyled(l)).collect();
    let prompt = printed.iter().position(|l| l.contains("fix the slug"));
    let notice = printed.iter().position(|l| l.contains("Compacted."));
    assert!(prompt.is_some() && prompt < notice, "{printed:?}");
    assert!(app.held_prompt.is_none());
    assert!(!shows(&app.live().0), "drawn once, in the scrollback");
}
