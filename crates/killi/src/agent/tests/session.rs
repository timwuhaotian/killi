use super::*;

#[test]
fn a_new_agent_starts_without_a_backup() {
    let agent = idle_agent("backup-none", Controls::default(), Rc::new(|_| {}));
    assert_eq!(agent.backup_alias(), None, "a backup is never inherited by a new session");
}

#[tokio::test(flavor = "current_thread")]
async fn a_rate_limited_primary_switches_to_the_backup() {
    let primary = rate_limited_stub(10).await;
    let backup = answering_stub("from backup").await;
    let mut agent = idle_agent("backup-switch", Controls::default(), Rc::new(|_| {}));
    agent.push(json!({"role": "system", "content": "sys"})).unwrap();
    agent.push(json!({"role": "user", "content": "hi"})).unwrap();
    agent.client = Rc::new(test_client_at("primary", &primary));
    agent.set_backup(test_client_at("backup", &backup)).unwrap();

    let reply = agent.request().await.unwrap();
    assert_eq!(reply.text, "from backup");
    assert_eq!(agent.model_alias(), "backup", "the backup became the running model");
    assert_eq!(agent.backup_alias(), None, "the backup was consumed by the switch");
}

#[tokio::test(flavor = "current_thread")]
async fn a_non_rate_limit_error_keeps_the_backup() {
    let auth = "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"error\":{\"message\":\"bad key\"}}".to_string();
    let primary = serve_raw(vec![auth]).await;
    let backup = answering_stub("from backup").await;
    let mut agent = idle_agent("backup-keep", Controls::default(), Rc::new(|_| {}));
    agent.push(json!({"role": "system", "content": "sys"})).unwrap();
    agent.push(json!({"role": "user", "content": "hi"})).unwrap();
    agent.client = Rc::new(test_client_at("primary", &primary));
    agent.set_backup(test_client_at("backup", &backup)).unwrap();

    let error = agent.request().await.unwrap_err();
    assert_eq!(error.downcast_ref::<ProviderError>().map(|p| p.kind), Some(ErrorKind::Auth));
    assert_eq!(agent.model_alias(), "primary", "an auth error leaves the running model alone");
    assert_eq!(agent.backup_alias(), Some("backup"), "the backup is kept for a later rate limit");
}

#[tokio::test(flavor = "current_thread")]
async fn a_rate_limited_primary_without_a_backup_fails_as_before() {
    let primary = rate_limited_stub(10).await;
    let mut agent = idle_agent("backup-absent", Controls::default(), Rc::new(|_| {}));
    agent.push(json!({"role": "system", "content": "sys"})).unwrap();
    agent.push(json!({"role": "user", "content": "hi"})).unwrap();
    agent.client = Rc::new(test_client_at("primary", &primary));

    let error = agent.request().await.unwrap_err();
    assert_eq!(error.downcast_ref::<ProviderError>().map(|p| p.kind), Some(ErrorKind::RateLimited));
    assert_eq!(agent.model_alias(), "primary");
}

#[test]
fn compaction_estimates_only_the_appended_suffix_and_reserves_output() {
    let mut agent = idle_agent("compaction-estimate", Controls::default(), Rc::new(|_| {}));
    agent.settings = Rc::new(Settings { compact_at: Some(10_000), ..Settings::default() });
    Rc::get_mut(&mut agent.client).unwrap().model.max_output = Some(1_000);
    agent.stats.context = 9_000;
    assert!(!agent.compaction_due(), "the exact input-plus-output boundary is still allowed");
    agent.stats.context = 9_001;
    assert!(agent.compaction_due(), "the output reserve counts before the next request");

    agent.stats.context = 100;
    agent.push(json!({"role": "user", "content": "old ".repeat(20_000)})).unwrap();
    agent.accounted_messages = agent.messages.len();
    assert!(!agent.compaction_due(), "the fixed prefix is not estimated a second time");
    agent.push(json!({"role": "user", "content": "new ".repeat(5_000)})).unwrap();
    assert!(agent.compaction_due(), "the unbilled appended suffix counts conservatively");
}

#[test]
fn compaction_aims_at_sixty_percent_of_the_window_by_default() {
    let mut agent = idle_agent("compaction-window", Controls::default(), Rc::new(|_| {}));
    let client = Rc::get_mut(&mut agent.client).unwrap();
    client.model.context_window = Some(1_000_000);
    client.model.max_output = Some(128_000);
    agent.accounted_messages = agent.messages.len();
    // limit = 600k, reserve = min(128k, 150k): a quarter of the limit caps the reserve.
    agent.stats.context = 472_000;
    assert!(!agent.compaction_due(), "the boundary is still allowed");
    agent.stats.context = 472_001;
    assert!(agent.compaction_due(), "the capped reserve counts before the next request");
}

#[test]
fn an_explicit_compact_at_overrides_the_window_share_and_zero_disables() {
    let mut agent = idle_agent("compaction-explicit", Controls::default(), Rc::new(|_| {}));
    let client = Rc::get_mut(&mut agent.client).unwrap();
    client.model.context_window = Some(1_000_000);
    client.model.max_output = Some(128_000);
    agent.accounted_messages = agent.messages.len();
    agent.stats.context = 8_000;
    assert!(!agent.compaction_due(), "without a setting the window share applies");
    agent.settings = Rc::new(Settings { compact_at: Some(10_000), ..Settings::default() });
    // reserve = min(128k, 2.5k), so the smaller explicit limit still leaves reply room.
    agent.stats.context = 7_500;
    assert!(!agent.compaction_due());
    agent.stats.context = 7_501;
    assert!(agent.compaction_due(), "the explicit limit wins over the window share");
    agent.settings = Rc::new(Settings { compact_at: Some(0), ..Settings::default() });
    agent.stats.context = 900_000;
    assert!(!agent.compaction_due(), "zero keeps proactive compaction off");
}

#[test]
fn an_unknown_window_falls_back_to_a_fixed_limit() {
    let mut agent = idle_agent("compaction-unknown", Controls::default(), Rc::new(|_| {}));
    Rc::get_mut(&mut agent.client).unwrap().model.max_output = None;
    agent.accounted_messages = agent.messages.len();
    agent.stats.context = DEFAULT_COMPACT_AT - DEFAULT_OUTPUT_RESERVE;
    assert!(!agent.compaction_due());
    agent.stats.context += 1;
    assert!(agent.compaction_due(), "the default reserve applies without an output cap");
}

#[test]
fn after_a_compaction_proactive_compaction_waits_for_real_growth() {
    let notices = Rc::new(RefCell::new(Vec::new()));
    let seen = notices.clone();
    let sink: Sink = Rc::new(move |event| {
        if let Event::Notice(text) = event {
            seen.borrow_mut().push(text);
        }
    });
    let mut agent = idle_agent("compaction-growth", Controls::default(), sink.clone());
    agent.settings = Rc::new(Settings { compact_at: Some(10_000), ..Settings::default() });
    Rc::get_mut(&mut agent.client).unwrap().model.max_output = Some(1_000);
    agent.push(json!({"role": "system", "content": "sys"})).unwrap();
    agent.push(json!({"role": "user", "content": "s".repeat(16_000)})).unwrap();
    agent.after_compaction(10_000);
    assert!(notices.borrow().is_empty(), "the compacted history is under the limit");
    agent.stats.context = 9_400;
    assert!(!agent.compaction_due(), "grown by less than a fifth of the limit since the compaction");
    agent.stats.context = 10_600;
    assert!(agent.compaction_due(), "a fifth of the limit of growth compacts again");

    let mut agent = idle_agent("compaction-cant-shrink", Controls::default(), sink);
    agent.settings = Rc::new(Settings { compact_at: Some(10_000), ..Settings::default() });
    Rc::get_mut(&mut agent.client).unwrap().model.max_output = Some(1_000);
    agent.push(json!({"role": "system", "content": "sys"})).unwrap();
    agent.push(json!({"role": "user", "content": "s".repeat(24_000)})).unwrap();
    agent.after_compaction(10_000);
    assert_eq!(notices.borrow().len(), 1, "a summary over the limit says so once");
    assert!(notices.borrow()[0].contains("the system prompt and kept requests alone exceed compact_at"), "{}", notices.borrow()[0]);
    agent.stats.context = 11_000;
    assert!(!agent.compaction_due(), "a history the summary can't shrink doesn't compact before every step");
}

#[test]
fn a_fork_accounts_its_inherited_history_once() {
    let mut agent = idle_agent("fork-estimate", Controls::default(), Rc::new(|_| {}));
    agent.settings = Rc::new(Settings { compact_at: Some(10_000), ..Settings::default() });
    Rc::get_mut(&mut agent.client).unwrap().model.max_output = Some(1_000);
    agent.push(json!({"role": "system", "content": "sys"})).unwrap();
    agent.push(json!({"role": "user", "content": "x".repeat(20_000)})).unwrap();
    agent.stats.context = 5_000;
    agent.current_reply = agent.messages.len();
    let child = agent.subagent("fork", None, true).unwrap();
    assert_eq!(child.stats.context, 5_000);
    assert_eq!(child.accounted_messages, child.messages.len());
    assert!(!child.compaction_due(), "the inherited history isn't estimated a second time on top of its billing");
}

#[test]
fn switching_model_returns_the_whole_conversation_to_the_estimate() {
    let mut agent = idle_agent("switch-estimate", Controls::default(), Rc::new(|_| {}));
    agent.settings = Rc::new(Settings { compact_at: Some(10_000), ..Settings::default() });
    Rc::get_mut(&mut agent.client).unwrap().model.max_output = Some(1_000);
    agent.push(json!({"role": "system", "content": "sys"})).unwrap();
    agent.push(json!({"role": "user", "content": "x".repeat(40_000)})).unwrap();
    agent.stats.context = 9_000;
    agent.accounted_messages = agent.messages.len();
    assert!(!agent.compaction_due(), "under the old model's billing it fits");
    agent.switch_model(agent.client()).unwrap();
    assert_eq!((agent.stats.context, agent.accounted_messages), (0, 0));
    assert!(agent.compaction_due(), "the same conversation is estimated whole again");
}

#[tokio::test(flavor = "current_thread")]
async fn switching_model_updates_image_capability() {
    let mut agent = idle_agent("switch-images", Controls::default(), Rc::new(|_| {}));
    agent.ctx.images.set(true);
    std::fs::write(agent.ctx.cwd.join("pic.png"), [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x01, 0x02]).unwrap();
    let args = json!({"path": "pic.png"});
    let before = dispatch_tool(&agent.ctx, "read", args.clone(), None).await;
    assert!(!before.is_error && before.image.is_some(), "{}", before.text);
    let mut client = agent.client();
    client.model.images = false;
    agent.switch_model(client).unwrap();
    assert!(!agent.ctx.images.get());
    let after = dispatch_tool(&agent.ctx, "read", args, None).await;
    assert!(after.is_error, "{}", after.text);
    assert!(after.text.contains("can't see images"), "{}", after.text);
    assert!(after.image.is_none());
    std::fs::remove_file(agent.ctx.cwd.join("pic.png")).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn a_rejected_image_is_stripped_once_and_the_turn_goes_on() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let replies = [
            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"error\":{\"message\":\"the image is too large\"}}",
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\ndata: [DONE]\n\n",
        ];
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buffer.len() >= end + 4 + length {
                        break;
                    }
                }
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                }
            }
            let _ = socket.write_all(reply.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    let notices = Rc::new(RefCell::new(Vec::new()));
    let seen = notices.clone();
    let sink: Sink = Rc::new(move |event| {
        if let Event::Notice(text) = event {
            seen.borrow_mut().push(text);
        }
    });
    let mut agent = idle_agent("media-recovery", Controls::default(), sink);
    let client = Rc::get_mut(&mut agent.client).unwrap();
    client.model.base_url = url;
    client.model.images = true;
    agent.ctx.images.set(true);
    agent.push(json!({"role": "system", "content": "sys"})).unwrap();
    let images = vec![Image { label: "[Image #1]".into(), url: "data:image/png;base64,AAAA".into() }];
    let (text, _) = agent.run("look", &images).await.unwrap();
    assert_eq!(text, "done");
    let flat = agent.messages.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    assert!(!flat.contains("image_url"), "{flat}");
    assert!(flat.contains(IMAGE_OMITTED), "{flat}");
    assert!(notices.borrow().iter().any(|notice| notice.contains("placeholder")), "{:?}", notices.borrow());
    let log = std::fs::read_to_string(&agent.session.log_path).unwrap();
    assert!(log.contains("\"media_rejected\""), "{log}");
    let loaded = crate::session::load(&agent.session.log_path).unwrap();
    assert!(loaded.messages.iter().map(Value::to_string).collect::<Vec<_>>().join("\n").contains(IMAGE_OMITTED));
}

#[test]
fn a_resumed_session_keeps_the_plan_mode_it_ended_in_and_drops_the_queue() {
    let dir = crate::test_temp::TempDir::new("killi-resume-plan");
    let log = dir.join("session.jsonl");
    let lines = [
        json!({"type": "message", "message": {"role": "system", "content": "sys"}}).to_string(),
        json!({"type": "message", "message": {"role": "user", "content": plan::ON}}).to_string(),
        json!({"type": "message", "message": {"role": "user", "content": "keep planning"}}).to_string(),
    ];
    std::fs::write(&log, lines.join("\n") + "\n").unwrap();
    let controls = Controls::default();
    controls.steer.borrow_mut().push_back(("queued".into(), Vec::new()));
    let load = |log: &Path| crate::session::load_reported(log).unwrap();
    let agent = Agent::resume_reported(test_client(), None, &dir, &log, load(&log), Vec::new(), Settings::default(), controls.clone(), Rc::new(|_| {}))
        .unwrap();
    assert!(agent.plan_announced, "the log ended in plan mode");
    assert!(controls.steer.borrow().is_empty(), "resume drops another session's queued messages");
    drop(agent);
    let off = [
        json!({"type": "message", "message": {"role": "system", "content": "sys"}}).to_string(),
        json!({"type": "message", "message": {"role": "user", "content": plan::OFF}}).to_string(),
    ];
    std::fs::write(&log, off.join("\n") + "\n").unwrap();
    let agent = Agent::resume_reported(test_client(), None, &dir, &log, load(&log), Vec::new(), Settings::default(), Controls::default(), Rc::new(|_| {}))
        .unwrap();
    assert!(!agent.plan_announced, "the log ended out of plan mode");
}

#[tokio::test(flavor = "current_thread")]
async fn length_continuations_keep_every_part() {
    let url = stub_replies(vec![sse("part1", "length"), sse("part2", "stop")]).await;
    let mut agent = idle_agent("length-text", Controls::default(), Rc::new(|_| {}));
    Rc::get_mut(&mut agent.client).unwrap().model.base_url = url;
    let (text, stop) = agent.run("go", &[]).await.unwrap();
    assert_eq!((text.as_str(), stop), ("part1part2", Stop::Completed));
}

#[test]
fn recover_keeps_finished_swarm_reports() {
    let mut agent = idle_agent("recover-swarm", Controls::default(), Rc::new(|_| {}));
    agent.push(json!({"role": "assistant", "tool_calls": [{"id": "s1"}]})).unwrap();
    agent.partial_swarm = Some(PartialSwarm {
        id: "s1".into(),
        items: vec!["a".into(), "b".into()],
        results: vec![Some(("item zero report".into(), false)), None],
    });
    agent.recover().unwrap();
    assert!(agent.interrupted);
    assert!(agent.partial_swarm.is_none());
    let results: Vec<&Value> = agent.messages.iter().filter(|m| m["role"] == "tool" && m["tool_call_id"] == "s1").collect();
    assert_eq!(results.len(), 1);
    let content = results[0]["content"].as_str().unwrap();
    assert!(content.contains("item zero report"), "{content}");
    assert!(content.contains("<subagent item=\"a\" outcome=\"completed\">"), "{content}");
    assert!(content.contains("<subagent item=\"b\" outcome=\"failed\">"), "{content}");
    assert!(content.contains("Interrupted by the user before this subagent finished, so it has no result."), "{content}");
    assert!(content.contains("Run the failed items again"), "{content}");
    assert_ne!(content, "Interrupted by the user before this finished.");
}

#[test]
fn recover_marks_a_tool_result_as_interrupted() {
    let mut agent = idle_agent("recover-tool", Controls::default(), Rc::new(|_| {}));
    agent.recover().unwrap();
    assert!(!agent.interrupted);
    agent.push(json!({"role": "assistant", "tool_calls": [{"id": "a"}]})).unwrap();
    agent.push(json!({"role": "tool", "tool_call_id": "a", "content": "done"})).unwrap();
    agent.recover().unwrap();
    assert!(agent.interrupted);
}

#[test]
fn resume_reports_skipped_lines_and_keeps_the_dangling_call_repair() {
    let dir = crate::test_temp::TempDir::new("killi-resume-repair");
    let log = dir.join("1-1.jsonl");
    let call = |id: &str| json!({"id": id, "type": "function", "function": {"name": "read", "arguments": "{}"}});
    let lines = [
        json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
        json!({"type": "message", "message": {"role": "user", "content": "do it"}}),
        json!({"type": "message", "message": {"role": "assistant", "content": "on it", "tool_calls": [call("c1")]}}),
        json!({"type": "message", "message": {"role": "user", "content": "keep going"}}),
        json!({"type": "message", "message": {"role": "assistant", "content": "later", "tool_calls": [call("c2")]}}),
    ];
    let mut text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        text.push_str("{\"type\":\"messa");
        std::fs::write(&log, text).unwrap();
        let notices = Rc::new(RefCell::new(Vec::new()));
        let seen = notices.clone();
        let sink: Sink =
            Rc::new(move |event| {
                if let Event::Notice(notice) = event {
                    seen.borrow_mut().push(notice);
                }
            });
        let report = crate::session::load_reported(&log).unwrap();
        let agent = Agent::resume_reported(test_client(), None, &dir, &log, report, Vec::new(), Settings::default(), Controls::default(), sink).unwrap();
        let placeholder = agent
            .messages
            .iter()
            .find(|m| m["role"] == "tool" && m["tool_call_id"] == "c1")
            .map(|m| m["content"].as_str().unwrap_or_default().to_string());
        assert!(placeholder.as_deref().is_some_and(|c| !c.is_empty()), "{placeholder:?}");
        assert!(notices.borrow().iter().any(|n| n.contains("Skipped 1 unreadable")), "{:?}", notices.borrow());
        drop(agent);
        let replayed = crate::session::load(&log).unwrap();
        assert!(
            replayed.messages.iter().any(|m| m["tool_call_id"] == "c1"),
            "the repair is kept as a compaction-kind entry, so a later load replays it"
        );
}

#[test]
fn resume_from_another_folder_warns_with_the_way_back() {
    let dir = crate::test_temp::TempDir::new("killi-resume-cwd");
    let log = dir.join("1-1.jsonl");
    let lines = [
        json!({"type": "session", "cwd": "/definitely/not/here", "model": "m"}),
        json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
        json!({"type": "message", "message": {"role": "user", "content": "hi"}}),
    ];
    std::fs::write(&log, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    let notices = Rc::new(RefCell::new(Vec::new()));
    let seen = notices.clone();
    let sink: Sink = Rc::new(move |event| {
        if let Event::Notice(notice) = event {
            seen.borrow_mut().push(notice);
        }
    });
    let report = crate::session::load_reported(&log).unwrap();
    let agent = Agent::resume_reported(test_client(), None, &dir, &log, report, Vec::new(), Settings::default(), Controls::default(), sink)
        .unwrap();
    assert!(
        notices.borrow().iter().any(|n| n.contains("/definitely/not/here") && n.contains("cd /definitely/not/here")),
        "the resume names the folder it left: {:?}",
        notices.borrow()
    );
    drop(agent);

    let home = [
        json!({"type": "session", "cwd": dir.as_os_str().to_str().unwrap(), "model": "m"}),
        json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
    ];
    std::fs::write(&log, home.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    let quiet = Rc::new(RefCell::new(Vec::new()));
    let seen = quiet.clone();
    let sink: Sink = Rc::new(move |event| {
        if let Event::Notice(notice) = event {
            seen.borrow_mut().push(notice);
        }
    });
    let report = crate::session::load_reported(&log).unwrap();
    let agent = Agent::resume_reported(test_client(), None, &dir, &log, report, Vec::new(), Settings::default(), Controls::default(), sink)
        .unwrap();
    assert!(quiet.borrow().is_empty(), "resuming where the session ran stays silent: {:?}", quiet.borrow());
    drop(agent);
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_runs_before_steered_messages_are_pushed() {
    let url = stub_replies(vec![sse("kept summary", "stop")]).await;
    let mut agent = idle_agent("compact-first", Controls::default(), Rc::new(|_| {}));
    Rc::get_mut(&mut agent.client).unwrap().model.base_url = url;
    agent.settings = Rc::new(Settings { compact_at: Some(100), ..Settings::default() });
    agent.stats.context = 1_000;
    agent.push(json!({"role": "system", "content": "sys"})).unwrap();
    agent.controls.steer.borrow_mut().push_back(("meanwhile this".into(), Vec::new()));
    agent.before_request().await.unwrap();
    assert_eq!(agent.messages.len(), 3);
    assert!(agent.messages[1]["content"].as_str().unwrap().contains("kept summary"));
    assert_eq!(agent.messages[2], json!({"role": "user", "content": "meanwhile this"}));
}

#[tokio::test(flavor = "current_thread")]
async fn undo_note_reaches_the_next_prompt() {
    let url = stub_replies(vec![sse("done", "stop")]).await;
    let mut agent = idle_agent("undo-note", Controls::default(), Rc::new(|_| {}));
    Rc::get_mut(&mut agent.client).unwrap().model.base_url = url;
    agent.note_for_next_prompt(
        "The user ran /undo: files were restored to how they were before \"fix it\". Changed back: a.txt, b.txt. Read files again before relying on what you saw."
            .to_string(),
    );
    let (text, _) = agent.run("hi", &[]).await.unwrap();
    assert_eq!(text, "done");
    let sent = agent.messages[0]["content"].as_str().unwrap();
    assert!(sent.contains("Changed back: a.txt, b.txt") && sent.ends_with("hi"), "{sent}");
}

#[tokio::test(flavor = "current_thread")]
async fn session_end_runs_hooks_only_for_started_sessions() {
    let marker = std::env::temp_dir().join(format!("killi-session-end-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    #[derive(serde::Deserialize)]
    struct Wrapper {
        hooks: crate::hooks::HooksConfig,
    }
    let hooks = toml::from_str::<Wrapper>(&format!("[[hooks.SessionEnd]]\ncommand = \"touch {}\"\n", marker.display())).unwrap().hooks;
    let mut agent = idle_agent("session-end", Controls::default(), Rc::new(|_| {}));
    agent.settings = Rc::new(Settings { hooks: Rc::new(hooks), ..Settings::default() });
    agent.session_end("completed").await;
    assert!(!marker.exists(), "an unstarted session runs no SessionEnd hooks");
    agent.session_started = true;
    agent.session_end("completed").await;
    assert!(marker.exists(), "a started session runs its SessionEnd hooks");
    let _ = std::fs::remove_file(&marker);
}
