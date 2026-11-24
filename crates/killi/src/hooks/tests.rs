use super::*;

fn config(toml_text: &str) -> HooksConfig {
    #[derive(Deserialize)]
    struct Wrapper {
        hooks: HooksConfig,
    }
    toml::from_str::<Wrapper>(toml_text).unwrap().hooks
}

#[tokio::test(flavor = "current_thread")]
async fn exit_code_two_blocks_with_stderr() {
    let hooks = config(
        "[[hooks.PreToolUse]]\nmatcher = \"bash\"\ncommand = \"grep -q 'rm -rf' && echo 'no rm -rf' >&2 && exit 2; exit 0\"\n",
    );
    let cwd = std::env::temp_dir();
    let blocked = hooks
        .run(
            "PreToolUse",
            Some("bash"),
            &cwd,
            "s",
            tool_payload("bash", &json!({"command": "rm -rf /"}), None),
        )
        .await;
    assert_eq!(blocked.block.as_deref(), Some("no rm -rf"));
    let allowed = hooks
        .run(
            "PreToolUse",
            Some("bash"),
            &cwd,
            "s",
            tool_payload("bash", &json!({"command": "ls"}), None),
        )
        .await;
    assert!(allowed.block.is_none());
    let other = hooks
        .run(
            "PreToolUse",
            Some("read"),
            &cwd,
            "s",
            tool_payload("read", &json!({}), None),
        )
        .await;
    assert!(other.block.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn a_hook_that_ignores_a_large_input_still_times_out_and_leaves_nothing_running() {
    let marker = std::env::temp_dir().join(format!("killi-hook-orphan-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let hooks = config(&format!(
        "[[hooks.PostToolUse]]\ncommand = \"(sleep 2; touch {}) & sleep 30\"\ntimeout = 1\n",
        marker.display()
    ));
    let big = "x".repeat(300_000);
    let cwd = std::env::temp_dir();
    let started = std::time::Instant::now();
    hooks
        .run(
            "PostToolUse",
            Some("bash"),
            &cwd,
            "s",
            tool_payload("bash", &json!({}), Some(&big)),
        )
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the timeout did not apply: {:?}",
        started.elapsed()
    );
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        !marker.exists(),
        "a process the hook started outlived its timeout"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_hook_that_backgrounds_a_child_returns_when_it_exits() {
    let hooks = config(concat!(
        "[[hooks.PreToolUse]]\nmatcher = \"bash\"\n",
        "command = \"echo '{\\\"decision\\\": \\\"block\\\", \\\"reason\\\": \\\"no\\\"}'; sleep 3 &\"\n",
        "[[hooks.PreToolUse]]\nmatcher = \"edit\"\n",
        "command = \"echo nope >&2; sleep 3 & exit 2\"\n",
        "[[hooks.Stop]]\ncommand = \"sleep 3 &\"\n",
    ));
    let cwd = std::env::temp_dir();
    for (event, tool, block) in [
        ("PreToolUse", Some("bash"), Some("no")),
        ("PreToolUse", Some("edit"), Some("nope")),
        ("Stop", None, None),
    ] {
        let started = std::time::Instant::now();
        let outcome = hooks
            .run(
                event,
                tool,
                &cwd,
                "s",
                tool_payload(tool.unwrap_or(""), &json!({}), None),
            )
            .await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{event} {tool:?} waited for its background child: {:?}",
            started.elapsed()
        );
        assert!(outcome.failures.is_empty(), "{event} {tool:?}");
        assert_eq!(outcome.block.as_deref(), block, "{event} {tool:?}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn hook_output_is_capped_and_cut_for_the_model() {
    let tmp = crate::test_temp::TempDir::new("killi-hook-cap");
    let flood = "yes x | head -c 1000000";
    let (code, out, err) = execute(
        &format!("{flood}; {flood} >&2"),
        tmp.path(),
        "",
        Duration::from_secs(20),
    )
    .await
    .expect("a chatty hook still finishes");
    assert_eq!(code, 0);
    assert_eq!((out.len(), err.len()), (OUTPUT_CAP, OUTPUT_CAP));
    let hooks = config(&format!(
        "[[hooks.UserPromptSubmit]]\ncommand = \"{flood}\"\n"
    ));
    let prompt = hooks
        .run("UserPromptSubmit", None, tmp.path(), "s", json!({}))
        .await;
    let context = &prompt.context[0];
    assert!(
        context.len() < MODEL_TEXT_BYTES + 100
            && context.contains(&format!("\n[hook output cut at {MODEL_TEXT_BYTES} of ")),
        "{}",
        &context[MODEL_TEXT_BYTES - 10..]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn continue_false_halts_a_stop_and_blocks_anything_else() {
    let hooks = config(concat!(
        "[[hooks.Stop]]\ncommand = \"echo '{\\\"continue\\\": false, \\\"stopReason\\\": \\\"all done\\\"}'\"\n",
        "[[hooks.PreToolUse]]\ncommand = \"echo '{\\\"continue\\\": false, \\\"stopReason\\\": \\\"not now\\\"}'\"\n",
    ));
    let cwd = std::env::temp_dir();
    let stop = hooks.run("Stop", None, &cwd, "s", json!({})).await;
    assert_eq!(stop.halt.as_deref(), Some("all done"));
    assert_eq!(stop.block, None, "a halt is not a request to keep going");
    let tool = hooks
        .run(
            "PreToolUse",
            Some("bash"),
            &cwd,
            "s",
            tool_payload("bash", &json!({}), None),
        )
        .await;
    assert_eq!(tool.block.as_deref(), Some("not now"));
}

#[tokio::test(flavor = "current_thread")]
async fn a_running_hook_is_tracked_so_kill_all_covers_it() {
    use crate::tools::registered;

    let tmp = crate::test_temp::TempDir::new("killi-hook-track");
    let mut child = spawn_hook("sleep 30", tmp.path()).unwrap();
    let pgid = child.id().unwrap();
    {
        let _group = Group::new(child.id());
        assert!(
            registered(pgid),
            "the hook's group is tracked while the hook runs"
        );
    }
    assert!(!registered(pgid), "a finished hook unregisters its group");
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[test]
fn tool_hooks_are_detected() {
    assert!(!config("[hooks]\n").has_tool_hooks());
    assert!(!config("[[hooks.SessionStart]]\ncommand = ':'\n").has_tool_hooks());
    assert!(config("[[hooks.PreToolUse]]\ncommand = ':'\n").has_tool_hooks());
    assert!(config("[[hooks.PostToolUse]]\ncommand = ':'\n").has_tool_hooks());
}

#[tokio::test(flavor = "current_thread")]
async fn no_hooks_env_runs_nothing() {
    let hooks = config("[[hooks.PreToolUse]]\ncommand = \"echo blocked >&2; exit 2\"\n");
    NO_HOOKS.with(|v| v.set(true));
    assert!(disabled());
    assert!(hooks.is_empty());
    assert!(!hooks.has_tool_hooks());
    let outcome = hooks
        .run(
            "PreToolUse",
            Some("bash"),
            &std::env::temp_dir(),
            "s",
            json!({}),
        )
        .await;
    NO_HOOKS.with(|v| v.set(false));
    assert!(outcome.block.is_none());
    assert!(outcome.context.is_empty());
    assert!(outcome.failures.is_empty());
    assert!(!disabled());
}

#[tokio::test(flavor = "current_thread")]
async fn json_answers_block_or_add_context() {
    let hooks = config(concat!(
        "[[hooks.UserPromptSubmit]]\ncommand = \"echo 'Today is release day.'\"\n",
        "[[hooks.Stop]]\ncommand = \"echo '{\\\"decision\\\": \\\"block\\\", \\\"reason\\\": \\\"run the tests first\\\"}'\"\n",
    ));
    let cwd = std::env::temp_dir();
    let prompt = hooks
        .run(
            "UserPromptSubmit",
            None,
            &cwd,
            "s",
            json!({"prompt": "ship it"}),
        )
        .await;
    assert_eq!(prompt.context, vec!["Today is release day.".to_string()]);
    let stop = hooks
        .run("Stop", None, &cwd, "s", json!({"stop_hook_active": false}))
        .await;
    assert_eq!(stop.block.as_deref(), Some("run the tests first"));
}
