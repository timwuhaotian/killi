use super::*;

use tokio::sync::mpsc;

use crate::agent::background::{BgStart, STOPPED_FOR_MESSAGE, UNANSWERED_MESSAGE};

/// Controls for an interactive session whose background work arrives on the returned receiver,
/// as the terminal's would.
fn attached() -> (Controls, mpsc::UnboundedReceiver<BgFuture>) {
    let controls = Controls {
        interactive: true,
        ..Controls::default()
    };
    let (tx, rx) = mpsc::unbounded_channel();
    controls.background.borrow_mut().attach(tx);
    (controls, rx)
}

/// Answers each request with the next reply after `delay`, as a slow model would.
async fn slow_stub(replies: Vec<String>, delay: Duration) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        for reply in replies {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut chunk = [0u8; 65536];
            let _ = socket.read(&mut chunk).await;
            tokio::time::sleep(delay).await;
            let body = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{reply}"
            );
            let _ = socket.write_all(body.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    url
}

fn agent_call(id: &str, args: Value) -> (String, ToolCall) {
    (
        id.to_string(),
        ToolCall {
            id: id.into(),
            name: "agent".into(),
            arguments: args.to_string(),
        },
    )
}

fn tool_call(id: &str, name: &str, args: Value) -> (String, ToolCall) {
    (
        id.to_string(),
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args.to_string(),
        },
    )
}

/// One SSE reply that calls `name` with `args`.
fn tool_reply(id: &str, name: &str, args: Value) -> String {
    let chunk = json!({"choices": [{"delta": {"tool_calls": [{
        "index": 0, "id": id, "type": "function",
        "function": {"name": name, "arguments": args.to_string()}
    }]}, "finish_reason": "tool_calls"}]});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn last_tool(agent: &Agent) -> String {
    agent
        .messages
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn reminders_with(agent: &Agent, needle: &str) -> usize {
    agent
        .messages
        .iter()
        .filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"].as_str())
        .filter(|text| text.contains(needle))
        .count()
}

#[tokio::test(flavor = "current_thread")]
async fn a_message_moves_a_running_subagent_to_the_background_and_its_report_comes_once() {
    let url = slow_stub(
        vec![sse("found the bug\\nStatus: ready", "stop")],
        Duration::from_millis(600),
    )
    .await;
    let (controls, mut rx) = attached();
    let mut agent = idle_agent("bg-focus", controls.clone(), Rc::new(|_| {}));
    Rc::get_mut(&mut agent.client).unwrap().model.base_url = url;
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    let calls = vec![agent_call(
        "a",
        json!({"prompt": "find the bug", "description": "hunt"}),
    )];
    let steer = controls.steer.clone();
    let (ran, ()) = tokio::join!(agent.execute_calls(calls, false), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        steer
            .borrow_mut()
            .push_back(("what is 2+2?".into(), Vec::new()));
    });
    ran.unwrap();
    let moved = last_tool(&agent);
    assert!(
        moved.contains("Moved to the background as task 1"),
        "{moved}"
    );
    assert_eq!(
        controls.background.borrow().tasks[&1].state,
        BgState::Running
    );
    // The terminal polls the work to its end.
    let work = rx.recv().await.expect("the work went to the terminal");
    assert_eq!(work.await, 1);
    assert_eq!(controls.background.borrow().tasks[&1].state, BgState::Done);
    agent.deliver_background().unwrap();
    agent.deliver_background().unwrap();
    assert_eq!(reminders_with(&agent, "found the bug"), 1);
    assert!(controls.background.borrow().tasks[&1].delivered);
    // Read again, it says the model already has it.
    let again = agent.background_output(1, &json!({"id": 1})).await;
    assert!(again.text.contains("already had"), "{}", again.text);
    let log = std::fs::read_to_string(&agent.session.log_path).unwrap();
    assert!(log.contains("\"type\":\"bg_start\""), "{log}");
    assert!(log.contains("\"type\":\"bg_end\""), "{log}");
}

#[tokio::test(flavor = "current_thread")]
async fn without_a_terminal_a_subagent_stays_in_the_foreground() {
    let url = stub_replies(vec![sse("done\\nStatus: ready", "stop")]).await;
    // Interactive, but nothing polls background work: as in `-p`.
    let controls = Controls {
        interactive: true,
        ..Controls::default()
    };
    let mut agent = idle_agent("bg-headless", controls.clone(), Rc::new(|_| {}));
    Rc::get_mut(&mut agent.client).unwrap().model.base_url = url;
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    controls.mode.set(crate::mode::Mode::Orchestrate);
    let calls = vec![agent_call(
        "a",
        json!({"prompt": "go", "description": "kid"}),
    )];
    agent.execute_calls(calls, false).await.unwrap();
    assert!(last_tool(&agent).contains("done"), "{}", last_tool(&agent));
    assert!(controls.background.borrow().tasks.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn orchestrate_starts_a_forked_subagent_in_the_background_at_once() {
    let (controls, mut rx) = attached();
    controls.mode.set(crate::mode::Mode::Orchestrate);
    let mut agent = idle_agent("bg-orchestrate", controls.clone(), Rc::new(|_| {}));
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    agent
        .push(json!({"role": "user", "content": "the parent's own request"}))
        .unwrap();
    agent.current_reply = agent.messages.len();
    let calls = vec![agent_call(
        "a",
        json!({"prompt": "go", "description": "kid"}),
    )];
    agent.execute_calls(calls, false).await.unwrap();
    let moved = last_tool(&agent);
    assert!(
        moved.contains("Running in the background as task 1"),
        "{moved}"
    );
    assert!(rx.try_recv().is_ok(), "the work went to the terminal");
    // Without `fork`, it starts from this conversation.
    let child_log = std::fs::read_to_string(agent.session.dir.join("agent-1.jsonl")).unwrap();
    assert!(
        child_log.contains("the parent's own request"),
        "{child_log}"
    );
    // The model hears of the mode once, at its next step.
    agent.before_request().await.unwrap();
    agent.before_request().await.unwrap();
    assert_eq!(reminders_with(&agent, "Orchestrate mode is on"), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn a_stopped_background_subagent_brings_no_report() {
    let (controls, mut rx) = attached();
    controls.mode.set(crate::mode::Mode::Orchestrate);
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut agent = idle_agent("bg-stop", controls.clone(), Rc::new(|_| {}));
    Rc::get_mut(&mut agent.client).unwrap().model.base_url =
        format!("http://{}", silent.local_addr().unwrap());
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    let calls = vec![agent_call(
        "a",
        json!({"prompt": "go", "description": "kid", "fork": false}),
    )];
    agent.execute_calls(calls, false).await.unwrap();
    let work = rx.recv().await.unwrap();
    let running = agent.background_output(1, &json!({"id": 1})).await;
    assert!(running.text.contains("Task 1"), "{}", running.text);
    let stop = vec![tool_call("s", "task_stop", json!({"id": 1}))];
    agent.execute_calls(stop, false).await.unwrap();
    assert!(last_tool(&agent).contains("Stopped task 1"));
    assert_eq!(work.await, 1);
    assert_eq!(
        controls.background.borrow().tasks[&1].state,
        BgState::Stopped
    );
    let before = agent.messages.len();
    agent.deliver_background().unwrap();
    assert_eq!(agent.messages.len(), before, "nothing to deliver");
}

#[tokio::test(flavor = "current_thread")]
async fn agent_message_reaches_a_background_subagent_and_the_tree_is_guarded() {
    let (controls, _rx) = attached();
    controls.mode.set(crate::mode::Mode::Orchestrate);
    let mut agent = idle_agent("bg-message", controls.clone(), Rc::new(|_| {}));
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    let calls = vec![agent_call(
        "a",
        json!({"prompt": "go", "description": "kid", "fork": false}),
    )];
    agent.execute_calls(calls, false).await.unwrap();
    let message = vec![tool_call(
        "m",
        "agent_message",
        json!({"id": 1, "message": "use the v2 API"}),
    )];
    agent.execute_calls(message, false).await.unwrap();
    assert!(
        last_tool(&agent).contains("Told task 1"),
        "{}",
        last_tool(&agent)
    );
    let queued = controls.background.borrow().tasks[&1]
        .steers
        .first()
        .unwrap()
        .borrow()
        .front()
        .map(|(text, _)| text.clone())
        .unwrap();
    assert!(queued.contains("use the v2 API") && queued.contains("not from the user"));
    let unknown = vec![tool_call(
        "u",
        "agent_message",
        json!({"id": 9, "message": "hi"}),
    )];
    agent.execute_calls(unknown, false).await.unwrap();
    assert!(last_tool(&agent).contains("No subagent works in the background as task 9"));
    // While it works, the main agent leaves the tree where it is; nothing polls it here, so
    // it is marked as having its slot, as the terminal's polling would.
    controls
        .background
        .borrow_mut()
        .tasks
        .get_mut(&1)
        .unwrap()
        .state = BgState::Running;
    let git = vec![tool_call(
        "g",
        "bash",
        json!({"command": "git checkout -- ."}),
    )];
    agent.execute_calls(git, false).await.unwrap();
    let refused = last_tool(&agent);
    assert!(
        refused.contains("background task 1") && refused.starts_with("Not run"),
        "{refused}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_wake_turn_reads_the_report_before_its_prompt_and_is_no_user_prompt() {
    let url = stub_replies(vec![
        sse("report text\\nStatus: ready", "stop"),
        sse("told the user", "stop"),
    ])
    .await;
    let (controls, mut rx) = attached();
    controls.mode.set(crate::mode::Mode::Orchestrate);
    let mut agent = idle_agent("bg-wake", controls.clone(), Rc::new(|_| {}));
    Rc::get_mut(&mut agent.client).unwrap().model.base_url = url;
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    let calls = vec![agent_call(
        "a",
        json!({"prompt": "go", "description": "kid", "fork": false}),
    )];
    agent.execute_calls(calls, false).await.unwrap();
    rx.recv().await.unwrap().await;
    let (text, _) = agent.wake().await.unwrap();
    assert_eq!(text, "told the user");
    let report = agent
        .messages
        .iter()
        .position(|m| {
            m["content"]
                .as_str()
                .is_some_and(|t| t.contains("report text"))
        })
        .unwrap();
    let wake = agent
        .messages
        .iter()
        .position(|m| m["content"].as_str() == Some(crate::agent::background::WAKE_PROMPT))
        .unwrap();
    assert!(report < wake, "the report comes before the prompt");
    assert!(
        agent.user_prompts().is_empty(),
        "{:?}",
        agent.user_prompts()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_resumed_session_continues_its_paused_subagents_from_their_logs() {
    let url = stub_replies(vec![sse(
        "finished after the resume\\nStatus: ready",
        "stop",
    )])
    .await;
    let dir = crate::test_temp::TempDir::new("killi-bg-resume");
    let log = dir.join("1-1.jsonl");
    let folder = dir.join("1-1");
    std::fs::create_dir_all(&folder).unwrap();
    let sent = "do it\n\nEnd your final message with one line";
    let line = |value: Value| value.to_string();
    // One subagent's log ends on its report; the other stopped mid-task.
    std::fs::write(
        folder.join("agent-1.jsonl"),
        [
            line(json!({"type": "compaction", "messages": [{"role": "system", "content": "sys"}]})),
            line(json!({"type": "message", "message": {"role": "user", "content": sent}})),
            line(json!({"type": "message", "message": {"role": "assistant", "content": "already reported"}})),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    std::fs::write(
        folder.join("agent-2.jsonl"),
        [
            line(json!({"type": "compaction", "messages": [{"role": "system", "content": "sys"}]})),
            line(json!({"type": "message", "message": {"role": "user", "content": sent}})),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    std::fs::write(
        &log,
        [
            line(json!({"type": "message", "message": {"role": "system", "content": "sys"}})),
            line(json!({"type": "message", "message": {"role": "user", "content": "hi"}})),
            line(json!({"type": "bg_start", "id": 4, "label": "done one", "items": [], "logs": ["agent-1.jsonl"], "sent": [sent]})),
            line(json!({"type": "bg_start", "id": 6, "label": "paused one", "items": [], "logs": ["agent-2.jsonl"], "sent": [sent]})),
            // A compaction doesn't close background work.
            line(json!({"type": "compaction", "messages": [{"role": "system", "content": "sys"}, {"role": "user", "content": "hi"}]})),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    let (controls, mut rx) = attached();
    let (locked, report) = Session::resume(&log).unwrap();
    let mut agent = Agent::resume_reported(
        test_client_at("test", &url),
        None,
        &dir,
        locked,
        report,
        Vec::new(),
        Settings::default(),
        controls.clone(),
        Rc::new(|_| {}),
    )
    .unwrap();
    assert_eq!(agent.resume_background(), 2);
    // Ids it kept stay taken.
    assert_eq!(agent.ctx.tasks.borrow_mut().reserve_id(), 7);
    let mut ended = Vec::new();
    for _ in 0..2 {
        ended.push(rx.recv().await.unwrap().await);
    }
    ended.sort();
    assert_eq!(ended, [4, 6]);
    agent.deliver_background().unwrap();
    assert_eq!(reminders_with(&agent, "already reported"), 1);
    assert_eq!(reminders_with(&agent, "finished after the resume"), 1);
    let continued = std::fs::read_to_string(folder.join("agent-2.jsonl")).unwrap();
    assert!(
        continued.contains("stopped while you were working"),
        "{continued}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_message_moves_a_running_swarm_to_the_background_as_one_task() {
    // The member about to end reads the message and takes one more step; the other starts
    // with it.
    let url = slow_stub(
        vec![
            sse("one\\nStatus: ready", "stop"),
            sse("one, with the new emphasis\\nStatus: ready", "stop"),
            sse("two\\nStatus: ready", "stop"),
        ],
        Duration::from_millis(500),
    )
    .await;
    let (controls, mut rx) = attached();
    let mut agent = idle_agent("bg-swarm", controls.clone(), Rc::new(|_| {}));
    agent.swarm_parallel.set(1);
    Rc::get_mut(&mut agent.client).unwrap().model.base_url = url;
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    let calls = vec![tool_call(
        "s",
        "agent_swarm",
        json!({"description": "pair", "prompt_template": "do {{item}}", "items": ["a", "b"]}),
    )];
    let steer = controls.steer.clone();
    let (ran, ()) = tokio::join!(agent.execute_calls(calls, false), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        steer.borrow_mut().push_back(("hello".into(), Vec::new()));
    });
    ran.unwrap();
    assert!(last_tool(&agent).contains("Moved to the background as task 1"));
    assert_eq!(controls.background.borrow().tasks[&1].members, 2);
    assert!(
        agent.partial_swarm.is_none(),
        "the call is answered by its task"
    );
    // The message reaches every member still working, the one running and the one waiting
    // for its turn alike.
    let message = agent.background_message(1, &json!({"id": 1, "message": "focus on security"}));
    assert!(!message.is_error, "{}", message.text);
    assert!(
        message.text.contains("2 subagents of task 1"),
        "{}",
        message.text
    );
    for steer in &controls.background.borrow().tasks[&1].steers {
        let queued = steer
            .borrow()
            .front()
            .map(|(text, _)| text.clone())
            .unwrap();
        assert!(queued.contains("focus on security") && queued.contains("not from the user"));
    }
    assert_eq!(rx.recv().await.unwrap().await, 1);
    assert!(
        controls.background.borrow().tasks[&1].steers.is_empty(),
        "a member that ended no longer takes messages"
    );
    let second = std::fs::read_to_string(agent.session.dir.join("agent-2.jsonl")).unwrap();
    assert!(second.contains("focus on security"), "{second}");
    let (report, failed) = controls.background.borrow().tasks[&1]
        .report
        .clone()
        .unwrap();
    assert!(!failed, "{report}");
    let (one, two) = (report.find("one").unwrap(), report.find("two").unwrap());
    assert!(one < two, "items in order: {report}");
    let late = agent.background_message(1, &json!({"id": 1, "message": "hi"}));
    assert!(
        late.is_error && late.text.contains("already ended"),
        "{}",
        late.text
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_subagent_paused_mid_call_answers_the_call_and_goes_on() {
    let url = stub_replies(vec![sse("went on\\nStatus: ready", "stop")]).await;
    let dir = crate::test_temp::TempDir::new("killi-bg-midcall");
    let log = dir.join("1-1.jsonl");
    let folder = dir.join("1-1");
    std::fs::create_dir_all(&folder).unwrap();
    let sent = "build it";
    let line = |value: Value| value.to_string();
    // It stopped with two calls out, one answered.
    std::fs::write(
        folder.join("agent-1.jsonl"),
        [
            line(json!({"type": "compaction", "messages": [{"role": "system", "content": "sys"}]})),
            line(json!({"type": "message", "message": {"role": "user", "content": sent}})),
            line(json!({"type": "message", "message": {"role": "assistant", "content": "", "tool_calls": [
                {"id": "r1", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a\"}"}},
                {"id": "b1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"make\"}"}}
            ]}})),
            line(json!({"type": "message", "message": {"role": "tool", "tool_call_id": "r1", "content": "a's text"}})),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    std::fs::write(
        &log,
        [
            line(json!({"type": "message", "message": {"role": "system", "content": "sys"}})),
            line(json!({"type": "message", "message": {"role": "user", "content": "hi"}})),
            line(json!({"type": "bg_start", "id": 2, "label": "build", "items": [], "logs": ["agent-1.jsonl"], "sent": [sent]})),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    let (controls, mut rx) = attached();
    let (locked, report) = Session::resume(&log).unwrap();
    let mut agent = Agent::resume_reported(
        test_client_at("test", &url),
        None,
        &dir,
        locked,
        report,
        Vec::new(),
        Settings::default(),
        controls.clone(),
        Rc::new(|_| {}),
    )
    .unwrap();
    assert_eq!(agent.resume_background(), 1);
    assert_eq!(rx.recv().await.unwrap().await, 2);
    let (report, failed) = controls.background.borrow().tasks[&2]
        .report
        .clone()
        .unwrap();
    assert!(!failed && report.contains("went on"), "{report}");
    let continued = std::fs::read_to_string(folder.join("agent-1.jsonl")).unwrap();
    let b1 = continued
        .find("\"tool_call_id\":\"b1\"")
        .expect("the open call is answered");
    let note = continued
        .find("stopped while you were working")
        .expect("the note is sent");
    assert!(
        b1 < note,
        "the call's result comes before the note: {continued}"
    );
    assert!(!continued.contains("The user interrupted"), "{continued}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_reply_announcing_background_work_ends_the_turn() {
    let url = stub_replies(vec![
        tool_reply(
            "a",
            "agent",
            json!({"prompt": "go", "description": "kid", "fork": false}),
        ),
        sse(
            "Started the review in the background; next I'll report what it finds.",
            "stop",
        ),
    ])
    .await;
    let (controls, _rx) = attached();
    controls.mode.set(crate::mode::Mode::Orchestrate);
    let mut agent = idle_agent("bg-announce", controls.clone(), Rc::new(|_| {}));
    Rc::get_mut(&mut agent.client).unwrap().model.base_url = url;
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    let (text, stop) = agent.run("review it", &[]).await.unwrap();
    assert_eq!(stop, Stop::Completed);
    assert!(text.contains("in the background"), "{text}");
    assert_eq!(agent.stats.requests, 2, "no extra request to go on");
}

/// What a test task starts as: already running, so it needs no slot.
fn running_start(label: &str) -> BgStart {
    BgStart {
        label: label.into(),
        members: 1,
        logs: Vec::new(),
        sent: Vec::new(),
        items: Vec::new(),
        steers: Vec::new(),
        started: true,
        since: Instant::now(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_wait_ends_when_another_task_does_and_a_timed_out_one_says_not_to_poll() {
    let (controls, mut rx) = attached();
    let mut agent = idle_agent("bg-wait", controls.clone(), Rc::new(|_| {}));
    let slow = agent.launch(
        running_start("slow"),
        Box::pin(std::future::pending::<(tools::Output, Stats)>()),
    );
    let quick = agent.launch(
        running_start("quick"),
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            (tools::Output::ok("quick report"), Stats::default())
        }),
    );
    let _slow_work = rx.recv().await.unwrap();
    let quick_work = rx.recv().await.unwrap();
    // A wait on the slow task stops once the quick one ends, long before its 30 s.
    let begun = Instant::now();
    let args = json!({"id": slow, "wait": 30});
    let (waited, ended) = tokio::join!(agent.background_output(slow, &args), quick_work);
    assert_eq!(ended, quick);
    assert!(begun.elapsed() < Duration::from_secs(5));
    assert!(
        waited
            .text
            .contains(&format!("[Stopped waiting: background task {quick} ended")),
        "{}",
        waited.text
    );
    assert!(!waited.text.contains("Rather than waiting again"));
    // Its report reaches the model at the next step.
    agent.deliver_background().unwrap();
    assert_eq!(reminders_with(&agent, "quick report"), 1);
    // A wait that runs out says the report comes by itself; a plain look doesn't.
    let timed_out = agent
        .background_output(slow, &json!({"id": slow, "wait": 1}))
        .await;
    assert!(
        timed_out.text.contains("is still running")
            && timed_out.text.contains("Rather than waiting again"),
        "{}",
        timed_out.text
    );
    let look = agent.background_output(slow, &json!({"id": slow})).await;
    assert!(
        !look.text.contains("Rather than waiting again"),
        "{}",
        look.text
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_wait_right_after_one_stopped_for_a_message_is_refused_until_the_model_replies() {
    let (controls, mut rx) = attached();
    let mut agent = idle_agent("bg-rewait", controls.clone(), Rc::new(|_| {}));
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    let slow = agent.launch(
        running_start("slow"),
        Box::pin(std::future::pending::<(tools::Output, Stats)>()),
    );
    let _work = rx.recv().await.unwrap();
    // A wait stops for a message from the user, which then reaches the model.
    controls
        .steer
        .borrow_mut()
        .push_back(("the plan first, please".into(), Vec::new()));
    let stopped = agent
        .background_output(slow, &json!({"id": slow, "wait": 30}))
        .await;
    assert!(
        stopped.text.contains(STOPPED_FOR_MESSAGE),
        "{}",
        stopped.text
    );
    agent
        .push(json!({"role": "tool", "tool_call_id": "w0", "content": stopped.text}))
        .unwrap();
    agent.before_request().await.unwrap();
    let reply = |ids: &[&str], args: Value, text: &str| {
        json!({"role": "assistant", "content": text, "tool_calls": ids
            .iter()
            .map(|id| json!({"id": id, "type": "function", "function": {
                "name": "task_output", "arguments": args.to_string()
            }}))
            .collect::<Vec<_>>()})
    };
    let results = |agent: &Agent, from: usize| -> Vec<String> {
        agent.messages[from..]
            .iter()
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| m["content"].as_str().map(String::from))
            .collect()
    };
    // Waiting again without a word back is refused, for every wait of that reply, at once.
    let wait = json!({"id": slow, "wait": 30});
    agent.push(reply(&["w1", "w2"], wait.clone(), "")).unwrap();
    let from = agent.messages.len();
    let begun = Instant::now();
    let calls = vec![
        tool_call("w1", "task_output", wait.clone()),
        tool_call("w2", "task_output", wait),
    ];
    agent.execute_calls(calls, false).await.unwrap();
    assert!(begun.elapsed() < Duration::from_secs(5));
    let refused = results(&agent, from);
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert!(
        refused
            .iter()
            .all(|text| text.contains(UNANSWERED_MESSAGE) && text.contains("Reply to it")),
        "{refused:?}"
    );
    // A plain look still runs, and so does a wait once the model has replied.
    agent.push(reply(&["l"], json!({"id": slow}), "")).unwrap();
    let from = agent.messages.len();
    agent
        .execute_calls(
            vec![tool_call("l", "task_output", json!({"id": slow}))],
            false,
        )
        .await
        .unwrap();
    assert!(
        results(&agent, from)[0].contains("is still running"),
        "{:?}",
        results(&agent, from)
    );
    agent
        .push(reply(
            &["w3"],
            json!({"id": slow, "wait": 1}),
            "The plan, as asked:",
        ))
        .unwrap();
    let from = agent.messages.len();
    agent
        .execute_calls(
            vec![tool_call(
                "w3",
                "task_output",
                json!({"id": slow, "wait": 1}),
            )],
            false,
        )
        .await
        .unwrap();
    let waited = &results(&agent, from)[0];
    assert!(
        waited.contains("is still running") && !waited.contains(UNANSWERED_MESSAGE),
        "{waited}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_resume_numbers_past_every_task_the_log_named() {
    let dir = crate::test_temp::TempDir::new("killi-bg-ids");
    let log = dir.join("1-1.jsonl");
    std::fs::create_dir_all(dir.join("1-1")).unwrap();
    let line = |value: Value| value.to_string();
    let call = |id: &str, name: &str| {
        json!({"role": "assistant", "content": "", "tool_calls": [
            {"id": id, "type": "function", "function": {"name": name, "arguments": "{}"}}
        ]})
    };
    std::fs::write(
        &log,
        [
            line(json!({"type": "message", "message": {"role": "system", "content": "sys"}})),
            line(json!({"type": "message", "message": {"role": "user", "content": "hi"}})),
            line(json!({"type": "message", "message": call("a", "agent_swarm")})),
            line(json!({"type": "bg_start", "id": 1, "label": "swarm", "items": [], "logs": [], "sent": []})),
            line(json!({"type": "message", "message": {"role": "tool", "tool_call_id": "a", "content": "Running in the background as task 1. Its report arrives as a reminder when it ends."}})),
            line(json!({"type": "bg_end", "id": 1})),
            line(json!({"type": "message", "message": call("b", "bash")})),
            line(json!({"type": "message", "message": {"role": "tool", "tool_call_id": "b", "content": "Started background task 4. Read its output with task_output."}})),
            line(json!({"type": "message", "message": {"role": "assistant", "content": "done"}})),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    let (controls, _rx) = attached();
    let (locked, report) = Session::resume(&log).unwrap();
    let agent = Agent::resume_reported(
        test_client_at("test", "http://127.0.0.1:9"),
        None,
        &dir,
        locked,
        report,
        Vec::new(),
        Settings::default(),
        controls,
        Rc::new(|_| {}),
    )
    .unwrap();
    // Task 1 ended and bash task 4 died with the old process; neither id is handed out again.
    assert_eq!(agent.ctx.tasks.borrow_mut().reserve_id(), 5);
}
