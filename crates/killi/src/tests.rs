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
    // Another test forking at this moment can pin the lock past the drop, so the
    // release is awaited instead of assumed instant; a leaked lock still fails.
    let mut unlocked = false;
    for _ in 0..100 {
        if !session::is_locked(&new) {
            unlocked = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(unlocked, "the lock goes when the session ends");
    assert_eq!(
        newest_available(vec![listed(new.clone())]),
        Some(new.clone())
    );
    assert_eq!(newest_available(Vec::new()), None);
}
