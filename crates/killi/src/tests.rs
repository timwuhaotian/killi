use super::*;

#[test]
fn system_prompt_and_tools_fit_the_token_budget() {
    let base = crate::prompt::DEFAULT;
    let mut tools = crate::tools::definitions();
    tools.push(crate::skills::definition());
    let tools = serde_json::to_string(&tools).unwrap();
    let chars = base.len() + tools.len();
    println!("system prompt + tools = {chars} chars");
    assert!(
        chars <= 12_000,
        "system prompt + tools = {chars} chars, over the ~3k-token budget"
    );
}

fn listed(path: PathBuf) -> session::Listed {
    session::Listed {
        path,
        modified: std::time::SystemTime::now(),
        first_prompt: String::new(),
        prompts: 1,
    }
}

#[test]
fn a_locked_log_counts_as_running_and_is_skipped_by_continue() {
    let dir = crate::test_temp::TempDir::new("killi-continue");
    let (old, new) = (dir.join("1-1.jsonl"), dir.join("2-2.jsonl"));
    std::fs::write(&old, "x").unwrap();
    std::fs::write(&new, "x").unwrap();
    assert!(!session::is_locked(&old));
    assert!(!session::is_locked(&dir.join("missing.jsonl")));
    assert_eq!(
        newest_available(vec![listed(new.clone()), listed(old.clone())]),
        Some(new.clone())
    );
    let held = session::Session::open(&new).unwrap();
    assert!(
        session::is_locked(&new),
        "the log is locked while its session runs"
    );
    assert_eq!(
        newest_available(vec![listed(new.clone()), listed(old.clone())]),
        Some(old.clone())
    );
    drop(held);
    crate::test_temp::await_unlocked(&new);
    assert_eq!(
        newest_available(vec![listed(new.clone())]),
        Some(new.clone())
    );
    assert_eq!(newest_available(Vec::new()), None);
}

#[test]
fn a_positional_prompt_starts_an_interactive_session_with_that_first_turn() {
    let options = parse(vec!["fix the bug".to_string()]).unwrap();
    assert_eq!(options.initial_prompt.as_deref(), Some("fix the bug"));
    assert_eq!(options.prompt, None);
    let options = parse(vec![
        "-m".to_string(),
        "openai/gpt".to_string(),
        "part".to_string(),
        "one".to_string(),
    ])
    .unwrap();
    assert_eq!(options.initial_prompt.as_deref(), Some("part one"));
    assert_eq!(options.model.as_deref(), Some("openai/gpt"));
    let options = parse(vec!["--prompt".to_string(), "hello".to_string()]).unwrap();
    assert_eq!(options.initial_prompt.as_deref(), Some("hello"));
}

#[test]
fn a_one_shot_and_an_interactive_prompt_are_not_both_accepted() {
    assert!(
        parse(vec![
            "-p".to_string(),
            "once".to_string(),
            "interactive".to_string()
        ])
        .is_err()
    );
}

/// A stream a test reads back.
#[derive(Clone, Default)]
struct Captured(Rc<RefCell<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8(self.0.borrow().clone()).unwrap()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A stream whose reader is gone.
struct Gone;

impl std::io::Write for Gone {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
}

/// Runs `events` through a printer and returns its stdout and stderr.
fn printed(events: Vec<agent::Event>) -> (String, String) {
    let (out, err) = (Captured::default(), Captured::default());
    let mut printer = Printer::to(
        std::path::Path::new("/work"),
        Box::new(out.clone()),
        Box::new(err.clone()),
        false,
    );
    for event in events {
        printer.event(event);
    }
    printer.finish();
    (out.text(), err.text())
}

fn tool_start(id: &str, summary: &str) -> agent::Event {
    agent::Event::ToolStart {
        id: id.into(),
        summary: summary.into(),
    }
}

fn tool_end(id: &str, is_error: bool) -> agent::Event {
    agent::Event::ToolEnd {
        id: id.into(),
        is_error,
        brief: String::new(),
        display: None,
        output: String::new(),
    }
}

#[test]
fn a_backup_after_partial_text_prints_only_the_backup_reply() {
    use agent::Event;
    let (out, err) = printed(vec![
        Event::Text("The primary began".into()),
        Event::Text(" and was cut".into()),
        Event::Discard {
            reason: "the backup model answers instead".into(),
        },
        Event::Notice("primary is rate-limited, so this session switched to its backup.".into()),
        Event::Text("The backup's answer.".into()),
    ]);
    assert_eq!(out, "The backup's answer.\n");
    assert!(err.contains("the partial reply was discarded"), "{err}");
    assert!(err.contains("switched to its backup"), "{err}");
}

#[test]
fn a_failed_request_prints_none_of_its_partial_reply() {
    use agent::Event;
    let (out, err) = printed(vec![
        Event::Text("Half an answ".into()),
        Event::Discard {
            reason: "the attempt failed".into(),
        },
    ]);
    assert_eq!(out, "", "stdout carries only a reply that finished");
    assert!(err.contains("discarded"), "{err}");
    let (_, err) = printed(vec![
        Event::Reasoning("thinking".into()),
        Event::Discard {
            reason: "the attempt failed".into(),
        },
    ]);
    assert_eq!(err, "", "nothing was held back, so nothing was discarded");
}

#[test]
fn a_retried_attempt_after_a_tool_keeps_the_separator() {
    use agent::Event;
    let before = || {
        vec![
            Event::Text("First.".into()),
            tool_start("1", "read a"),
            tool_end("1", false),
        ]
    };
    let mut retried = before();
    retried.extend([
        Event::Text("Cut".into()),
        Event::Retry {
            attempt: 1,
            wait: std::time::Duration::ZERO,
            reason: "reset".into(),
        },
        Event::Text("\n\nAfter.".into()),
    ]);
    let mut straight = before();
    straight.push(Event::Text("\n\nAfter.".into()));
    let (out, err) = printed(retried);
    assert_eq!(out, printed(straight).0);
    assert!(
        out.ends_with("\nAfter.\n") && !out.contains("Cut"),
        "{out:?}"
    );
    assert!(
        err.contains("the partial reply was discarded; retrying"),
        "{err}"
    );
}

#[test]
fn a_retried_continuation_keeps_the_part_already_recorded() {
    use agent::Event;
    let retry = || Event::Retry {
        attempt: 1,
        wait: std::time::Duration::ZERO,
        reason: "reset".into(),
    };
    let (out, err) = printed(vec![
        Event::Text("The first part, cut off mid-".into()),
        Event::Settled,
        Event::Text("sentence and lo".into()),
        retry(),
        Event::Text("sentence and the rest.\n".into()),
    ]);
    assert_eq!(out, "The first part, cut off mid-sentence and the rest.\n");
    assert!(
        err.contains("the partial reply was discarded; retrying"),
        "{err}"
    );
    let (out, err) = printed(vec![
        Event::Text("Para one.\n\n".into()),
        Event::Settled,
        retry(),
        Event::Text("Para two.".into()),
    ]);
    assert_eq!(out, "Para one.\n\nPara two.\n", "held newlines survive");
    assert!(!err.contains("discarded"), "nothing streamed since: {err}");
}

#[test]
fn parallel_calls_get_a_line_each_and_their_own_marks() {
    let (_, err) = printed(vec![
        tool_start("1", "read a"),
        tool_start("2", "read b"),
        tool_end("1", true),
        tool_end("2", false),
    ]);
    assert_eq!(err, "● read a\n● read b\n✗ read a\n");
    let (_, err) = printed(vec![
        tool_start("1", "read a"),
        tool_start("2", "read b"),
        tool_end("2", true),
        tool_end("1", false),
    ]);
    assert_eq!(err, "● read a\n● read b ✗\n");
}

#[test]
fn a_notice_or_a_subagent_retry_starts_its_own_line() {
    use agent::Event;
    let (_, err) = printed(vec![
        tool_start("1", "bash ls"),
        Event::Notice("PreToolUse hook failed".into()),
        tool_end("1", false),
    ]);
    assert_eq!(err, "● bash ls\nPreToolUse hook failed\n");
    let (_, err) = printed(vec![
        tool_start("1", "agent look"),
        Event::Sub {
            label: "look".into(),
            slot: None,
            event: Box::new(Event::Retry {
                attempt: 1,
                wait: std::time::Duration::from_secs(2),
                reason: "429".into(),
            }),
        },
        tool_end("1", false),
    ]);
    assert_eq!(err, "● agent look\n  ↳ look: retry 1 in 2.0s: 429\n");
}

#[test]
fn a_closed_stdout_or_stderr_ends_the_output_without_a_panic() {
    use agent::Event;
    let halt = Rc::new(tokio::sync::Notify::new());
    let mut printer = Printer::to(
        std::path::Path::new("/work"),
        Box::new(Gone),
        Box::new(Gone),
        false,
    );
    printer.halt_on(halt.clone());
    printer.event(Event::Text("an answer".into()));
    assert!(!printer.closed(), "nothing was written yet");
    printer.event(tool_start("1", "read a"));
    assert!(printer.closed());
    printer.event(Event::Text("more".into()));
    printer.event(Event::Notice("a note".into()));
    printer.finish();
    let notified = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(1), halt.notified())
                .await
                .is_ok()
        });
    assert!(notified, "the run is told to end");
}

#[test]
fn a_signal_record_is_one_stamped_json_line() {
    let line = crash::signal_line("SIGTERM", 9);
    assert!(line.ends_with("}\n") && line.matches('\n').count() == 1);
    let back: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(back["type"], "signal");
    assert_eq!(back["signal"], "SIGTERM");
    assert_eq!(back["ts"], 9);
}
