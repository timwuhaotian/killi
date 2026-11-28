//! What a compaction must carry or reset beyond the summary: the state killi holds for the
//! model, and the guards that assumed the old messages were still in view.

use super::*;

/// An agent with a system prompt and one request, talking to a stub that answers `times`
/// summary requests.
async fn compactable(name: &str, times: usize) -> Agent {
    let url = answering_stub_times("summary", times).await;
    let mut agent = idle_agent(name, Controls::default(), Rc::new(|_| {}));
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    agent.client = Rc::new(test_client_at("primary", &url));
    agent
        .push(json!({"role": "user", "content": "build it"}))
        .unwrap();
    agent
}

fn open_list() -> Value {
    json!({"todos": [
        {"content": "write the parser", "status": "completed"},
        {"content": "test it", "status": "in_progress"},
        {"content": "document it", "status": "pending"},
    ]})
}

#[tokio::test(flavor = "current_thread")]
async fn a_compaction_lets_an_identical_call_run_again() {
    let read = call(r#"{"path":"a"}"#);
    let mut agent = compactable("compact-repeats", 1).await;
    for id in ["c1", "c2"] {
        agent
            .finish_call(
                id,
                &read,
                tools::Output::ok("same result"),
                Duration::ZERO,
                None,
            )
            .unwrap();
    }
    assert!(agent.repeat_block(&read).is_some(), "refused before");
    agent.compact(None).await.unwrap();
    assert!(
        agent.repeat_block(&read).is_none(),
        "the results it would point at went with the old messages"
    );
    assert!(agent.near_repeats.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_compaction_restates_the_open_task_list() {
    let mut agent = compactable("compact-todos", 1).await;
    assert!(!agent.todo_write(&open_list()).is_error);
    agent.compact(None).await.unwrap();
    let note = agent.messages[2]["content"].as_str().unwrap();
    assert!(
        note.starts_with("<system-reminder>") && note.ends_with("</system-reminder>"),
        "{note}"
    );
    assert!(
        note.contains("2 open:\n1. [completed] write the parser\n2. [in_progress] test it\n3. [pending] document it\n"),
        "{note}"
    );
    assert_eq!(agent.messages.len(), 3, "{:?}", agent.messages);
}

#[tokio::test(flavor = "current_thread")]
async fn a_compaction_with_nothing_held_adds_no_note() {
    let mut agent = compactable("compact-nothing-held", 1).await;
    let done = json!({"todos": [{"content": "write the parser", "status": "completed"}]});
    assert!(!agent.todo_write(&done).is_error);
    agent.compact(None).await.unwrap();
    assert_eq!(agent.messages.len(), 2, "{:?}", agent.messages);
}

#[test]
fn the_task_note_keeps_each_item_on_one_line_and_cuts_a_long_one() {
    let todos = vec![
        Todo {
            content: format!("first\nline {}", "x".repeat(400)),
            status: "in_progress".into(),
        },
        Todo {
            content: "second".into(),
            status: "pending".into(),
        },
    ];
    let note = tasks_note(&todos).unwrap();
    let lines: Vec<&str> = note.lines().collect();
    assert!(
        lines[1].starts_with("1. [in_progress] first line xxx"),
        "{note}"
    );
    assert!(lines[1].chars().count() <= 220, "{}", lines[1]);
    assert_eq!(lines[2], "2. [pending] second");
}

#[tokio::test(flavor = "current_thread")]
async fn a_compaction_names_the_commands_still_running() {
    let mut agent = compactable("compact-running", 1).await;
    let started = tools::run(
        &agent.ctx,
        "bash",
        json!({"command": "sleep 30", "run_in_background": true}),
        &|| false,
    )
    .await;
    assert!(!started.is_error, "{}", started.text);
    agent.compact(None).await.unwrap();
    let note = agent.messages[2]["content"].as_str().unwrap().to_string();
    agent.stop_tasks();
    assert!(
        note.contains("Still running in the background: task 1 `sleep 30`"),
        "{note}"
    );
    assert!(note.contains("task_stop"), "{note}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_session_resumed_after_a_compaction_keeps_its_task_list() {
    let mut agent = compactable("compact-todos-resume", 1).await;
    assert!(!agent.todo_write(&open_list()).is_error);
    agent.compact(None).await.unwrap();
    let log = std::fs::read_to_string(&agent.session.log_path).unwrap();
    drop(agent);

    let dir = crate::test_temp::TempDir::new("killi-compact-todos-resumed");
    let path = dir.join("session.jsonl");
    std::fs::write(&path, log).unwrap();
    let (locked, report) = Session::resume(&path).unwrap();
    let resumed = Agent::resume_reported(
        test_client(),
        None,
        &dir,
        locked,
        report,
        Vec::new(),
        Settings::default(),
        Controls::default(),
        Rc::new(|_| {}),
    )
    .unwrap();
    let todos: Vec<(&str, &str)> = resumed
        .todos
        .iter()
        .map(|t| (t.content.as_str(), t.status.as_str()))
        .collect();
    assert_eq!(
        todos,
        [
            ("write the parser", "completed"),
            ("test it", "in_progress"),
            ("document it", "pending"),
        ]
    );
}

#[test]
fn a_list_set_after_the_compaction_wins_over_the_one_it_recorded() {
    let dir = crate::test_temp::TempDir::new("killi-compact-todos-later");
    let path = dir.join("session.jsonl");
    let later = json!({"todos": [{"content": "ship it", "status": "in_progress"}]});
    let lines = [
        json!({"type": "compaction", "cause": {"context": 9}, "todos": open_list()["todos"], "messages": [
            {"role": "system", "content": "sys"},
            {"role": "user", "content": "summary"},
        ]}),
        json!({"type": "message", "message": {"role": "assistant", "tool_calls": [{"id": "t1", "type": "function", "function": {"name": "todo_write", "arguments": later.to_string()}}]}}),
        json!({"type": "message", "message": {"role": "tool", "tool_call_id": "t1", "content": "Task list updated: 1 open, 0 done."}}),
        // A rewrite that isn't a compaction records no list and must not bring the old one back.
        json!({"type": "compaction", "cause": {"retracted": 9}, "messages": [
            {"role": "system", "content": "sys"},
            {"role": "user", "content": "summary"},
            {"role": "assistant", "tool_calls": [{"id": "t1", "type": "function", "function": {"name": "todo_write", "arguments": later.to_string()}}]},
            {"role": "tool", "tool_call_id": "t1", "content": "Task list updated: 1 open, 0 done."},
        ]}),
    ]
    .map(|line| line.to_string())
    .join("\n");
    std::fs::write(&path, lines + "\n").unwrap();
    let (locked, report) = Session::resume(&path).unwrap();
    let resumed = Agent::resume_reported(
        test_client(),
        None,
        &dir,
        locked,
        report,
        Vec::new(),
        Settings::default(),
        Controls::default(),
        Rc::new(|_| {}),
    )
    .unwrap();
    let todos: Vec<&str> = resumed.todos.iter().map(|t| t.content.as_str()).collect();
    assert_eq!(todos, ["ship it"]);
}

/// A project folder with `web/AGENTS.md` in it, and an agent working there.
fn agent_with_nested_instructions(name: &str) -> (Agent, PathBuf) {
    let agent = idle_agent(name, Controls::default(), Rc::new(|_| {}));
    let root = agent.ctx.cwd.clone();
    std::fs::create_dir_all(root.join("web/src")).unwrap();
    std::fs::write(root.join("web/AGENTS.md"), "use pnpm").unwrap();
    (agent, root)
}

fn nested_reminders(agent: &Agent) -> usize {
    agent
        .messages
        .iter()
        .filter_map(|m| m["content"].as_str())
        .filter(|text| text.contains("These AGENTS.md files apply"))
        .count()
}

#[tokio::test(flavor = "current_thread")]
async fn a_nested_instruction_file_is_named_again_after_a_compaction() {
    let (mut agent, root) = agent_with_nested_instructions("compact-nested");
    let url = answering_stub_times("summary", 1).await;
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    agent.client = Rc::new(test_client_at("primary", &url));
    agent
        .push(json!({"role": "user", "content": "build it"}))
        .unwrap();
    agent.instructions.touched(&root.join("web/src/app.ts"));
    agent.before_request().await.unwrap();
    agent.instructions.touched(&root.join("web/src/app.ts"));
    agent.before_request().await.unwrap();
    assert_eq!(nested_reminders(&agent), 1, "named once while in view");

    agent.compact(None).await.unwrap();
    assert_eq!(nested_reminders(&agent), 0, "the summary took it");
    agent.instructions.touched(&root.join("web/src/app.ts"));
    agent.before_request().await.unwrap();
    assert_eq!(nested_reminders(&agent), 1, "{:?}", agent.messages);
}

#[tokio::test(flavor = "current_thread")]
async fn a_fresh_subagent_hears_of_nested_instruction_files_its_parent_already_did() {
    let (mut parent, root) = agent_with_nested_instructions("child-nested");
    parent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    parent.instructions.touched(&root.join("web/src/app.ts"));
    parent.before_request().await.unwrap();
    assert_eq!(nested_reminders(&parent), 1);
    parent.current_reply = parent.messages.len();

    let mut fresh = parent
        .subagent("fresh", None, false, &Default::default())
        .unwrap();
    fresh.instructions.touched(&root.join("web/src/app.ts"));
    assert_eq!(
        fresh.instructions.take_found().len(),
        1,
        "its history starts at the system prompt"
    );

    let mut fork = parent
        .subagent("fork", None, true, &Default::default())
        .unwrap();
    fork.instructions.touched(&root.join("web/src/app.ts"));
    assert!(
        fork.instructions.take_found().is_empty(),
        "a fork's history holds the parent's reminder"
    );
}

#[test]
fn a_request_cut_by_one_compaction_keeps_its_note_through_the_next() {
    let long = "字".repeat(5_000);
    let once = kept_prompt(&long, None);
    assert_eq!(cut_at(&once), None);
    assert_eq!(
        kept_prompt(&once, None),
        once,
        "not cut down to its own note"
    );
}

#[test]
fn a_cut_request_says_where_the_whole_text_was_saved() {
    let long = "a".repeat(4_100);
    let kept = kept_prompt(&long, Some(Path::new("/s/request-1.txt")));
    assert!(
        kept.ends_with("\n[… 100 characters cut; the whole request is saved at /s/request-1.txt]"),
        "{kept}"
    );
    assert_eq!(kept_prompt(&kept, None), kept);
}

#[tokio::test(flavor = "current_thread")]
async fn a_compaction_saves_the_whole_text_of_a_request_it_cuts() {
    let url = answering_stub_times("summary", 1).await;
    let mut agent = idle_agent(
        "compact-saved-request",
        Controls::default(),
        Rc::new(|_| {}),
    );
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    agent.client = Rc::new(test_client_at("primary", &url));
    let long = format!("spec: {}", "must ".repeat(1_000));
    agent
        .push(json!({"role": "user", "content": long}))
        .unwrap();
    agent.compact(None).await.unwrap();
    let summary = agent.messages[1]["content"].as_str().unwrap();
    let saved = summary
        .split_once("the whole request is saved at ")
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(path, _)| path)
        .expect(summary);
    assert_eq!(std::fs::read_to_string(saved).unwrap(), long);
}

#[tokio::test(flavor = "current_thread")]
async fn a_turn_that_outgrows_the_window_twice_compacts_twice() {
    let overflow = "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"error\":{\"message\":\"maximum context length exceeded\"}}".to_string();
    let good = |body: String| {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
        )
    };
    let call = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"todo_write\",\"arguments\":\"{\\\"todos\\\":[]}\"}}]}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n";
    let url = serve_raw(vec![
        overflow.clone(),
        good(sse("summary one", "stop")),
        good(call.to_string()),
        overflow,
        good(sse("summary two", "stop")),
        good(sse("Done.", "stop")),
    ])
    .await;
    let mut agent = idle_agent("overflow-twice", Controls::default(), Rc::new(|_| {}));
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    agent.client = Rc::new(test_client_at("primary", &url));
    let (text, stop) = agent.run("build it", &[]).await.unwrap();
    assert_eq!((text.as_str(), stop), ("Done.", Stop::Completed));
    let summary = agent.messages[1]["content"].as_str().unwrap();
    assert!(summary.contains("summary two"), "{summary}");
}

/// Appends one step to `agent`'s history: a reply that calls `read`, and the call's result.
fn push_step(agent: &mut Agent, id: &str, result: &str) {
    agent
        .push(json!({"role": "assistant", "content": "", "tool_calls": [{"id": id, "type": "function", "function": {"name": "read", "arguments": format!("{{\"path\":\"{id}\"}}")}}]}))
        .unwrap();
    agent
        .push(json!({"role": "tool", "tool_call_id": id, "content": result}))
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn a_compaction_keeps_the_newest_steps_word_for_word() {
    let mut agent = compactable("compact-tail", 1).await;
    push_step(&mut agent, "old", &"x".repeat(60_000));
    push_step(&mut agent, "mid", "1\talpha");
    push_step(&mut agent, "new", "1\tbeta");
    let before = agent.messages[4..].to_vec();
    agent.compact(None).await.unwrap();
    assert_eq!(
        agent.messages[2..],
        before[..],
        "the two small steps, as they were"
    );
    let summary = agent.messages[1]["content"].as_str().unwrap();
    assert!(
        summary.contains("The last steps before the compaction follow this message"),
        "{summary}"
    );
    let log = std::fs::read_to_string(&agent.session.log_path).unwrap();
    assert!(log.contains("\"kept_messages\":4"), "{log}");
    assert!(
        agent.compact_floor < 5_000,
        "the step that filled the context is gone: {}",
        agent.compact_floor
    );
}

#[tokio::test(flavor = "current_thread")]
async fn kept_steps_lose_their_signed_thinking_and_nothing_else() {
    let mut agent = compactable("compact-tail-thinking", 1).await;
    push_step(&mut agent, "old", &"x".repeat(60_000));
    agent
        .push(json!({
            "role": "assistant",
            "content": "",
            "reasoning_content": "why",
            "reasoning_items": [
                {"type": "thinking", "thinking": "hm", "signature": "sig"},
                {"type": "reasoning", "encrypted_content": "blob"},
            ],
            "tool_calls": [{"id": "new", "type": "function", "function": {"name": "read", "arguments": "{}"}, "extra_content": {"google": {"thought_signature": "s"}}}],
        }))
        .unwrap();
    agent
        .push(json!({"role": "tool", "tool_call_id": "new", "content": "1\tbeta"}))
        .unwrap();
    agent
        .push(json!({"role": "assistant", "content": "done", "reasoning_items": [{"type": "redacted_thinking", "data": "d"}]}))
        .unwrap();
    agent.compact(None).await.unwrap();
    let step = &agent.messages[2];
    assert_eq!(
        step["reasoning_items"],
        json!([{"type": "reasoning", "encrypted_content": "blob"}])
    );
    assert_eq!(step["reasoning_content"], "why");
    assert_eq!(
        step["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
        "s"
    );
    assert_eq!(
        agent.messages[4],
        json!({"role": "assistant", "content": "done"}),
        "a list left empty goes with its blocks"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_request_typed_inside_the_kept_steps_is_listed_once() {
    let mut agent = compactable("compact-tail-prompt", 2).await;
    push_step(&mut agent, "old", &"x".repeat(60_000));
    agent
        .push(json!({"role": "assistant", "content": "Built."}))
        .unwrap();
    agent
        .push(json!({"role": "user", "content": "now test it"}))
        .unwrap();
    push_step(&mut agent, "new", "1\tbeta");
    agent.compact(None).await.unwrap();
    let summary = agent.messages[1]["content"].as_str().unwrap();
    assert!(
        summary.contains("<request>\nbuild it\n</request>") && !summary.contains("now test it"),
        "{summary}"
    );
    assert_eq!(agent.messages[3]["content"], "now test it");
    assert_eq!(agent.user_prompts(), ["build it", "now test it"]);
    // The next compaction moves it into the list, still once.
    push_step(&mut agent, "big", &"y".repeat(60_000));
    push_step(&mut agent, "last", "ok");
    agent.compact(None).await.unwrap();
    assert_eq!(agent.user_prompts(), ["build it", "now test it"]);
}

#[test]
fn the_kept_steps_start_at_a_reply_and_fit_the_budget() {
    let step = |id: &str, size: usize| {
        [
            json!({"role": "assistant", "content": "", "tool_calls": [{"id": id, "type": "function", "function": {"name": "read", "arguments": "{}"}}]}),
            json!({"role": "tool", "tool_call_id": id, "content": "x".repeat(size)}),
        ]
    };
    let mut messages = vec![
        json!({"role": "system", "content": "sys"}),
        json!({"role": "user", "content": "build it"}),
    ];
    messages.extend(step("a", 4_000));
    messages.extend(step("b", 400));
    messages.push(json!({"role": "user", "content": "<system-reminder>note</system-reminder>"}));
    assert_eq!(
        tail_start(&messages, 1_000),
        Some(4),
        "the small step and what follows it"
    );
    assert_eq!(
        tail_start(&messages, 100_000),
        Some(2),
        "never the request itself"
    );
    assert_eq!(
        tail_start(&messages, 100),
        None,
        "not even the last step fits"
    );
    messages.push(json!({"role": "assistant", "content": "", "tool_calls": [{"id": "c", "type": "function", "function": {"name": "read", "arguments": "{}"}}]}));
    assert_eq!(
        tail_start(&messages, 100_000),
        None,
        "a call still waits for its result"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_small_conversation_keeps_at_most_a_quarter_of_itself() {
    let mut agent = compactable("compact-tail-small", 1).await;
    for id in ["a", "b", "c", "d", "e", "f", "g", "h"] {
        push_step(&mut agent, id, &"x".repeat(2_000));
    }
    agent.compact(None).await.unwrap();
    assert_eq!(
        agent.messages.len(),
        2 + 2 * 2,
        "{:?}",
        agent.messages.len()
    );
}
