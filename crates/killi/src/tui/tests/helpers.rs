use super::*;
use super::super::commands::is_command;
use super::super::notify::{base64, notification_args, shows_osc9, osc9_sequence};
use super::super::picker::{PickerChoice, prune_choices, search_options};
use super::super::run::wants_recap;
use super::super::start::{crash_line, parse_background, parse_ground};
use super::super::turn::{aside_question, leaves_something_for_later, take_sent};

#[test]
fn a_crash_record_is_one_stamped_json_line() {
    let line = crash_line("boom \"x\"", 7);
    assert!(line.ends_with("}\n") && line.matches('\n').count() == 1, "{line}");
    let back: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(back["type"].as_str(), Some("crash"));
    assert_eq!(back["message"].as_str(), Some("boom \"x\""));
    assert_eq!(back["ts"].as_u64(), Some(7));
}

#[test]
fn fullscreen_is_the_setting_or_kil() {
    assert!(wants_fullscreen(None, "kil"));
    assert!(!wants_fullscreen(None, "iTerm.app"));
    assert!(wants_fullscreen(Some(true), "iTerm.app"));
    assert!(!wants_fullscreen(Some(false), "kil"));
}

#[test]
fn prune_offers_only_counts_that_remove_something() {
    assert_eq!(prune_choices(3), Vec::<usize>::new());
    assert_eq!(prune_choices(12), [5, 10]);
    assert_eq!(prune_choices(500), [5, 10, 20, 50, 100, 200]);
    assert_eq!(megabytes(312_000), "312 KB");
    assert_eq!(megabytes(4_200_000), "4.2 MB");
}

#[test]
fn a_reply_that_leaves_work_for_later_is_recognised() {
    assert!(leaves_something_for_later("Fixed the test. The flaky CI job is a follow-up."));
    assert!(leaves_something_for_later("修好了；缓存的问题留到之后再做。"));
    assert!(!leaves_something_for_later("Fixed the test; cargo test passes."));
}

#[test]
fn base64_pads_like_the_standard() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"killi session"), "a2lsbGkgc2Vzc2lvbg==");
}

#[test]
fn a_notifications_line_is_the_first_words_of_the_text() {
    assert_eq!(first_line("修好了：见下方。\n详情略。", 120), "修好了：见下方。");
    assert_eq!(first_line("\n\n  ## 结论\n 正文", 120), "结论");
    assert_eq!(first_line("```rust\nfn x() {}\n```\n改完了。", 120), "改完了。");
    assert_eq!(first_line("- **完成** 三项", 120), "完成 三项");
    assert_eq!(first_line("0123456789", 5), "0123…");
    assert_eq!(first_line("", 120), "");
    assert_eq!(first_line("\n   \n", 120), "");
}

#[test]
fn a_notification_cannot_break_out_of_its_sequence_or_script() {
    let sequence = osc9_sequence("done", "line\u{7}with\u{1b}escapes");
    assert!(sequence.starts_with("\x1b]9;killi: ") && sequence.ends_with('\u{7}'));
    let body = &sequence["\x1b]9;killi: ".len()..sequence.len() - 1];
    assert!(!body.contains(['\u{7}', '\u{1b}']) && body.starts_with("done — linewith"), "{sequence:?}");
    let args = notification_args("say \"hi\"\u{7}", "");
    assert_eq!(args, ["-e", "display notification \"say 'hi'\" with title \"killi\""]);
    let args = notification_args("h", "d\"etail");
    assert_eq!(args, ["-e", "display notification \"d'etail\" with title \"killi\" subtitle \"h\""]);
    }

    #[test]
    fn kil_gets_osc9_whatever_version_its_pane_remembers() {
        assert!(shows_osc9("kil", false));
        assert!(shows_osc9("", true));
        assert!(!shows_osc9("Apple_Terminal", false));
    }

    #[test]
    fn the_terminals_background_answer_is_read_at_any_depth() {
        assert_eq!(parse_background("\x1b]11;rgb:0b0b/0f0f/1414\x1b\\"), Some((11, 15, 20)));
        assert_eq!(parse_background("\x1b]11;rgb:f5/f7/f4\x07"), Some((245, 247, 244)));
        assert_eq!(parse_background("\x1b]11;rgb:f/f/f\x07"), Some((255, 255, 255)));
        assert_eq!(parse_background("\x1b]11;rgb:ffff/ffff\x07"), None);
        assert_eq!(parse_background(""), None);
    }

    #[test]
    fn the_terminals_foreground_and_background_answers_are_read_together() {
        let both = "\x1b]11;rgb:1111/1313/1616\x1b\\\x1b]10;rgb:d7d7/dada/e0e0\x1b\\";
        assert_eq!(parse_ground(both), Some(Ground { background: (17, 19, 22), foreground: Some((215, 218, 224)) }));
        let swapped = "\x1b]10;rgb:ff/ff/ff\x07\x1b]11;rgb:00/00/00\x07";
        assert_eq!(parse_ground(swapped), Some(Ground { background: (0, 0, 0), foreground: Some((255, 255, 255)) }));
        assert_eq!(parse_ground("\x1b]11;rgb:0b/0f/14\x07"), Some(Ground { background: (11, 15, 20), foreground: None }));
        assert_eq!(parse_ground("\x1b]10;rgb:ff/ff/ff\x07"), None, "the background is the one that must be there");
        assert_eq!(parse_ground("\x1b]11;rgb:1111/1313/16"), None, "an answer isn't read before it is complete");
    }

    #[test]
    fn resume_command_quotes_only_when_needed() {
        assert_eq!(resume_command(Path::new("/home/a@b.c/.killi/sessions/-src-app/1-2.jsonl")), "killi -r /home/a@b.c/.killi/sessions/-src-app/1-2.jsonl");
        assert_eq!(resume_command(Path::new("/s/-My Code-it's/1-2.jsonl")), r"killi -r '/s/-My Code-it'\''s/1-2.jsonl'");
    }

    #[test]
    fn choosing_a_skill_leaves_it_in_the_prompt() {
        let item = |value: &str| Item { value: value.into(), description: String::new() };
        let menu = Suggestions { items: vec![item("usage"), item("cost"), item("pr-resolve")], start: 0, fill: Fill::Command };
        assert!(runs_at_once(&menu, 0));
        assert!(runs_at_once(&menu, 1));
        assert!(!runs_at_once(&menu, 2));
        assert!(!runs_at_once(&Suggestions { start: 4, ..menu }, 0));
    }

    #[test]
    fn a_skill_keeps_the_images_pasted_beside_it() {
        let image = Image { label: "[Image #1]".into(), url: "data:image/png;base64,AA".into() };
        match submission("/lean-build make it nicer [Image #1]".into(), vec![image.clone()]) {
            Some(Input::Command(name, arg, images)) => {
                assert_eq!((name.as_str(), arg.as_str()), ("lean-build", "make it nicer [Image #1]"));
                assert_eq!(images, vec![image.clone()]);
            }
            _ => panic!("a prompt starting with / is a command"),
        }
        assert!(matches!(submission("what is [Image #1]".into(), vec![image]), Some(Input::Prompt(_, images)) if images.len() == 1));
        assert!(submission("  ".into(), Vec::new()).is_none());
    }

    #[test]
    fn a_command_that_needs_an_argument_waits_for_it() {
        let menu = Suggestions { items: vec![Item { value: "btw".into(), description: String::new() }], start: 0, fill: Fill::Command };
        assert!(!runs_at_once(&menu, 0));
    }

    #[test]
    fn an_effort_is_named_among_the_ones_the_model_takes() {
        let efforts = ["off", "high"];
        assert_eq!(effort_named("m/x", &efforts, " High "), Ok("high".to_string()));
        assert_eq!(effort_named("m/x", &efforts, "turbo"), Err("m/x has no effort “turbo”; choose one of off, high.".to_string()));
        assert_eq!(effort_named("m/x", &efforts, ""), Err("Say which effort: /effort off | high.".to_string()));
    }

    #[test]
    fn the_commands_offered_while_busy_exist() {
        assert!(BUSY_COMMANDS.iter().all(|name| is_command(name)));
    }

    #[test]
    fn search_options_list_the_services_and_mark_the_one_in_use() {
        let names = |options: &[(String, String, PickerChoice)]| options.iter().map(|(name, _, _)| name.clone()).collect::<Vec<_>>();
        let options = search_options(&crate::config::SearchConfig::default());
        assert_eq!(names(&options), ["brave", "tavily", "searxng"]);
        assert!(options[0].1.contains("in use"));
        let searxng = crate::config::SearchConfig { provider: Some("searxng".into()), ..Default::default() };
        assert!(search_options(&searxng)[2].1.contains("in use"));
        assert!(!search_options(&searxng)[0].1.contains("in use"));
    }

    #[test]
    fn a_closed_session_names_what_it_stopped() {
        assert_eq!(stopped_note(&[]), None);
        assert_eq!(stopped_note(&["npm run dev".into(), "./server".into()]).unwrap(), "Stopped what was still running: `npm run dev`, `./server`.");
    }

    #[test]
    fn retry_reasons_keep_the_status_and_message() {
        let reason = r#"HTTP 529 <unknown status code>: {"type":"error","error":{"type":"overloaded_error","message":"busy (2064)"}}"#;
        assert_eq!(gist(reason), "HTTP 529: busy (2064)");
        assert_eq!(gist("connection reset"), "connection reset");
    }

    #[test]
    fn a_spent_quota_points_to_another_model_not_to_waiting_a_moment() {
        let spent = error_hint(&anyhow::anyhow!("HTTP 429 Too Many Requests: Usage limit reached for 5 hour. Your limit will reset at 16:06")).unwrap();
        assert!(spent.contains("usage limit is spent"));
        let limited = error_hint(&anyhow::anyhow!("HTTP 429 Too Many Requests: slow down")).unwrap();
        assert!(limited.contains("wait a moment"));
    }

    #[test]
    fn hint_follows_status_not_digits() {
        let overflow = anyhow::Error::new(ProviderError {
            status: Some(500),
            kind: ErrorKind::Other,
            message: "prompt is too long: 214013 tokens".to_string(),
            attempts: 1,
            advice: String::new(),
        });
        let hint = error_hint(&overflow).unwrap_or("");
        assert!(!hint.contains("api_key"), "digits in the text are not a 401: {hint}");
        assert!(error_hint(&anyhow::anyhow!("tried model 4010 twice")).is_none(), "digits alone hint at nothing");
        let auth = anyhow::Error::new(ProviderError {
            status: Some(401),
            kind: ErrorKind::Auth,
            message: "bad key".to_string(),
            attempts: 1,
            advice: String::new(),
        });
        assert!(error_hint(&auth).unwrap().contains("api_key"));
    }

    #[test]
    fn queued_messages_the_agent_took_leave_the_queue_oldest_first() {
        let mut queued = vec!["first".to_string(), "second".to_string(), "third".to_string()];
        assert!(take_sent(&mut queued, 3).is_empty());
        assert_eq!(take_sent(&mut queued, 1), ["first", "second"]);
        assert_eq!(queued, ["third"]);
        assert_eq!(take_sent(&mut queued, 0), ["third"]);
        assert!(queued.is_empty());
    }

    #[test]
    fn undo_with_nothing_to_undo_is_a_notice_not_an_error() {
        let root = crate::test_temp::TempDir::new("killi-undo");
        assert!(undo_empty(&Snapshots::new(&root)));
    }

    #[test]
    fn undo_paths_take_the_path_after_the_status() {
        let changes = ["M\tsrc/a.rs", "A\tb.txt", "D\told name.rs", "M README.md", ""];
        let changes: Vec<String> = changes.iter().map(|s| s.to_string()).collect();
        assert_eq!(undo_paths(&changes), ["src/a.rs", "b.txt", "old name.rs", "README.md"]);
    }

    #[test]
    fn the_undo_note_names_the_prompt_and_caps_the_paths() {
        let short = undo_note("Fix the bug", &["a".to_string(), "b".to_string()]);
        assert_eq!(
            short,
            "The user ran /undo: files were restored to how they were before \"Fix the bug\". Changed back: a, b. Read files again before relying on what you saw."
        );
        let paths: Vec<String> = (1..=35).map(|n| format!("f{n}.rs")).collect();
        let note = undo_note("Fix the bug", &paths);
        assert!(note.contains("Changed back: f1.rs, f2.rs"), "{note}");
        assert!(note.contains("f30.rs, and 5 more"), "{note}");
        assert!(!note.contains("f31.rs"), "{note}");
        assert!(note.ends_with("Read files again before relying on what you saw."), "{note}");
    }

    #[test]
    fn btw_takes_the_rest_of_the_line() {
        assert_eq!(aside_question("/btw what is this?").as_deref(), Some("what is this?"));
        assert_eq!(aside_question("  /btw  two\nlines ").as_deref(), Some("two\nlines"));
        assert_eq!(aside_question("/btw"), None);
        assert_eq!(aside_question("/btwx y"), None);
        assert_eq!(aside_question("ask /btw y"), None);
    }

    #[test]
    fn typing_in_a_picker_fuzzy_filters_its_options() {
        let models = ["minimax/MiniMax-M3.1-Flash", "qwen/qwen3.7-plus", "qwen/qwen3.8-max", "zai/glm-5.3"];
        let options = models.iter().map(|m| (m.to_string(), String::new(), PickerChoice::Model(m.to_string()))).collect();
        let mut picker = Picker::new("Switch model".into(), options, 3);
        assert!(matches!(picker.choice(), Some(PickerChoice::Model(m)) if m == "zai/glm-5.3"));
        picker.filter("q38".into());
        assert!(matches!(picker.choice(), Some(PickerChoice::Model(m)) if m == "qwen/qwen3.8-max"));
        picker.filter("glm".into());
        assert_eq!(picker.shown, [3]);
        picker.filter("xyz".into());
        assert!(picker.choice().is_none());
        picker.filter(String::new());
        assert_eq!(picker.shown.len(), 4);
    }

    #[test]
    fn a_proxy_error_page_shows_its_title_not_its_markup() {
        let nginx = "<html><head><title>502 Bad Gateway</title></head><body><h1>Bad Gateway</h1></body></html>";
        assert_eq!(gist(&format!("HTTP 502 Bad Gateway: {nginx}")), "HTTP 502: 502 Bad Gateway");
        let cloudflare = "<!doctype html><html><head><title>Attention Required! | Cloudflare</title><style>.x{color:red}</style></head></html>";
        assert_eq!(gist(&format!("HTTP 403 Forbidden: {cloudflare}")), "HTTP 403: Attention Required! | Cloudflare");
    }

    #[test]
    fn a_proxy_page_without_a_title_shows_its_first_line() {
        let page = "<html><body><h1>Service Unavailable</h1>\n<p>try again</p></body></html>";
        assert_eq!(gist(&format!("HTTP 503: {page}")), "HTTP 503: Service Unavailable");
    }

    #[test]
    fn angle_brackets_outside_a_page_are_untouched() {
        assert_eq!(gist("connection reset"), "connection reset");
        assert_eq!(gist("expected <foo> but got <bar>"), "expected <foo> but got <bar>");
    }

    #[test]
    fn a_failed_request_shows_its_status_and_message_and_what_follows() {
        let reason = r#"HTTP 401 Unauthorized: {"error":{"message":"Invalid API key"}} (after 1 attempt(s))"#;
        assert_eq!(gist(reason), "HTTP 401: Invalid API key (after 1 attempt(s))");
    }

    #[test]
    fn only_a_completed_turn_that_used_tools_is_recapped_by_itself() {
        assert!(wants_recap(true, true, 3));
        assert!(!wants_recap(true, true, 0), "an answer is its own recap");
        assert!(!wants_recap(true, false, 3), "interrupted or cut off");
        assert!(!wants_recap(false, true, 3), "switched off");
    }

    #[test]
    fn a_closed_pipe_reads_as_a_gone_terminal() {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: fills the two descriptors.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        assert!(!terminal_gone(fds[0]), "an open pipe is not gone");
        // SAFETY: writes one byte this test reads back, and closes descriptors it opened.
        assert_eq!(unsafe { libc::write(fds[1], b"x".as_ptr().cast(), 1) }, 1);
        unsafe { libc::close(fds[1]) };
        assert!(terminal_gone(fds[0]), "a hangup is gone even with input buffered");
        let mut byte = [0u8; 1];
        // SAFETY: reads one byte into a buffer of that length.
        assert_eq!(unsafe { libc::read(fds[0], byte.as_mut_ptr().cast(), 1) }, 1, "the probe ate no input");
        assert!(terminal_gone(fds[0]), "a hangup with nothing to read is gone");
        unsafe { libc::close(fds[0]) };
        assert!(terminal_gone(-1), "a bad descriptor reads as gone too");
    }
