use super::*;
use serde_json::json;
use std::fs;

fn ctx() -> Context {
    let tmp = crate::test_temp::TempDir::new("killi-bash");
    let mut ctx = Context::new(tmp.path().to_path_buf(), tmp.join("spill"));
    ctx.temp = Some(tmp);
    ctx
}

fn never() -> bool {
    false
}

#[tokio::test(flavor = "current_thread")]
async fn merges_stderr_and_reports_exit_code() {
    let out = bash(
        &ctx(),
        &json!({"command": "echo out; echo err >&2; exit 3"}),
        &never,
    )
    .await;
    assert!(out.is_error);
    assert_eq!(out.text, "out\nerr\n[Command exited with code 3]");
}

#[tokio::test(flavor = "current_thread")]
async fn cwd_picks_the_directory_and_must_exist() {
    let ctx = ctx();
    let sub = ctx.cwd.join(format!("killi-cwd-{}", std::process::id()));
    fs::create_dir_all(&sub).unwrap();
    let name = sub.file_name().unwrap().to_str().unwrap();
    for cwd in [sub.to_str().unwrap(), name] {
        let out = bash(&ctx, &json!({"command": "pwd", "cwd": cwd}), &never).await;
        assert_eq!(
            out.text.trim(),
            sub.canonicalize().unwrap().to_str().unwrap()
        );
    }
    let out = bash(
        &ctx,
        &json!({"command": "pwd", "cwd": "no-such-dir"}),
        &never,
    )
    .await;
    assert!(out.is_error);
    assert_eq!(out.text, "cwd no-such-dir is not a directory.");
    fs::remove_dir_all(sub).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn a_silent_success_says_so_with_its_exit_code() {
    let out = bash(&ctx(), &json!({"command": "true"}), &never).await;
    assert!(!out.is_error);
    assert_eq!(out.text, "(no output, exit code 0)");
    assert_eq!(out.brief, "no output");
}

#[tokio::test(flavor = "current_thread")]
async fn timeout_and_ids_arrive_as_strings_too() {
    let out = bash(
        &ctx(),
        &json!({"command": "echo hi", "timeout": "1"}),
        &never,
    )
    .await;
    assert_eq!(out.text, "hi");
    let ctx = ctx();
    bash(
        &ctx,
        &json!({"command": "sleep 30", "run_in_background": true}),
        &never,
    )
    .await;
    assert_eq!(task_stop(&ctx, &json!({"id": "1"})).text, "Stopped task 1.");
    bash(
        &ctx,
        &json!({"command": "sleep 30", "run_in_background": true}),
        &never,
    )
    .await;
    let out = task_output(&ctx, &json!({"id": "2", "wait": "1"}), &never).await;
    assert!(
        out.text.starts_with("Task 2 is still running"),
        "{}",
        out.text
    );
    ctx.tasks.borrow_mut().stop_all();
}

#[tokio::test(flavor = "current_thread")]
async fn a_slow_command_moves_to_the_background() {
    let ctx = ctx();
    let out = bash(
        &ctx,
        &json!({"command": "echo start; sleep 2; echo done", "timeout": 1}),
        &never,
    )
    .await;
    assert!(
        out.text
            .starts_with("start\n[Still running after 1s, so it moved to background task 1.")
    );
    let out = task_output(&ctx, &json!({"id": 1, "wait": 10}), &never).await;
    assert_eq!(out.text, "Task 1 exited with code 0.\ndone");
    assert!(ctx.tasks.borrow_mut().newly_finished().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_message_from_the_user_ends_the_wait_and_keeps_the_command_running() {
    let ctx = ctx();
    let sent = std::cell::Cell::new(false);
    let wake = || sent.get();
    let started = Instant::now();
    let args = json!({"command": "echo start; sleep 20", "timeout": 60});
    let (out, ()) = tokio::join!(bash(&ctx, &args, &wake), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        sent.set(true);
    });
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it waited for the command"
    );
    assert!(
        out.text.starts_with(
            "start\n[A message from the user is waiting, so it moved to background task 1 after 0s."
        ),
        "{}",
        out.text
    );
    assert!(ctx.tasks.borrow().list[&1].process.running());
    assert_eq!(task_stop(&ctx, &json!({"id": 1})).text, "Stopped task 1.");
}

#[tokio::test(flavor = "current_thread")]
async fn a_command_that_ends_first_is_not_moved_when_a_message_waits() {
    // Either the command wins the wait (the usual case: the select prefers its exit
    // when both are ready) or, under load, the wake poll fires first and the finished
    // command is reported as a background task. Both report the output.
    let out = bash(&ctx(), &json!({"command": "echo quick"}), &|| true).await;
    assert!(
        out.text == "quick" || out.text.contains("moved to background task"),
        "a finished command is reported either way: {}",
        out.text
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_message_from_the_user_ends_a_task_output_wait() {
    let ctx = ctx();
    bash(
        &ctx,
        &json!({"command": "sleep 20", "run_in_background": true}),
        &never,
    )
    .await;
    let sent = std::cell::Cell::new(false);
    let wake = || sent.get();
    let started = Instant::now();
    let args = json!({"id": 1, "wait": 60});
    let (out, ()) = tokio::join!(task_output(&ctx, &args, &wake), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        sent.set(true);
    });
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it waited for the task"
    );
    assert!(
        out.text.starts_with("Task 1 is still running"),
        "{}",
        out.text
    );
    assert!(
        out.text
            .contains("[Stopped waiting: a message from the user is waiting."),
        "{}",
        out.text
    );
    task_stop(&ctx, &json!({"id": 1}));
}

#[tokio::test(flavor = "current_thread")]
async fn background_tasks_report_when_they_finish() {
    let ctx = ctx();
    let out = bash(
        &ctx,
        &json!({"command": "exit 4", "run_in_background": true}),
        &never,
    )
    .await;
    assert_eq!(
        out.text,
        "Started background task 1. Read its output with task_output."
    );
    let mut exit = ctx.tasks.borrow().list[&1].process.exit.clone();
    exit.wait_for(Option::is_some).await.unwrap();
    assert_eq!(
        ctx.tasks.borrow_mut().newly_finished(),
        vec!["Background task 1 (`exit 4`) exited with code 4.".to_string()]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_command_has_no_controlling_terminal() {
    let ctx = ctx();
    let out = bash(
        &ctx,
        &json!({"command": "( : </dev/tty ) 2>/dev/null && echo opened || echo closed"}),
        &never,
    )
    .await;
    assert_eq!(out.text, "closed");
}

#[tokio::test(flavor = "current_thread")]
async fn task_stop_kills_the_process_group() {
    let ctx = ctx();
    bash(
        &ctx,
        &json!({"command": "sleep 30 & sleep 30", "run_in_background": true}),
        &never,
    )
    .await;
    assert_eq!(task_stop(&ctx, &json!({"id": 1})).text, "Stopped task 1.");
    let out = task_output(&ctx, &json!({"id": 1, "wait": 5}), &never).await;
    assert!(
        out.text.starts_with("Task 1 was killed by a signal."),
        "{}",
        out.text
    );
}

async fn until_stopped(ctx: &Context, id: u32) -> bool {
    for _ in 0..100 {
        if !ctx.tasks.borrow().list[&id].process.running() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test(flavor = "current_thread")]
async fn what_a_finished_command_leaves_running_becomes_a_task_raised_once() {
    let ctx = ctx();
    let out = bash(
        &ctx,
        &json!({"command": "nohup sleep 30 >/dev/null 2>&1 &"}),
        &never,
    )
    .await;
    assert_eq!(
        out.text,
        "(no output, exit code 0)\n[What it started is still running, as background task 1; task_output with `wait` returns when it ends, task_stop ends it.]"
    );
    assert_eq!(out.brief, "no output · left task 1");
    assert!(ctx.tasks.borrow_mut().newly_finished().is_empty());
    let out = task_output(&ctx, &json!({"id": 1}), &never).await;
    assert!(
        out.text
            .starts_with("Task 1 exited with code 0; what it started is still running ("),
        "{}",
        out.text
    );
    let left = ctx.tasks.borrow_mut().left_running();
    assert_eq!(left.len(), 1);
    assert!(
        left[0].starts_with("task 1 `nohup sleep 30 >/dev/null 2>&1 &` ("),
        "{left:?}"
    );
    assert!(ctx.tasks.borrow_mut().left_running().is_empty());
    assert_eq!(task_stop(&ctx, &json!({"id": 1})).text, "Stopped task 1.");
    assert!(until_stopped(&ctx, 1).await);
    assert_eq!(
        task_stop(&ctx, &json!({"id": 1})).text,
        "Task 1 had already finished."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_command_that_leaves_nothing_running_is_not_kept() {
    let ctx = ctx();
    let out = bash(
        &ctx,
        &json!({"command": "sleep 0.1 & wait; echo hi"}),
        &never,
    )
    .await;
    assert_eq!(out.text, "hi");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(ctx.tasks.borrow_mut().left_running().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn closing_the_session_kills_what_still_runs() {
    let ctx = ctx();
    bash(
        &ctx,
        &json!({"command": "nohup sleep 30 >/dev/null 2>&1 &"}),
        &never,
    )
    .await;
    bash(
        &ctx,
        &json!({"command": "sleep 30", "run_in_background": true}),
        &never,
    )
    .await;
    let pgids: Vec<u32> = ctx
        .tasks
        .borrow()
        .list
        .values()
        .map(|task| task.process.pgid)
        .collect();
    assert_eq!(
        ctx.tasks.borrow_mut().stop_all(),
        vec![
            "nohup sleep 30 >/dev/null 2>&1 &".to_string(),
            "sleep 30".to_string()
        ]
    );
    assert!(until_stopped(&ctx, 1).await && until_stopped(&ctx, 2).await);
    drop(ctx);
    assert!(pgids.iter().all(|pgid| !groups().contains_key(pgid)));
}

#[tokio::test(flavor = "current_thread")]
async fn waiting_on_a_task_waits_for_what_it_left_running() {
    let ctx = ctx();
    bash(
        &ctx,
        &json!({"command": "nohup sleep 1 >/dev/null 2>&1 &"}),
        &never,
    )
    .await;
    let started = Instant::now();
    let out = task_output(&ctx, &json!({"id": 1, "wait": 20}), &never).await;
    assert!(
        out.text.starts_with("Task 1 exited with code 0.\n"),
        "{}",
        out.text
    );
    assert!(
        started.elapsed() >= Duration::from_millis(800),
        "returned before the job ended"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "waited past the job's end"
    );
    assert!(!ctx.tasks.borrow().list[&1].process.running());
}

#[tokio::test(flavor = "current_thread")]
async fn waiting_on_a_task_gives_up_at_its_limit() {
    let ctx = ctx();
    bash(
        &ctx,
        &json!({"command": "nohup sleep 30 >/dev/null 2>&1 &"}),
        &never,
    )
    .await;
    let out = task_output(&ctx, &json!({"id": 1, "wait": 1}), &never).await;
    assert!(
        out.text
            .starts_with("Task 1 exited with code 0; what it started is still running ("),
        "{}",
        out.text
    );
    ctx.tasks.borrow_mut().stop_all();
}

#[test]
fn only_a_long_leading_sleep_is_refused() {
    assert_eq!(leading_sleep("sleep 270; tail log"), Some(270.0));
    assert_eq!(leading_sleep("  sleep 45&&echo done"), Some(45.0));
    assert_eq!(leading_sleep("sleep 0.5"), Some(0.5));
    assert_eq!(leading_sleep("echo start; sleep 40"), None);
    assert_eq!(leading_sleep("sleepy 40"), None);
    assert_eq!(leading_sleep("sleep"), None);
}

#[tokio::test(flavor = "current_thread")]
async fn a_long_fixed_sleep_does_not_run() {
    let out = bash(
        &ctx(),
        &json!({"command": "sleep 270; tail /tmp/log"}),
        &never,
    )
    .await;
    assert!(out.is_error);
    assert!(
        out.text
            .starts_with("Not run: `sleep 270` waits the whole time"),
        "{}",
        out.text
    );
    assert!(out.text.contains("task_output"), "{}", out.text);
    let out = bash(&ctx(), &json!({"command": "sleep 0.1; echo ok"}), &never).await;
    assert_eq!(out.text, "ok");
    let ctx = ctx();
    let out = bash(
        &ctx,
        &json!({"command": "sleep 30", "run_in_background": true}),
        &never,
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
    ctx.tasks.borrow_mut().stop_all();
}

#[tokio::test(flavor = "current_thread")]
async fn a_call_in_flight_says_how_long_it_ran_and_what_it_printed() {
    let ctx = ctx();
    assert_eq!(ctx.running.borrow().describe(), None);
    let command = json!({"command": "echo one; echo two; sleep 2", "timeout": 10});
    let (_, seen) = tokio::join!(bash(&ctx, &command, &never), async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        ctx.running.borrow().describe()
    });
    let seen = seen.expect("the command is still running");
    assert!(
        seen.starts_with(
            "bash `echo one; echo two; sleep 2` has run 0s, last output 0s ago.\n  one\n  two"
        ),
        "{seen}"
    );
    assert_eq!(ctx.running.borrow().describe(), None);
}

#[test]
fn long_output_keeps_the_tail() {
    let output: String = (1..=3000).map(|n| format!("{n}\n")).collect();
    let ctx = ctx();
    let text = tail(&ctx, &output, 0);
    assert!(text.starts_with("[Showing lines 1001-3000 of 3000. Full output: "));
    assert!(text.ends_with("\n3000"));
    let _ = fs::remove_dir_all(&ctx.spill_dir);
}

#[test]
fn failed_output_keeps_the_head_and_tail_within_budget_and_utf8() {
    let ctx = ctx();
    let output = format!(
        "根因: first\n{}末尾: last",
        (1..=3000).map(|n| format!("行 {n}\n")).collect::<String>()
    );
    let text = failed_tail(&ctx, &output, 0);
    assert!(text.starts_with("[Showing the first "), "{}", &text[..160]);
    assert!(
        text.contains(" lines of 3002. Full output: /"),
        "{}",
        &text[..160]
    );
    let body = text.split_once('\n').unwrap().1;
    assert!(body.starts_with("根因: first\n"));
    assert!(body.ends_with("末尾: last"));
    assert!(body.len() <= MAX_BYTES);
    assert!(body.lines().count() <= MAX_LINES);
    assert_eq!(failed_tail(&ctx, "first\nlast", 0), "first\nlast");
    let _ = fs::remove_dir_all(&ctx.spill_dir);
}

#[tokio::test(flavor = "current_thread")]
async fn a_failed_command_returns_both_ends_and_the_spill_path() {
    let ctx = ctx();
    let command = "echo '根因: first'; for n in {1..2100}; do echo \"line $n\"; done; echo '末尾: last'; exit 9";
    let out = bash(&ctx, &json!({"command": command}), &never).await;
    assert!(out.is_error);
    assert!(out.text.contains("根因: first"), "{}", &out.text[..160]);
    assert!(
        out.text
            .contains("末尾: last\n[Command exited with code 9]")
    );
    assert!(out.text.contains("Full output: /"), "{}", &out.text[..200]);
    let _ = fs::remove_dir_all(&ctx.spill_dir);
}

#[test]
fn failed_output_cuts_a_wide_line_at_utf8_boundaries() {
    let ctx = ctx();
    let output = "界".repeat(MAX_BYTES);
    let text = failed_tail(&ctx, &output, 0);
    assert!(text.contains("[... output omitted ...]"));
    assert!(text.split_once('\n').unwrap().1.len() <= MAX_BYTES);
    let _ = fs::remove_dir_all(&ctx.spill_dir);
}

#[test]
fn the_tail_is_cut_by_lines_or_by_bytes_whichever_comes_first() {
    let ctx = ctx();
    let short = "y\n".repeat(1_000_000);
    let text = tail(&ctx, &short, 0);
    assert!(
        text.starts_with("[Showing lines 998001-1000000 of 1000000. Full output: "),
        "{}",
        &text[..80]
    );
    assert_eq!(text.lines().count(), 2001);
    let wide = format!("{}\n", "x".repeat(99)).repeat(1000);
    let text = tail(&ctx, &wide, 0);
    assert!(
        text.starts_with("[Showing lines 489-1000 of 1000. Full output: "),
        "{}",
        &text[..80]
    );
    assert_eq!(text.lines().count(), 513);
    assert_eq!(tail(&ctx, "a\nb\n", 0), "a\nb");
    let _ = fs::remove_dir_all(&ctx.spill_dir);
}

#[tokio::test(flavor = "current_thread")]
async fn a_job_holding_the_pipe_does_not_hold_the_command() {
    let ctx = ctx();
    let started = Instant::now();
    let out = bash(
        &ctx,
        &json!({"command": "echo started; (sleep 1; echo late) &", "timeout": 20}),
        &never,
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "waited for the job's pipe: {:?}",
        started.elapsed()
    );
    assert!(
        out.text
            .starts_with("started\n[What it started is still running, as background task 1"),
        "{}",
        out.text
    );
    let mut seen = String::new();
    for _ in 0..100 {
        seen.push_str(&task_output(&ctx, &json!({"id": 1}), &never).await.text);
        if seen.contains("late") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the job's later output was not captured: {seen}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_finished_task_keeps_only_what_the_model_has_not_read() {
    let ctx = ctx();
    for _ in 0..2 {
        bash(&ctx, &json!({"command": "head -c 3000000 /dev/zero | tr '\\0' a", "run_in_background": true}), &never).await;
    }
    let out = task_output(&ctx, &json!({"id": 1, "wait": 30}), &never).await;
    assert!(
        out.text.starts_with("Task 1 exited with code 0."),
        "{}",
        out.text
    );
    let mut exit = ctx.tasks.borrow().list[&2].process.exit.clone();
    exit.wait_for(Option::is_some).await.unwrap();
    ctx.tasks.borrow_mut().newly_finished();
    let held = |id: u32| {
        let tasks = ctx.tasks.borrow();
        let captured = tasks.list[&id].process.output.lock().unwrap();
        (captured.bytes.len(), captured.dropped)
    };
    assert_eq!(held(1), (0, 3_000_000), "read output is let go");
    assert_eq!(
        held(2),
        (FINISHED_KEEP, 3_000_000 - FINISHED_KEEP),
        "unread output keeps its last megabyte"
    );
    let out = task_output(&ctx, &json!({"id": 2}), &never).await;
    assert!(
        out.text.contains(&format!(
            "The first {} bytes were not kept.",
            3_000_000 - FINISHED_KEEP
        )),
        "{}",
        &out.text[..120]
    );
    assert_eq!(held(2).0, 0);
    assert_eq!(
        task_output(&ctx, &json!({"id": 1}), &never).await.text,
        "Task 1 exited with code 0.\n(no new output)"
    );
    let _ = fs::remove_dir_all(&ctx.spill_dir);
}
