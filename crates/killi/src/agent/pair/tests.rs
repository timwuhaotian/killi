use super::super::tests::{idle_agent, sse, stub_replies, test_client_at};
use super::*;

fn config() -> PairConfig {
    PairConfig {
        mentor_model: "mentor-test".into(),
        mentor_effort: "high".into(),
        executor_model: "executor-test".into(),
        executor_effort: "low".into(),
    }
}

/// A parent with a system prompt for the pair children to inherit.
fn parent(name: &str) -> Agent {
    let mut agent = idle_agent(name, Controls::default(), Rc::new(|_| {}));
    agent
        .push(json!({"role": "system", "content": "test"}))
        .unwrap();
    agent
}

/// Serves `replies` as SSE responses, reading each request's body before answering: pair
/// requests grow with the history, so answering on the headers alone can close the
/// connection while the client is still sending.
async fn stub_replies_full(replies: Vec<String>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
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
            let body = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{reply}"
            );
            let _ = socket.write_all(body.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    url
}

#[test]
fn the_done_mark_needs_a_line_of_its_own() {
    assert!(is_done("TASK_COMPLETE"));
    assert!(is_done("looks good\nTASK_COMPLETE"));
    assert!(is_done("looks good\n`TASK_COMPLETE`"));
    assert!(!is_done("will TASK_COMPLETE soon"));
    assert!(!is_done("nothing here"));
}

#[test]
fn the_prompts_carry_the_protocol() {
    let kickoff = kickoff_prompt("fix it");
    assert!(kickoff.contains("fix it"), "{kickoff}");
    assert!(kickoff.contains("Do not write TASK_COMPLETE"), "{kickoff}");
    let handoff = handoff_prompt("the plan", false);
    assert!(handoff.contains("never write TASK_COMPLETE"), "{handoff}");
    assert!(handoff.contains("the plan"), "{handoff}");
    let handoff = handoff_prompt("the plan", true);
    assert!(
        handoff.contains("You wrote TASK_COMPLETE last round"),
        "{handoff}"
    );
    let review = review_prompt(2, "the report");
    assert!(review.contains("the report"), "{review}");
    assert!(review.contains("exactly `TASK_COMPLETE`"), "{review}");
    let replan = replan_prompt(&["go left".to_string()]);
    assert!(replan.contains("go left"), "{replan}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_review_with_the_mark_ends_the_pair_after_one_round() {
    // Newlines stay escaped: the text goes into one JSON string of the SSE reply.
    let mentor_url = stub_replies(vec![
        sse("the plan", "stop"),
        sse("good work\\nTASK_COMPLETE", "stop"),
    ])
    .await;
    let executor_url = stub_replies(vec![sse("did it", "stop")]).await;
    let mut agent = parent("done");
    agent
        .pair_begin(
            &config(),
            test_client_at("mentor-test", &mentor_url),
            test_client_at("executor-test", &executor_url),
            "do the thing",
            vec![],
        )
        .unwrap();
    agent.pair_drive().await.unwrap();
    assert_eq!(agent.pair_outcome(), Some(Outcome::Done));
    assert!(
        agent.pair_status().unwrap().contains("1 round"),
        "{}",
        agent.pair_status().unwrap()
    );
    let split = agent.pair_split().unwrap();
    assert_eq!((split[0].requests, split[1].requests), (2, 1));
    assert_eq!(
        agent.messages.len(),
        1,
        "the session's own history is untouched"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_mentor_sees_reports_and_the_executor_keeps_its_own_work() {
    let mentor_url = stub_replies(vec![
        sse("PLAN-ONE", "stop"),
        sse("FEEDBACK-TWO", "stop"),
        sse("all good\\nTASK_COMPLETE", "stop"),
    ])
    .await;
    let executor_url =
        stub_replies(vec![sse("REPORT-ONE", "stop"), sse("REPORT-TWO", "stop")]).await;
    let mut agent = parent("histories");
    agent
        .pair_begin(
            &config(),
            test_client_at("mentor-test", &mentor_url),
            test_client_at("executor-test", &executor_url),
            "do the thing",
            vec![],
        )
        .unwrap();
    agent.pair_drive().await.unwrap();
    assert_eq!(agent.pair_outcome(), Some(Outcome::Done));
    let dir = agent.session.dir.clone();
    let mentor_log = std::fs::read_to_string(dir.join("agent-1.jsonl")).unwrap();
    let executor_log = std::fs::read_to_string(dir.join("agent-2.jsonl")).unwrap();
    assert!(
        mentor_log.contains("do the thing"),
        "the mentor keeps the task"
    );
    assert!(
        mentor_log.contains("REPORT-ONE") && mentor_log.contains("REPORT-TWO"),
        "the mentor gets every report back"
    );
    assert!(
        !mentor_log.contains("Carry out the mentor's plan"),
        "the mentor never sees the executor's handoffs"
    );
    assert!(
        !executor_log.contains("do the thing"),
        "the task itself never goes to the executor"
    );
    assert!(
        !executor_log.contains("The executor reports back"),
        "the executor never sees the mentor's review prompts"
    );
    let plan = executor_log.find("PLAN-ONE").unwrap();
    let report = executor_log.find("REPORT-ONE").unwrap();
    let feedback = executor_log.find("FEEDBACK-TWO").unwrap();
    assert!(
        plan < report && report < feedback,
        "the executor's own work stays in its history across rounds"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_hundredth_round_without_the_mark_ends_the_pair() {
    let mentor_url = stub_replies(vec![sse("the plan", "stop"), sse("not yet", "stop")]).await;
    let executor_url = stub_replies(vec![sse("did it", "stop")]).await;
    let mut agent = parent("budget");
    agent
        .pair_begin(
            &config(),
            test_client_at("mentor-test", &mentor_url),
            test_client_at("executor-test", &executor_url),
            "do the thing",
            vec![],
        )
        .unwrap();
    agent.pair.as_mut().unwrap().rounds = MAX_ROUNDS - 1;
    agent.pair_drive().await.unwrap();
    assert_eq!(agent.pair_outcome(), Some(Outcome::Budget));
    assert!(
        agent.pair_status().unwrap().contains("100 rounds"),
        "{}",
        agent.pair_status().unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_stop_before_the_drive_runs_nothing() {
    let mut agent = parent("stop");
    agent
        .pair_begin(
            &config(),
            test_client_at("mentor-test", ""),
            test_client_at("executor-test", ""),
            "do the thing",
            vec![],
        )
        .unwrap();
    agent.pair_ctl().unwrap().stop.set(true);
    agent.pair_drive().await.unwrap();
    assert_eq!(agent.pair_outcome(), Some(Outcome::Stopped));
    let split = agent.pair_split().unwrap();
    assert_eq!((split[0].requests, split[1].requests), (0, 0));
}

#[tokio::test(flavor = "current_thread")]
async fn an_empty_turn_fails_the_drive_and_keeps_the_pair() {
    let mentor_url = stub_replies(vec![sse("", "stop")]).await;
    let mut agent = parent("empty");
    agent
        .pair_begin(
            &config(),
            test_client_at("mentor-test", &mentor_url),
            test_client_at("executor-test", ""),
            "do the thing",
            vec![],
        )
        .unwrap();
    let err = agent.pair_drive().await.unwrap_err();
    assert!(err.to_string().contains("no reply"), "{err:#}");
    assert!(agent.pair_active());
}

#[tokio::test(flavor = "current_thread")]
async fn a_failed_turn_stays_queued_and_resume_retries_it() {
    let mentor_url = stub_replies_full(vec![
        sse("", "stop"),
        sse("the plan", "stop"),
        sse("good work\\nTASK_COMPLETE", "stop"),
    ])
    .await;
    let executor_url = stub_replies(vec![sse("did it", "stop")]).await;
    let mut agent = parent("retry");
    agent
        .pair_begin(
            &config(),
            test_client_at("mentor-test", &mentor_url),
            test_client_at("executor-test", &executor_url),
            "do the thing",
            vec![],
        )
        .unwrap();
    let err = agent.pair_drive().await.unwrap_err();
    assert!(err.to_string().contains("no reply"), "{err:#}");
    assert!(agent.pair_active());
    assert!(
        matches!(
            agent.pair.as_ref().unwrap().pending.front(),
            Some(Step::MentorPlan(..))
        ),
        "the failed turn stays queued for resume"
    );
    agent.pair_drive().await.unwrap();
    assert_eq!(agent.pair_outcome(), Some(Outcome::Done));
}

#[tokio::test(flavor = "current_thread")]
async fn an_interrupted_plan_stays_queued_and_resume_retries_it() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // The mentor's first request is held until the test drops the drive; later requests get
    // the scripted replies.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mentor_url = format!("http://{}", listener.local_addr().unwrap());
    let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let replies = [
        sse("the plan", "stop"),
        sse("good work\\nTASK_COMPLETE", "stop"),
    ];
    tokio::spawn(async move {
        async fn read_request(socket: &mut tokio::net::TcpStream) {
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
                        return;
                    }
                }
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                }
            }
        }
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        let _ = arrived_tx.send(());
        let _ = release_rx.await;
        let _ = socket.shutdown().await;
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            let body = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{reply}"
            );
            let _ = socket.write_all(body.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });

    let executor_url = stub_replies(vec![sse("did it", "stop")]).await;
    let mut agent = parent("interrupt-resume");
    agent
        .pair_begin(
            &config(),
            test_client_at("mentor-test", &mentor_url),
            test_client_at("executor-test", &executor_url),
            "do the thing",
            vec![],
        )
        .unwrap();

    let mut drive = Box::pin(agent.pair_drive());
    tokio::select! {
        _ = &mut drive => panic!("the drive ended before the interrupt"),
        _ = arrived_rx => {}
    }
    drop(drive);
    agent.pair_recover().unwrap();
    assert!(
        matches!(
            agent.pair.as_ref().unwrap().pending.front(),
            Some(Step::MentorPlan(..))
        ),
        "the interrupted plan stays queued for resume"
    );
    let _ = release_tx.send(());
    agent.pair_drive().await.unwrap();
    assert_eq!(agent.pair_outcome(), Some(Outcome::Done));
}

#[tokio::test(flavor = "current_thread")]
async fn the_budget_holds_across_an_interjection() {
    let mentor_url = stub_replies(vec![sse("replan", "stop")]).await;
    let executor_url = stub_replies(vec![sse("did it", "stop")]).await;
    let mut agent = parent("budget-interject");
    agent
        .pair_begin(
            &config(),
            test_client_at("mentor-test", &mentor_url),
            test_client_at("executor-test", &executor_url),
            "do the thing",
            vec![],
        )
        .unwrap();
    // The budget is spent and only an executor step stands (what an interjection at the
    // boundary would leave): the pair ends without running a hundred-and-first round.
    let pair = agent.pair.as_mut().unwrap();
    pair.rounds = MAX_ROUNDS;
    pair.pending.clear();
    pair.pending.push_back(Step::Executor("replan".to_string()));
    agent.pair_drive().await.unwrap();
    assert_eq!(agent.pair_outcome(), Some(Outcome::Budget));
    let split = agent.pair_split().unwrap();
    assert_eq!(
        split[1].requests, 0,
        "the executor never runs past the budget"
    );
}

#[test]
fn the_status_counts_the_round_in_flight() {
    let mut agent = parent("status");
    agent
        .pair_begin(
            &config(),
            test_client_at("mentor-test", ""),
            test_client_at("executor-test", ""),
            "do the thing",
            vec![],
        )
        .unwrap();
    let status = agent.pair_status().unwrap();
    assert!(status.contains("1 round"), "{status}");
    assert!(status.contains("starting"), "{status}");
}

#[test]
fn a_session_that_only_ran_a_pair_keeps_its_log() {
    let mut agent = parent("keep");
    agent
        .pair_begin(
            &config(),
            test_client_at("mentor-test", ""),
            test_client_at("executor-test", ""),
            "do the thing",
            vec![],
        )
        .unwrap();
    agent.discard_if_unused();
    assert!(
        agent.session.log_path.exists(),
        "the pair's note points at its children's logs"
    );
}

#[test]
fn an_interjection_replaces_the_queue_with_a_replan() {
    let mut agent = parent("interject");
    agent
        .pair_begin(
            &config(),
            test_client_at("mentor-test", ""),
            test_client_at("executor-test", ""),
            "do the thing",
            vec![],
        )
        .unwrap();
    agent
        .pair_ctl()
        .unwrap()
        .interject
        .borrow_mut()
        .push_back(("go left".to_string(), vec![]));
    assert!(agent.pair_interject());
    let pair = agent.pair.as_ref().unwrap();
    assert_eq!(pair.pending.len(), 1);
    assert!(
        matches!(&pair.pending[0], Step::MentorPlan(text, _) if text.contains("go left")),
        "the mentor plans again from its full context"
    );
    assert!(!agent.pair_interject(), "the queue is drained");
}

#[test]
fn the_mentor_is_plan_locked_and_the_executor_keeps_subagents() {
    let mut agent = parent("roles");
    agent
        .pair_begin(
            &config(),
            test_client_at("mentor-test", ""),
            test_client_at("executor-test", ""),
            "do the thing",
            vec![],
        )
        .unwrap();
    let pair = agent.pair.as_ref().unwrap();
    let mentor = pair.mentor.as_ref().unwrap();
    let executor = pair.executor.as_ref().unwrap();
    assert!(mentor.controls.plan.get());
    assert!(!executor.controls.plan.get());
    assert!(!mentor.pair_exec);
    assert!(executor.pair_exec);
    assert_eq!((mentor.depth, executor.depth), (1, 1));
}
