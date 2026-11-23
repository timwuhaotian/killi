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
