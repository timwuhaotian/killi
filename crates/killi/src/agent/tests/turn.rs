use super::*;

#[tokio::test(flavor = "current_thread")]
async fn read_batches_start_together_and_write_results_in_call_order() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    let sink: Sink = Rc::new(move |event| {
        seen.borrow_mut().push(match event {
            Event::ToolStart { id, .. } => format!("start:{id}"),
            Event::ToolEnd { id, .. } => format!("end:{id}"),
            _ => return,
        });
    });
    let mut agent = idle_agent("parallel-reads", Controls::default(), sink);
    for (name, text) in [("a.txt", "alpha"), ("b.txt", "beta"), ("c.txt", "gamma")] {
        std::fs::write(agent.ctx.cwd.join(name), text).unwrap();
    }
    let calls: Vec<_> = [("a", "a.txt"), ("b", "b.txt"), ("c", "c.txt")]
        .into_iter()
        .map(|(id, path)| {
            (
                id.to_string(),
                ToolCall {
                    id: id.into(),
                    name: "read".into(),
                    arguments: json!({"path": path}).to_string(),
                },
            )
        })
        .collect();
    assert!(!agent.execute_calls(calls, false).await.unwrap());
    assert_eq!(
        *events.borrow(),
        ["start:a", "start:b", "start:c", "end:a", "end:b", "end:c"]
    );
    let results: Vec<_> = agent
        .messages
        .iter()
        .map(|m| {
            (
                m["tool_call_id"].as_str().unwrap(),
                m["content"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        results.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    for ((_, text), expected) in results.into_iter().zip(["alpha", "beta", "gamma"]) {
        assert!(text.contains(expected), "{text}");
    }
    assert_eq!(agent.stats.tool_calls, 3);
}

#[tokio::test(flavor = "current_thread")]
async fn the_first_tool_call_waits_for_an_in_flight_snapshot() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    let sink: Sink = Rc::new(move |event| {
        if let Event::ToolStart { id, .. } = event {
            seen.borrow_mut().push(format!("start:{id}"));
        }
    });
    let mut agent = idle_agent("snapshot-wait", Controls::default(), sink);
    std::fs::write(agent.ctx.cwd.join("a.txt"), "alpha").unwrap();
    let (tx, rx) = oneshot::channel();
    agent.set_snapshot_wait(rx);
    let sent = events.clone();
    let send = async move {
        sent.borrow_mut().push("sent".to_string());
        let _ = tx.send(());
    };
    let calls = vec![(
        "a".to_string(),
        ToolCall {
            id: "a".into(),
            name: "read".into(),
            arguments: r#"{"path":"a.txt"}"#.into(),
        },
    )];
    let (done, _) = tokio::join!(agent.execute_calls(calls, false), send);
    assert!(!done.unwrap());
    assert_eq!(*events.borrow(), ["sent", "start:a"]);
}

#[tokio::test(flavor = "current_thread")]
async fn configured_hooks_keep_read_calls_serial() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    let sink: Sink = Rc::new(move |event| {
        seen.borrow_mut().push(match event {
            Event::ToolStart { id, .. } => format!("start:{id}"),
            Event::ToolEnd { id, .. } => format!("end:{id}"),
            _ => return,
        });
    });
    let mut agent = idle_agent("serial-hooks", Controls::default(), sink);
    let hooks = toml::from_str("[[PreToolUse]]\ncommand = ':'\n").unwrap();
    agent.settings = Rc::new(Settings {
        hooks: Rc::new(hooks),
        ..Settings::default()
    });
    std::fs::write(agent.ctx.cwd.join("a.txt"), "alpha").unwrap();
    let calls: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|id| {
            (
                id.to_string(),
                ToolCall {
                    id: id.into(),
                    name: "read".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            )
        })
        .collect();
    agent.execute_calls(calls, false).await.unwrap();
    assert_eq!(*events.borrow(), ["start:a", "end:a", "start:b", "end:b"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a_message_typed_during_a_call_goes_ahead_of_the_calls_after_it() {
    let controls = Controls::default();
    let steer = controls.steer.clone();
    let sink: Sink = Rc::new(move |event| {
        if let Event::ToolStart { id, .. } = event
            && id == "a"
        {
            steer.borrow_mut().push_back(("stop".into(), Vec::new()));
        }
    });
    let mut agent = idle_agent("steer", controls, sink);
    let calls: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|id| {
            (
                id.to_string(),
                ToolCall {
                    id: id.to_string(),
                    name: "todo_write".into(),
                    arguments: r#"{"todos":[]}"#.into(),
                },
            )
        })
        .collect();
    let ids: Vec<_> = calls.iter().map(|(id, _)| json!({"id": id})).collect();
    agent
        .push(json!({"role": "assistant", "tool_calls": ids}))
        .unwrap();
    agent.execute_calls(calls, false).await.unwrap();
    let results: Vec<_> = agent.messages[1..]
        .iter()
        .map(|m| {
            (
                m["tool_call_id"].as_str().unwrap(),
                m["content"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(results[0].1.starts_with("Task list updated"), "{results:?}");
    assert_eq!(
        results[1..],
        [("b", SKIPPED_FOR_MESSAGE), ("c", SKIPPED_FOR_MESSAGE)]
    );
    assert!(unanswered(&agent.messages).is_empty());
    assert_eq!(
        agent.controls.steer.borrow().len(),
        1,
        "the message waits for the next request"
    );
    agent.before_request().await.unwrap();
    assert_eq!(
        agent.messages.last().unwrap(),
        &json!({"role": "user", "content": "stop"})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_task_list_unchanged_for_many_calls_is_pointed_out_once() {
    let mut agent = idle_agent("stale-tasks", Controls::default(), Rc::new(|_| {}));
    std::fs::write(agent.ctx.cwd.join("a.txt"), "alpha").unwrap();
    let read = |id: &str| {
        (
            id.to_string(),
            ToolCall {
                id: id.into(),
                name: "read".into(),
                arguments: r#"{"path":"a.txt"}"#.into(),
            },
        )
    };
    let list = |id: &str, todos: &str| {
        (
            id.to_string(),
            ToolCall {
                id: id.into(),
                name: "todo_write".into(),
                arguments: format!(r#"{{"todos":{todos}}}"#),
            },
        )
    };
    let reads = |n: usize| (0..n).map(|i| read(&format!("r{i}"))).collect::<Vec<_>>();
    let stale = |agent: &Agent| {
        agent
            .messages
            .iter()
            .filter(|m| {
                m["content"]
                    .as_str()
                    .is_some_and(|c| c.contains("hasn't changed in"))
            })
            .count()
    };
    // Without a list, no number of calls says anything.
    agent
        .execute_calls(reads(STALE_TASKS_AFTER), false)
        .await
        .unwrap();
    agent.before_request().await.unwrap();
    assert_eq!(stale(&agent), 0);
    // Setting the list starts the count over; one call short of the limit stays quiet.
    agent.execute_calls(vec![list("t", r#"[{"content":"first","status":"in_progress"},{"content":"second","status":"pending"}]"#)], false).await.unwrap();
    agent
        .execute_calls(reads(STALE_TASKS_AFTER - 1), false)
        .await
        .unwrap();
    agent.before_request().await.unwrap();
    assert_eq!(stale(&agent), 0);
    agent.execute_calls(reads(1), false).await.unwrap();
    agent.before_request().await.unwrap();
    assert_eq!(stale(&agent), 1);
    assert!(
        agent.messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("`first` still shows")
    );
    // Said once, then not again until as many calls pass; a finished list ends it.
    agent.before_request().await.unwrap();
    assert_eq!(stale(&agent), 1);
    agent
        .execute_calls(reads(STALE_TASKS_AFTER), false)
        .await
        .unwrap();
    agent.before_request().await.unwrap();
    assert_eq!(stale(&agent), 2);
    agent.execute_calls(vec![list("u", r#"[{"content":"first","status":"completed"},{"content":"second","status":"completed"}]"#)], false).await.unwrap();
    agent
        .execute_calls(reads(STALE_TASKS_AFTER), false)
        .await
        .unwrap();
    agent.before_request().await.unwrap();
    assert_eq!(stale(&agent), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn the_model_hears_the_session_goal_until_it_ends() {
    let mut agent = idle_agent("goal", Controls::default(), Rc::new(|_| {}));
    agent.set_goal("ship v2").unwrap();
    assert_eq!(agent.goal().as_deref(), Some("ship v2"));
    agent.before_request().await.unwrap();
    let said = agent.messages.last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        said.contains("Session goal: ship v2") && said.contains("/goal done"),
        "{said}"
    );
    agent.before_request().await.unwrap();
    assert_eq!(
        agent.messages.last().unwrap()["content"].as_str().unwrap(),
        said,
        "announced once"
    );
    agent.clear_goal().unwrap();
    assert_eq!(agent.goal(), None);
    agent.before_request().await.unwrap();
    let over = agent.messages.last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        over.contains("no longer stands") && over.contains("ship v2"),
        "{over}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_subagent_leaves_finished_tasks_for_its_parent_to_report() {
    let mut agent = idle_agent("child-tasks", Controls::default(), Rc::new(|_| {}));
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    let started = dispatch_tool(
        &agent.ctx,
        "bash",
        json!({"command": "exit 0", "run_in_background": true}),
        None,
    )
    .await;
    assert!(
        started.text.starts_with("Started background task 1"),
        "{}",
        started.text
    );
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let stopped = dispatch_tool(&agent.ctx, "task_stop", json!({"id": 1}), None).await;
        if stopped.text.contains("had already finished") {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut child = agent.subagent("child", None, false).unwrap();
    child.before_request().await.unwrap();
    assert_eq!(
        child.messages.len(),
        1,
        "a subagent hears nothing about background tasks"
    );
    agent.before_request().await.unwrap();
    let reported = agent.messages.last().unwrap()["content"].as_str().unwrap();
    assert!(
        reported.contains("Background task 1") && reported.contains("task_output"),
        "{reported}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_model_hears_once_when_the_sessions_in_its_tree_change_and_gets_their_messages() {
    let dir = crate::test_temp::TempDir::new("killi-peers");
    let entry = |pid, tree: &str| live::Entry {
        pid,
        tree: tree.into(),
        cwd: tree.into(),
        ..live::Entry::default()
    };
    let me = live::Registration::open_in(&dir, entry(1, "/w")).unwrap();
    let other = live::Registration::open_in(&dir, entry(2, "/w")).unwrap();
    let _elsewhere = live::Registration::open_in(&dir, entry(3, "/x")).unwrap();
    let notices = Rc::new(RefCell::new(Vec::new()));
    let seen = notices.clone();
    let sink: Sink = Rc::new(move |event| {
        if let Event::Notice(text) = event {
            seen.borrow_mut().push(text);
        }
    });
    let controls = Controls {
        here: Some(Rc::new(me.here())),
        ..Controls::default()
    };
    let mut agent = idle_agent("peers", controls.clone(), sink);
    let last = |agent: &Agent| {
        agent
            .messages
            .last()
            .and_then(|m| m["content"].as_str())
            .unwrap_or_default()
            .to_string()
    };

    agent.before_request().await.unwrap();
    assert!(
        last(&agent).starts_with(
            "<system-reminder>Another killi session works in this repository too: 2 · idle"
        ),
        "{}",
        last(&agent)
    );
    assert_eq!(notices.borrow().len(), 1);
    agent.before_request().await.unwrap();
    assert_eq!(agent.messages.len(), 1, "nothing new while nothing changes");

    controls.peer_notes.borrow_mut().push_back(live::tell_note(
        "killi session 2",
        Some(2),
        "pushed",
    ));
    drop(other);
    agent.before_request().await.unwrap();
    assert!(
        agent.messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("<message>\npushed\n</message>")
    );
    assert!(last(&agent).contains("No other killi session works in this repository now"));
    assert!(controls.peer_notes.borrow().is_empty());

    let mut child = agent.subagent("child", None, false).unwrap();
    let _again = live::Registration::open_in(&dir, entry(4, "/w")).unwrap();
    child.before_request().await.unwrap();
    assert_eq!(
        child.messages.len(),
        1,
        "a subagent is never told: only its system prompt"
    );
}

/// Runs one `bash` call while a message from the user is typed, for an agent at `depth`.
async fn bash_with_a_message(command: &str, depth: u8) -> (String, Duration) {
    let controls = Controls::default();
    let steer = controls.steer.clone();
    let sink: Sink = Rc::new(move |event| {
        if let Event::ToolStart { .. } = event {
            steer
                .borrow_mut()
                .push_back(("are you stuck?".into(), Vec::new()));
        }
    });
    let mut agent = idle_agent(&format!("wake-{depth}"), controls, sink);
    agent.depth = depth;
    let call = ToolCall {
        id: "a".into(),
        name: "bash".into(),
        arguments: json!({"command": command, "timeout": 60}).to_string(),
    };
    let started = Instant::now();
    let output = agent.execute("a", &call, false).await;
    (output.text, started.elapsed())
}

#[tokio::test(flavor = "current_thread")]
async fn a_message_typed_during_a_long_command_does_not_wait_for_it() {
    let (text, took) = bash_with_a_message("sleep 20", 0).await;
    assert!(took < Duration::from_secs(5), "waited {took:?}");
    assert!(
        text.contains("A message from the user is waiting, so it moved to background task 1"),
        "{text}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_subagent_waits_out_its_command_whatever_the_user_types() {
    let (text, _) = bash_with_a_message("sleep 1; echo done", 1).await;
    assert_eq!(text, "done");
}

#[tokio::test(flavor = "current_thread")]
async fn plan_mode_blocks_mcp_tool_calls_but_lists_tools() {
    let mut agent = idle_agent("plan-mcp", Controls::default(), Rc::new(|_| {}));
    agent.controls.plan.set(true);
    let tool = ToolCall {
        id: "a".into(),
        name: "mcp".into(),
        arguments: json!({"server": "s", "tool": "t"}).to_string(),
    };
    let blocked = agent
        .dispatch(&tool, &json!({"server": "s", "tool": "t"}))
        .await;
    assert!(
        blocked.is_error && blocked.text.contains("Plan mode"),
        "{}",
        blocked.text
    );
    let list = ToolCall {
        id: "b".into(),
        name: "mcp".into(),
        arguments: json!({"server": "s"}).to_string(),
    };
    let listed = agent.dispatch(&list, &json!({"server": "s"})).await;
    assert!(!listed.text.contains("Plan mode"), "{}", listed.text);
}

#[test]
fn a_subagent_in_plan_mode_hears_it_first() {
    let mut agent = idle_agent("plan-child", Controls::default(), Rc::new(|_| {}));
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    let calm = agent.subagent("kid", None, false).unwrap();
    assert_eq!(calm.messages.len(), 1);
    agent.controls.plan.set(true);
    let child = agent.subagent("kid", None, false).unwrap();
    assert_eq!(child.messages.len(), 2);
    assert_eq!(child.messages[1]["content"].as_str(), Some(plan::ON));
    assert!(child.plan_announced);
}

#[tokio::test(flavor = "current_thread")]
async fn a_blocking_hook_stops_an_agent_call() {
    let mut agent = idle_agent("hook-agent", Controls::default(), Rc::new(|_| {}));
    let hooks: HooksConfig = toml::from_str(
        "[[PreToolUse]]\nmatcher = \"agent\"\ncommand = \"echo no subagents >&2; exit 2\"\n",
    )
    .unwrap();
    agent.settings = Rc::new(Settings {
        hooks: Rc::new(hooks),
        ..Settings::default()
    });
    let calls = vec![(
        "a".to_string(),
        ToolCall {
            id: "a".into(),
            name: "agent".into(),
            arguments: json!({"prompt": "do it", "description": "kid"}).to_string(),
        },
    )];
    assert!(!agent.execute_calls(calls, false).await.unwrap());
    let result = agent.messages.last().unwrap()["content"].as_str().unwrap();
    assert!(
        result.contains("A hook blocked this call: no subagents"),
        "{result}"
    );
}

#[test]
fn later_takes_a_number_or_a_string_and_shows_numbered_items() {
    let tmp = crate::test_temp::TempDir::new("killi-later-home");
    let previous = std::env::var_os("HOME");
    // SAFETY: the override lasts only for the file calls below, then HOME is restored.
    unsafe { std::env::set_var("HOME", tmp.path()) };
    let mut agent = idle_agent("later-tool", Controls::default(), Rc::new(|_| {}));
    let first = agent.later(&json!({"note": "fix the flaky test"}));
    let second = agent.later(&json!({"note": "cache the glob"}));
    let done = agent.later(&json!({"done": "1"}));
    let missing = agent.later(&json!({"done": 5}));
    match previous {
        Some(home) => unsafe { std::env::set_var("HOME", home) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    assert!(!first.is_error, "{}", first.text);
    assert!(
        first.text.contains("1. fix the flaky test"),
        "{}",
        first.text
    );
    assert!(!second.is_error, "{}", second.text);
    assert!(
        second
            .text
            .contains("1. fix the flaky test\n2. cache the glob"),
        "{}",
        second.text
    );
    assert!(!done.is_error, "{}", done.text);
    assert!(
        done.text.contains('1') && done.text.contains("fix the flaky test"),
        "{}",
        done.text
    );
    assert!(
        missing.is_error && missing.text.contains("no item 5"),
        "{}",
        missing.text
    );
}

#[tokio::test(flavor = "current_thread")]
async fn nested_agents_files_cover_bash_cwd_and_directories() {
    let mut agent = idle_agent("nested-agents", Controls::default(), Rc::new(|_| {}));
    let sub = agent.ctx.cwd.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("AGENTS.md"), "be careful").unwrap();
    let read = ToolCall {
        id: "r".into(),
        name: "read".into(),
        arguments: json!({"path": "sub"}).to_string(),
    };
    agent.execute("r", &read, false).await;
    let bash = ToolCall {
        id: "b".into(),
        name: "bash".into(),
        arguments: json!({"command": "pwd", "cwd": "sub"}).to_string(),
    };
    agent.execute("b", &bash, false).await;
    agent.before_request().await.unwrap();
    let note = agent.messages.last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(note.contains("AGENTS.md") && note.contains("sub"), "{note}");
    std::fs::remove_dir_all(&sub).unwrap();
}

#[test]
fn queued_messages_can_be_taken_back_for_the_prompt() {
    let controls = Controls::default();
    controls
        .steer
        .borrow_mut()
        .push_back(("first".into(), Vec::new()));
    controls
        .steer
        .borrow_mut()
        .push_back(("second".into(), Vec::new()));
    let mut agent = idle_agent("take-steered", controls, Rc::new(|_| {}));
    assert!(agent.pending_steer());
    let taken = agent.take_steered();
    assert_eq!(taken.len(), 2);
    assert!(!agent.pending_steer());
}

/// An agent working in a git tree with a peer registered there, for the peer guard.
/// The peer's registration is returned so it stays listed until the test ends.
fn peered_agent(name: &str) -> (Agent, live::Registration, crate::test_temp::TempDir) {
    let live_dir = crate::test_temp::TempDir::new(&format!("killi-{name}-live"));
    let mut agent = idle_agent(name, Controls::default(), Rc::new(|_| {}));
    std::fs::create_dir_all(agent.ctx.cwd.join(".git")).unwrap();
    let tree = agent.ctx.cwd.clone();
    let me = live::Registration::open_in(
        &live_dir,
        live::Entry {
            pid: 1,
            tree: tree.clone(),
            cwd: tree.clone(),
            ..live::Entry::default()
        },
    )
    .unwrap();
    agent.controls.here = Some(Rc::new(me.here()));
    let peer = live::Registration::open_in(
        &live_dir,
        live::Entry {
            pid: 2,
            tree: tree.clone(),
            cwd: tree.clone(),
            ..live::Entry::default()
        },
    )
    .unwrap();
    (agent, peer, live_dir)
}

fn bash_call(command: &str) -> ToolCall {
    ToolCall {
        id: "a".into(),
        name: "bash".into(),
        arguments: json!({"command": command}).to_string(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn tree_moving_git_is_refused_while_a_peer_works_here() {
    let (mut agent, _peer, _live) = peered_agent("peer-guard");
    for command in [
        "git restore --staged --worktree -- f",
        "git checkout -- .",
        "git switch main",
        "git stash",
        "git reset --hard",
        "git clean -fd",
        "true; git stash",
    ] {
        let call = bash_call(command);
        let output = agent.dispatch(&call, &json!({"command": command})).await;
        assert!(
            output.is_error
                && output
                    .text
                    .starts_with("Not run: another killi session (pid 2)"),
            "{command}: {}",
            output.text
        );
        assert!(
            output.text.contains("`git worktree add"),
            "{command}: {}",
            output.text
        );
    }
    // Read-only git and lookalikes run as usual: nothing is refused.
    for command in [
        "git stash list",
        "git stash show -p",
        "git status",
        "echo \"git restore\"",
    ] {
        let call = bash_call(command);
        let output = agent.dispatch(&call, &json!({"command": command})).await;
        assert!(
            !output.text.starts_with("Not run:"),
            "{command}: {}",
            output.text
        );
    }
    // A subagent moves the same tree, so it is refused too.
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    let mut child = agent.subagent("kid", None, false).unwrap();
    assert!(child.controls.here.is_some());
    let call = bash_call("git restore f");
    let output = child
        .dispatch(&call, &json!({"command": "git restore f"}))
        .await;
    assert!(
        output.is_error && output.text.starts_with("Not run: another killi session"),
        "{}",
        output.text
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tree_moving_git_runs_without_a_peer_or_in_another_tree() {
    let (mut agent, peer, live) = peered_agent("peer-guard-free");
    // No peer registered: runs (and fails in the scratch repo, like any git call there).
    drop(peer);
    let call = bash_call("git restore f");
    let output = agent
        .dispatch(&call, &json!({"command": "git restore f"}))
        .await;
    assert!(!output.text.starts_with("Not run:"), "{}", output.text);
    // A peer in a linked worktree is a tree of its own: not blocked.
    let wt = crate::test_temp::TempDir::new("killi-peer-guard-wt");
    std::fs::write(wt.path().join(".git"), "gitdir: elsewhere").unwrap();
    let _wt_peer = live::Registration::open_in(
        &live,
        live::Entry {
            pid: 3,
            tree: wt.path().to_path_buf(),
            cwd: wt.path().to_path_buf(),
            ..live::Entry::default()
        },
    )
    .unwrap();
    let call = bash_call("git stash");
    let output = agent
        .dispatch(&call, &json!({"command": "git stash"}))
        .await;
    assert!(!output.text.starts_with("Not run:"), "{}", output.text);
    // A `cwd` in another repo is not this tree either.
    let other = crate::test_temp::TempDir::new("killi-peer-guard-other");
    std::fs::create_dir_all(other.path().join(".git")).unwrap();
    let elsewhere = other.path().to_string_lossy().to_string();
    let call = bash_call("git restore f");
    let output = agent
        .dispatch(
            &call,
            &json!({"command": "git restore f", "cwd": elsewhere}),
        )
        .await;
    assert!(!output.text.starts_with("Not run:"), "{}", output.text);
}

#[tokio::test(flavor = "current_thread")]
async fn a_reply_ending_on_a_colon_is_asked_to_go_on_once() {
    use crate::agent::turn::UNFINISHED;
    let notices = Rc::new(RefCell::new(Vec::new()));
    let seen = notices.clone();
    let sink: Sink = Rc::new(move |event| {
        if let Event::Notice(text) = event {
            seen.borrow_mut().push(text);
        }
    });
    let url = stub_replies(vec![
        sse("Digging into why:", "stop"),
        sse("Found it.", "stop"),
    ])
    .await;
    let mut agent = idle_agent("unfinished-reply", Controls::default(), sink);
    Rc::get_mut(&mut agent.client).unwrap().model.base_url = url;
    let (text, stop) = agent.run("why?", &[]).await.unwrap();
    assert_eq!((text.as_str(), stop), ("Found it.", Stop::Completed));
    let roles: Vec<_> = agent
        .messages
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["user", "assistant", "user", "assistant"]);
    assert_eq!(agent.messages[2]["content"], UNFINISHED);
    assert_eq!(notices.borrow().len(), 1);

    // A second colon ends the turn: it is asked once, not until it gives in.
    let url = stub_replies(vec![sse("Digging:", "stop"), sse("Still digging:", "stop")]).await;
    let mut agent = idle_agent("unfinished-twice", Controls::default(), Rc::new(|_| {}));
    Rc::get_mut(&mut agent.client).unwrap().model.base_url = url;
    let (text, _) = agent.run("why?", &[]).await.unwrap();
    assert_eq!(text, "Still digging:");
    assert_eq!(
        agent
            .messages
            .iter()
            .filter(|m| m["content"] == UNFINISHED)
            .count(),
        1
    );
}
