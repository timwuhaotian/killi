#[test]
fn prune_keeps_the_newest_and_the_current_session_and_removes_the_rest_with_their_folders() {
    let root = std::env::temp_dir().join(format!("killi-prune-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (a, b) = (root.join("-p-one"), root.join("-p-two"));
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let logs = [a.join("1-1.jsonl"), a.join("2-2.jsonl"), b.join("3-3.jsonl"), b.join("4-4.jsonl")];
    for (i, log) in logs.iter().enumerate() {
        fs::write(log, "x".repeat(100)).unwrap();
        let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i as u64 * 60);
        File::options().write(true).open(log).unwrap().set_modified(when).unwrap();
    }
    fs::create_dir_all(a.join("1-1")).unwrap();
    fs::write(a.join("1-1/agent-1.jsonl"), "y".repeat(50)).unwrap();
    let newest: Vec<PathBuf> = list_all_in(&root).into_iter().map(|(p, _)| p).collect();
    assert_eq!(newest, [logs[3].clone(), logs[2].clone(), logs[1].clone(), logs[0].clone()]);

    let pruned = prune_in(&root, 1, &logs[1]).unwrap();
    assert_eq!(pruned, Pruned { sessions: 2, bytes: 250 });
    assert!(logs[3].exists() && logs[1].exists(), "the newest and the current stay");
    assert!(!logs[0].exists() && !logs[2].exists() && !a.join("1-1").exists());
    assert!(a.exists() && b.exists());
    assert_eq!(prune_in(&root, 0, &logs[1]).unwrap().sessions, 1);
    assert!(!b.exists(), "a project folder left empty goes too");
    fs::remove_dir_all(root).unwrap();
}

use super::*;
use serde_json::json;

fn loaded(messages: Vec<Value>) -> Loaded {
    Loaded { messages, tools: HashMap::new(), context: 0, choice: Choice::default(), backup: None }
}

#[test]
fn a_resumed_session_shows_prompts_text_and_tool_calls_in_order() {
    let mut log = loaded(vec![
        json!({"role": "system", "content": "sys"}),
        json!({"role": "user", "content": "<system-reminder>note</system-reminder>\n\nfix it"}),
        json!({"role": "assistant", "content": "Looking.", "tool_calls": [
            {"id": "c1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"cd /w && ls\"}"}},
            {"id": "c2", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"/w/a.rs\"}"}},
        ]}),
        json!({"role": "tool", "tool_call_id": "c1", "content": "a\nb\n[Command exited with code 2]"}),
        json!({"role": "tool", "tool_call_id": "c2", "content": "1\tfn main() {}"}),
        json!({"role": "user", "content": "<system-reminder>only a reminder</system-reminder>"}),
        json!({"role": "assistant", "content": "Done."}),
    ]);
    log.tools.insert("c2".into(), ToolEnd { is_error: false, brief: "1 line".into() });
    let shown = replay(&log, Path::new("/w"), 10);
    assert_eq!(
        shown,
        [
            Replayed::Prompt("fix it".into()),
            Replayed::Text("Looking.".into()),
            Replayed::Tool { summary: "bash ls".into(), output: "a\nb\n[Command exited with code 2]".into(), is_error: true, brief: "Command exited with code 2".into() },
            Replayed::Tool { summary: "read a.rs".into(), output: "1\tfn main() {}".into(), is_error: false, brief: "1 line".into() },
            Replayed::Text("Done.".into()),
        ]
    );
}

#[test]
fn a_replay_keeps_the_last_prompts_and_counts_the_rest() {
    let mut messages = vec![json!({"role": "system", "content": "sys"})];
    for n in 1..=4 {
        messages.push(json!({"role": "user", "content": format!("prompt {n}")}));
        messages.push(json!({"role": "assistant", "content": format!("answer {n}")}));
    }
    let shown = replay(&loaded(messages), Path::new("/w"), 2);
    assert_eq!(shown[0], Replayed::Note("2 earlier prompts not shown.".into()));
    assert_eq!(shown[1], Replayed::Prompt("prompt 3".into()));
    assert_eq!(shown.len(), 5);
}

#[test]
fn prompts_come_back_without_reminders() {
    assert_eq!(strip_reminders("<system-reminder>a</system-reminder>\n\nfix it"), Some("fix it"));
    assert_eq!(strip_reminders("<system-reminder>Plan mode is on.</system-reminder>"), None);
    let compacted = format!("{COMPACTED_MARK} to save space.</system-reminder>\n\n<user-requests>\n<request>\none\n</request>\n<request>\ntwo\n</request>\n</user-requests>");
    let messages = vec![json!({"role": "user", "content": compacted}), json!({"role": "user", "content": "three"})];
    assert_eq!(user_prompts(&messages), ["one", "two", "three"]);
}

#[test]
fn a_skill_run_comes_back_as_the_command_that_started_it() {
    let skill = "Use the pr-review skill for this.\n\n<skill name=\"pr-review\" folder=\"/s/pr-review\">\n---\nname: pr-review\n---\nReview it.\n</skill>";
    let files = "<system-reminder>The files the user named with @, as they are now:\n\n<file path=\"a.rs\">\nfn a() {}\n</file></system-reminder>";
    let messages = vec![
        json!({"role": "system", "content": "sys"}),
        json!({"role": "user", "content": format!("<system-reminder>Plan mode is on.</system-reminder>\n\n{skill}\n\nreview 42")}),
        json!({"role": "user", "content": format!("{skill}\n\nFollow the skill.")}),
        json!({"role": "user", "content": format!("{skill}\n\nlook at @a.rs\n\n{files}")}),
        json!({"role": "user", "content": format!("explain @a.rs\n\n{files}")}),
    ];
    let shown = ["/pr-review review 42", "/pr-review", "/pr-review look at @a.rs", "explain @a.rs"];
    assert_eq!(user_prompts(&messages), shown);
    assert_eq!(replay(&loaded(messages), Path::new("/w"), 8), shown.map(|p| Replayed::Prompt(p.into())));
    // Requests a compaction kept before it kept them short: cut inside the files, or the skill.
    let cut = format!("{COMPACTED_MARK} to save space.</system-reminder>\n\n<user-requests>\n<request>\nexplain @a.rs\n\n{}\n[… 900 characters cut]\n</request>\n<request>\n{}\n[… 900 characters cut]\n</request>\n</user-requests>", &files[..60], &skill[..60]);
    assert_eq!(user_prompts(&[json!({"role": "user", "content": cut})]), ["explain @a.rs", "/pr-review"]);
}

#[test]
fn load_restarts_at_a_compaction() {
    let dir = std::env::temp_dir().join(format!("killi-session-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("1-1.jsonl");
    let lines = [
        json!({"type": "session", "model": "m/x"}),
        json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
        json!({"type": "message", "message": {"role": "user", "content": "first"}}),
        json!({"type": "compaction", "messages": [{"role": "system", "content": "sys"}, {"role": "user", "content": "summary"}]}),
        json!({"type": "message", "message": {"role": "user", "content": "next"}}),
    ];
    fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    let loaded = load(&path).unwrap();
    assert_eq!(loaded.choice, Choice { model: Some("m/x".into()), effort: None });
    let contents: Vec<_> = loaded.messages.iter().map(|m| m["content"].as_str().unwrap()).collect();
    assert_eq!(contents, ["sys", "summary", "next"]);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn scan_prompts_matches_a_full_load_without_parsing_tool_results() {
    let dir = std::env::temp_dir().join(format!("killi-scan-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("1-1.jsonl");
    let big = "x".repeat(200_000);
    let decoy = r#"{"role":"user","content":"not a message"}"#;
    let lines = [
        json!({"type": "session", "model": "user"}),
        json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
        json!({"type": "message", "message": {"role": "user", "content": "<system-reminder>note</system-reminder>\n\nfix it"}}),
        json!({"type": "message", "message": {"role": "assistant", "content": "on it", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{}"}}]}}),
        json!({"type": "message", "message": {"role": "tool", "tool_call_id": "c1", "content": format!("{big} {decoy} compaction")}}),
        json!({"type": "usage", "prompt": 900}),
        json!({"type": "message", "message": {"role": "user", "content": "<system-reminder>only a reminder</system-reminder>"}}),
        json!({"type": "compaction", "messages": [{"role": "system", "content": "sys"}, {"role": "user", "content": "before"}]}),
        json!({"type": "message", "message": {"role": "user", "content": "after\nmore"}}),
    ];
    fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    let loaded = load(&path).unwrap();
    let prompts = user_prompts(&loaded.messages);
    assert_eq!(prompts, ["before", "after\nmore"]);
    assert_eq!(scan_prompts(&path), Some(("before".to_string(), 2)));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn scan_prompts_skips_logs_a_load_would_refuse_or_find_empty() {
    let dir = std::env::temp_dir().join(format!("killi-scan-skip-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, lines: Vec<Value>| {
        let path = dir.join(name);
        fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        path
    };
    let no_system =
        write("a.jsonl", vec![json!({"type": "message", "message": {"role": "user", "content": "hi"}})]);
    assert!(load(&no_system).is_err());
    assert_eq!(scan_prompts(&no_system), None);
    let assistant_first = write(
        "b.jsonl",
        vec![
            json!({"type": "message", "message": {"role": "assistant", "content": "hi"}}),
            json!({"type": "message", "message": {"role": "user", "content": "yo"}}),
        ],
    );
    assert!(load(&assistant_first).is_err());
    assert_eq!(scan_prompts(&assistant_first), None);
    let reminders_only = write(
        "c.jsonl",
        vec![
            json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
            json!({"type": "message", "message": {"role": "user", "content": "<system-reminder>only a reminder</system-reminder>"}}),
        ],
    );
    assert!(load(&reminders_only).is_ok());
    assert_eq!(scan_prompts(&reminders_only), None);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn child_logs_skip_existing_names() {
    let root = std::env::temp_dir().join(format!("killi-child-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let parent = Session::open_new(root.join("1-1.jsonl")).unwrap();
    fs::create_dir_all(&parent.dir).unwrap();
    fs::write(parent.dir.join("agent-1.jsonl"), "taken").unwrap();
    let (first, used) = parent.child(1).unwrap();
    assert_eq!(used, 2);
    assert!(first.log_path.ends_with("agent-2.jsonl"));
    let (_, reused) = parent.child(1).unwrap();
    assert_eq!(reused, 3, "agent-2 now exists too");
    drop(first);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn discarding_removes_the_log_and_only_an_empty_folder() {
    let root = std::env::temp_dir().join(format!("killi-discard-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let empty = Session::open_new(root.join("1-1.jsonl")).unwrap();
    empty.discard();
    assert!(!root.join("1-1.jsonl").exists());
    let kept = Session::open_new(root.join("2-2.jsonl")).unwrap();
    fs::create_dir_all(&kept.dir).unwrap();
    fs::write(kept.dir.join("result-1.txt"), "saved output").unwrap();
    kept.discard();
    assert!(!root.join("2-2.jsonl").exists());
    assert!(kept.dir.join("result-1.txt").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_log_open_in_one_session_cannot_be_opened_again() {
    let dir = std::env::temp_dir().join(format!("killi-lock-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("1-1.jsonl");
    let mut one = Session::open_new(path.clone()).unwrap();
    one.record(&json!({"type": "message", "message": {"role": "system", "content": "sys"}})).unwrap();

    assert!(is_locked(&path), "the log is locked while its session runs");
    match Session::open(&path) {
        Ok(_) => panic!("a log held by one session opened again"),
        Err(e) => assert!(e.to_string().starts_with("session is in use"), "{e:#}"),
    }
    drop(one);

    assert!(!is_locked(&path), "the lock goes when the session ends");
    assert!(Session::open(&path).is_ok());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_second_session_in_the_same_second_takes_the_next_name() {
    let root = std::env::temp_dir().join(format!("killi-names-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let one = Session::create_in(&root).unwrap();
    let two = Session::create_in(&root).unwrap();
    assert_ne!(one.log_path, two.log_path, "the same second and pid don't share a log");

    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let taken = root.join(format!("{secs}-{}.jsonl", std::process::id()));
    fs::write(&taken, "x").unwrap();
    let three = Session::create_in(&root).unwrap();
    assert_ne!(three.log_path, taken, "a name already used is skipped");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn load_keeps_how_tools_ended_and_how_full_the_context_was() {
    let dir = std::env::temp_dir().join(format!("killi-ends-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("1-1.jsonl");
    let lines = [
        json!({"type": "session", "model": "m/x", "ts": 1}),
        json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
        json!({"type": "usage", "prompt": 900, "cached": 0, "completion": 5}),
        json!({"type": "message", "message": {"role": "tool", "tool_call_id": "c1", "content": "boom"}, "tool": {"is_error": true, "brief": "exit 1", "ms": 40}}),
        json!({"type": "usage", "prompt": 1200, "cached": 900, "completion": 7}),
    ];
    fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    let loaded = load(&path).unwrap();
    assert_eq!(loaded.context, 1200);
    assert_eq!(loaded.tools["c1"], ToolEnd { is_error: true, brief: "exit 1".into() });
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_entry_is_one_stamped_line() {
    let entry = json!({"type": "message", "message": {"role": "user", "content": "a \"quoted\"\nline"}});
    let line = stamped(&entry, 7);
    assert!(line.ends_with("}\n") && line.matches('\n').count() == 1, "{line}");
    let back: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(back["ts"], 7);
    assert_eq!(back["message"], entry["message"]);
    assert_eq!(serde_json::from_str::<Value>(&stamped(&json!({}), 3)).unwrap(), json!({"ts": 3}));
    assert_eq!(stamped(&json!("not an object"), 3), "\"not an object\"\n");
}

#[test]
fn recorded_entries_load_back() {
    let dir = std::env::temp_dir().join(format!("killi-record-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mut session = Session::open_new(dir.join("1-1.jsonl")).unwrap();
    session.record(&json!({"type": "message", "message": {"role": "system", "content": "sys"}})).unwrap();
    session.record(&json!({"type": "message", "message": {"role": "user", "content": "hi\nthere"}})).unwrap();
    let loaded = load(&session.log_path).unwrap();
    let contents: Vec<_> = loaded.messages.iter().map(|m| m["content"].as_str().unwrap()).collect();
    assert_eq!(contents, ["sys", "hi\nthere"]);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_silent_result_from_an_old_log_reads_as_no_output() {
    assert_eq!(guess_end("(no output)"), (false, "no output".into()));
    assert_eq!(guess_end("a\nb\n[Command exited with code 2]"), (true, "Command exited with code 2".into()));
    assert_eq!(guess_end("one line"), (false, String::new()));
}

#[test]
fn a_switch_in_the_log_is_what_a_resume_runs_on() {
    let dir = std::env::temp_dir().join(format!("killi-switch-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("1-1.jsonl");
    let lines = [
        json!({"type": "session", "model": "m/x", "effort": "high"}),
        json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
        json!({"type": "model", "model": "m/y", "effort": "medium"}),
        json!({"type": "model", "model": "m/y", "effort": "low"}),
    ];
    fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    assert_eq!(load(&path).unwrap().choice, Choice { model: Some("m/y".into()), effort: Some("low".into()) });
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_backup_entry_is_what_a_resume_runs_on() {
    let dir = std::env::temp_dir().join(format!("killi-backup-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("1-1.jsonl");
    let lines = [
        json!({"type": "session", "model": "m/x"}),
        json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
        json!({"type": "backup", "model": "m/y"}),
        json!({"type": "backup", "model": "m/z"}),
    ];
    fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    assert_eq!(load(&path).unwrap().backup.as_deref(), Some("m/z"), "the last set backup wins");
    let cleared = [
        json!({"type": "session", "model": "m/x"}),
        json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
        json!({"type": "backup", "model": "m/z"}),
        json!({"type": "backup"}),
    ];
    fs::write(&path, cleared.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    assert_eq!(load(&path).unwrap().backup, None, "clearing the backup wins");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_saved_choice_reads_back_and_a_missing_one_is_empty() {
    let path = std::env::temp_dir().join(format!("killi-choice-{}.json", std::process::id()));
    assert_eq!(read_choice(&path), Choice::default());
    let choice = Choice { model: Some("m/y".into()), effort: Some("low".into()) };
    write_choice(&path, &choice).unwrap();
    assert_eq!(read_choice(&path), choice);
    fs::write(&path, "not json").unwrap();
    assert_eq!(read_choice(&path), Choice::default());
    fs::remove_file(path).unwrap();
}

#[test]
fn prune_skips_a_locked_log_and_a_live_listed_one() {
    let root = std::env::temp_dir().join(format!("killi-prune-live-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let project = root.join("proj");
    fs::create_dir_all(&project).unwrap();
    let logs = [project.join("1-1.jsonl"), project.join("2-2.jsonl"), project.join("3-3.jsonl")];
    for (i, log) in logs.iter().enumerate() {
        fs::write(log, "x".repeat(50)).unwrap();
        let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i as u64 * 60);
        File::options().write(true).open(log).unwrap().set_modified(when).unwrap();
    }
    let held = Session::open(&logs[0]).unwrap();
    let live: HashSet<PathBuf> = [logs[2].clone()].into_iter().collect();
    assert!(!deletable(&logs[0], Path::new("nowhere"), &live), "a locked log stays");
    assert!(!deletable(&logs[2], Path::new("nowhere"), &live), "a live-listed log stays");
    assert!(deletable(&logs[1], Path::new("nowhere"), &live));
    assert!(!deletable(&logs[1], &logs[1], &HashSet::new()), "the current session stays");
    let pruned = prune_in(&root, 0, Path::new("nowhere")).unwrap();
    assert!(logs[0].exists(), "a log locked by a running session is skipped");
    assert!(!logs[1].exists() && !logs[2].exists());
    assert_eq!(pruned.sessions, 2);
    drop(held);
    assert_eq!(prune_in(&root, 0, Path::new("nowhere")).unwrap().sessions, 1);
    assert!(!logs[0].exists(), "an unlocked log prunes once its session ends");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn opening_a_torn_log_starts_the_next_record_on_its_own_line() {
    let dir = std::env::temp_dir().join(format!("killi-torn-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("1-1.jsonl");
        fs::write(&path, format!("{}\n{}", json!({"type": "message", "message": {"role": "system", "content": "sys"}}), "{\"type\":\"messa")).unwrap();
        let mut session = Session::open(&path).unwrap();
        session.record(&json!({"type": "message", "message": {"role": "user", "content": "next"}})).unwrap();
        drop(session);
        let report = load_reported(&path).unwrap();
        let contents: Vec<_> = report.loaded.messages.iter().map(|m| m["content"].as_str().unwrap()).collect();
        assert_eq!((contents, report.skipped), (vec!["sys", "next"], 1));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_skips_an_undecodable_line_and_reports_how_many() {
        let dir = std::env::temp_dir().join(format!("killi-badline-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1-1.jsonl");
        let mut bytes = format!("{}\n", json!({"type": "message", "message": {"role": "system", "content": "sys"}})).into_bytes();
        bytes.extend_from_slice(b"\xff\xfe not utf-8\n");
        bytes.extend_from_slice(format!("{}\n", json!({"type": "message", "message": {"role": "user", "content": "hi"}})).as_bytes());
        fs::write(&path, &bytes).unwrap();
        let report = load_reported(&path).unwrap();
        let contents: Vec<_> = report.loaded.messages.iter().map(|m| m["content"].as_str().unwrap()).collect();
        assert_eq!((contents, report.skipped), (vec!["sys", "hi"], 1));
        assert_eq!(load(&path).unwrap().messages.len(), 2, "load keeps its shape for existing callers");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_repairs_a_mid_history_tool_call_that_never_returned() {
        let dir = std::env::temp_dir().join(format!("killi-repair-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1-1.jsonl");
        let call = |id: &str| json!({"id": id, "type": "function", "function": {"name": "read", "arguments": "{}"}});
        let lines = [
            json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
            json!({"type": "message", "message": {"role": "user", "content": "do it"}}),
            json!({"type": "message", "message": {"role": "assistant", "content": "on it", "tool_calls": [call("c1")]}}),
            json!({"type": "message", "message": {"role": "user", "content": "keep going"}}),
            json!({"type": "message", "message": {"role": "assistant", "content": "later", "tool_calls": [call("c2")]}}),
        ];
        fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        let report = load_reported(&path).unwrap();
        assert_eq!(report.repaired, 1, "only the mid-history call is repaired, not the trailing pending one");
        let messages = &report.loaded.messages;
        assert_eq!(messages.len(), 6);
        assert_eq!(messages[3]["tool_call_id"], "c1");
        assert_eq!(messages[3]["role"], "tool");
        assert!(messages[3]["content"].as_str().is_some_and(|c| !c.is_empty()));
        assert_eq!(messages[5]["tool_calls"][0]["id"], "c2");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_logs_and_folders_are_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("killi-perms-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let session = Session::create_in(&root).unwrap();
        assert_eq!(session.log_path.parent(), Some(root.as_path()));
        assert_eq!(session.log_path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        let (child, _) = session.child(0).unwrap();
        assert_eq!(session.dir.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(child.log_path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        drop(child);
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }
