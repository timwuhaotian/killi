use super::*;
use super::super::draw::screen_rows;
use super::super::layout::status_layout;
use super::super::rows::{diff_row, snapshot_word};

#[test]
fn a_long_folder_is_cut_at_a_folder_boundary() {
    assert_eq!(shorten_path("~/code/killi", 20), "~/code/killi");
    assert_eq!(shorten_path("/Users/wuhaotian@sphnet.com.sg/code/killi", 20), "…/code/killi");
    assert_eq!(shorten_path("/Users/me/averyverylongfoldername", 12), "…gfoldername");
}

#[test]
fn help_wraps_descriptions_under_their_column() {
    let bare = |row: &str| {
        let mut out = String::new();
        let mut chars = row.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                chars.by_ref().find(char::is_ascii_alphabetic);
            } else {
                out.push(c);
            }
        }
        out
    };
    let rows: Vec<String> = help_rows(60, true).iter().map(|r| bare(r)).collect();
    let plain: Vec<String> = help_rows(60, false).iter().map(|r| bare(r)).collect();
    assert!(plain.iter().any(|r| r.contains("Ctrl+J")) && !plain.iter().any(|r| r.contains("Shift+Enter")));
    assert!(rows.iter().all(|r| text::width(r) <= 60));
    let recap = rows.iter().position(|r| r.starts_with("  /recap")).unwrap();
    let column = rows[recap].find("Recap").unwrap();
    assert!(rows[recap + 1].starts_with(&" ".repeat(column)), "{:?}", rows[recap + 1]);
    assert!(!rows[recap + 1].trim_start().starts_with('/'));
}

#[test]
fn the_handoff_names_the_log_and_sums_the_session_up() {
    let stats = crate::agent::Stats {
        requests: 6,
        tool_calls: 7,
        usage: Usage { prompt: 22_400, cached: 18_600, completion: 1_200, reasoning: 0 },
        ..crate::agent::Stats::default()
    };
    let prompts = vec!["The slugify test fails.\nFix it.".to_string()];
    let log = Path::new("/h/.killi/sessions/x/1792673250-71085.jsonl");
    let text = handoff("1792673250-71085", log, "~/code/slug", "mimo/mimo-v2.6-pro", "high", &prompts, &stats, None);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "killi session 1792673250-71085 · ~/code/slug");
    assert_eq!(lines[1], "log: /h/.killi/sessions/x/1792673250-71085.jsonl");
    assert_eq!(lines[2], "mimo/mimo-v2.6-pro · effort high · 1 prompt · 6 requests · 7 tool calls · 22.4k in, 83% cached · 1.2k out");
    assert_eq!(lines[3], "1. The slugify test fails. Fix it.");
    assert!(lines[4].contains("`usage`") && lines[4].contains("sessions.py"));
    let later = Some((PathBuf::from("/h/.killi/sessions/x/later.md"), 2));
    let text = handoff("1", log, "~/code/slug", "m", "high", &prompts, &stats, later);
    assert_eq!(text.lines().nth(3).unwrap(), "later: /h/.killi/sessions/x/later.md (2 open: what the agent noted to do later)");
}

#[test]
fn the_live_area_is_padded_down_to_the_bottom_row_until_the_conversation_fills_the_screen() {
    assert_eq!(padding(30, 16, 4), 10);
    assert_eq!(padding(30, 16, 14), 0);
    assert_eq!(padding(30, 40, 4), 0);
}

#[test]
fn the_banner_lists_the_setup_in_its_column() {
    let overview = Overview {
        custom_prompt: true,
        instructions: vec![crate::config::home().join(".agents/AGENTS.md"), PathBuf::from("AGENTS.md")],
        instruction_tokens: 4551,
        skills: 204,
        later: 2,
        ..Overview::default()
    };
    let rows: Vec<String> = overview_rows(&overview).iter().map(|row| unstyled(row)).collect();
    assert_eq!(rows, [
        "rules  ~/.agents/AGENTS.md, AGENTS.md · ~4.6k tokens",
        "skills 204",
        "prompt custom · [prompt] in config.toml",
        "later  2 noted · /later",
    ]);
    assert!(overview_rows(&Overview::default()).is_empty());
    let hints: Vec<String> = key_hints(true).iter().map(|hint| unstyled(hint)).collect();
    assert_eq!(hints.join(" · "), "/ commands and skills · @ files · Shift+Enter new line · Shift+Tab plan mode · Esc interrupts");
    let plain: Vec<String> = key_hints(false).iter().map(|hint| unstyled(hint)).collect();
    assert_eq!(plain.join(" · "), "/ commands and skills · @ files · Ctrl+J new line · Shift+Tab plan mode · Esc interrupts");
}

#[test]
fn a_long_line_takes_the_rows_it_wraps_to() {
    assert_eq!(screen_rows("", 80), 1);
    assert_eq!(screen_rows(&format!("{DIM}{}{RESET}", "x".repeat(80)), 80), 1);
    assert_eq!(screen_rows(&"x".repeat(81), 80), 2);
    assert_eq!(screen_rows("a\nb", 80), 2);
}

    #[test]
    fn the_status_line_drops_detail_before_the_model_as_it_narrows() {
        let status = Status {
            window: Some(200_000),
            context: 38_000,
            prompt: 38_000,
            cached: 30_000,
            completion: 2_000,
            branch: Some("main".into()),
            ..Status::default()
        };
        let line = |width| status_layout("qwen3.7-plus", &status, width).map(|line| unstyled(&line));
        let wide = line(120).unwrap();
        assert!(wide.contains("cached") && wide.contains("⎇ main"));
        for width in 12..120 {
            let Some(shown) = line(width) else { continue };
            assert!(text::width(&shown) <= width, "{width}: {shown}");
            assert!(shown.starts_with("qwen3.7-plus"), "{width}: {shown:?}");
        }
        let narrow = line(40).unwrap();
        assert!(narrow.contains("ctx") && !narrow.contains("cached"));
    }

    #[test]
    fn tool_rows_bold_the_name_mute_the_folders_and_hang_output_off_a_gutter() {
        assert_eq!(tool_summary("read src/lib.rs"), format!("{BOLD}read{RESET} {DIM}src/{RESET}lib.rs"));
        assert_eq!(tool_summary("edit Cargo.toml"), format!("{BOLD}edit{RESET} Cargo.toml"));
        assert_eq!(tool_summary("write crates/a/b.rs"), format!("{BOLD}write{RESET} {DIM}crates/a/{RESET}b.rs"));
        assert_eq!(tool_summary("grep \"x\" src/"), format!("{BOLD}grep{RESET} \"x\" src/"), "only a path argument is split");
        assert_eq!(tool_summary("todo"), format!("{BOLD}todo{RESET}"));
        assert_eq!(unstyled(&diff_row("+12\tlet x = 1;", 40, false)), "  │   12 + let x = 1;");
        assert!(diff_row("-3\told", 40, false).contains(&format!("{DANGER}- old")));
        assert_eq!(unstyled(&diff_row("@@ hunk", 40, true)), "  │ @@ hunk");
    }

    #[test]
    fn an_added_or_removed_line_sits_on_a_stripe_to_the_edge_of_the_window() {
        let added = diff_row("+12\tlet x = 1;", 30, true);
        assert!(added.contains(&ADD.to_string()) && added.ends_with(RESET));
        assert_eq!(text::width(&added), 30);
        assert_eq!(unstyled(&added), format!("  │   12 + let x = 1;{}", " ".repeat(30 - 21)));
        let removed = diff_row("-3\told", 30, true);
        assert!(removed.contains(&DEL.to_string()) && removed.contains(&format!("{DANGER}-")));
        assert!(!removed.contains(&format!("{DANGER}- old")), "the text keeps its own color");
        let context = diff_row(" 4\tkept", 30, true);
        assert!(!context.contains(&ADD.to_string()) && !context.contains(&DEL.to_string()), "context has no stripe");
        assert_eq!(text::width(&diff_row("+1\t", 30, false)), 11, "without stripes the row ends with its text");
        let long = diff_row(&format!("+1\t{}", "x".repeat(80)), 30, true);
        assert_eq!(text::width(&long), 30);
        assert!(unstyled(&long).ends_with('…'), "a long line is cut with an ellipsis, on its stripe to the end");
        assert!(long.contains(&ADD.to_string()));
    }

    #[test]
    fn only_the_first_row_that_hides_output_names_the_key() {
        assert_eq!(unstyled(&hidden_row(14, "more", true)), "  │ … 14 more lines · Ctrl+O");
        assert_eq!(unstyled(&hidden_row(1, "earlier", false)), "  │ … 1 earlier line");
        assert_eq!(text::hanging_indent(&unstyled(&hidden_row(3, "more", false))), 4, "a wrapped row hangs under its text");
    }

    #[test]
    fn hints_leave_out_the_least_useful_first_and_stay_in_their_order() {
        let hints = [(2, "type to filter".to_string()), (1, String::new()), (4, "↑↓ choose".into()), (5, "Tab or Enter selects".into()), (3, "Esc closes".into())];
        assert_eq!(unstyled(&hint_row(&hints, 80)), "  type to filter · ↑↓ choose · Tab or Enter selects · Esc closes");
        assert_eq!(unstyled(&hint_row(&hints, 52)), "  ↑↓ choose · Tab or Enter selects · Esc closes");
        assert_eq!(unstyled(&hint_row(&hints, 34)), "  ↑↓ choose · Tab or Enter selects");
        let narrow = unstyled(&hint_row(&hints, 12));
        assert!(text::width(&narrow) <= 12 && narrow.ends_with('…'), "{narrow:?}: the last one left is cut with an ellipsis");
        for width in 5..90 {
            assert!(text::width(&hint_row(&hints, width)) <= width, "{width}");
        }
    }

    #[test]
    fn items_wrap_between_themselves_never_inside_one() {
        let items: Vec<String> = ["/ commands and skills", "Shift+Enter new line", "Shift+Tab plan mode", "Esc interrupts"].map(String::from).to_vec();
        let rows: Vec<String> = wrap_items(&items, 80).iter().map(|row| unstyled(row)).collect();
        assert_eq!(rows, ["/ commands and skills · Shift+Enter new line · Shift+Tab plan mode", "Esc interrupts"]);
        assert_eq!(wrap_items(&items, 118).len(), 1);
        for width in 8..120 {
            let rows = wrap_items(&items, width);
            assert!(rows.iter().all(|row| text::width(row) <= width), "{width}");
            let joined = rows.iter().map(|row| unstyled(row)).collect::<Vec<_>>().join(" · ");
            if width >= 22 {
                assert!(items.iter().all(|item| joined.contains(&unstyled(item))), "{width}: {joined}");
            }
        }
    }

    #[test]
    fn the_status_line_keeps_the_model_at_the_left_and_the_figures_at_the_right_edge() {
        let status = Status {
            window: Some(200_000),
            context: 38_000,
            prompt: 38_000,
            cached: 30_000,
            completion: 2_000,
            branch: Some("main".into()),
            ..Status::default()
        };
        let wide = unstyled(&status_layout("qwen3.7-plus", &status, 120).unwrap());
        assert_eq!(text::width(&wide), 120);
        assert!(wide.starts_with("qwen3.7-plus  ") && wide.ends_with("⎇ main"));
        assert!(wide.contains("   ctx ") && wide.contains("   38k in · 78% cached · 2k out   "), "{wide}");
        let tight = unstyled(&status_layout("qwen3.7-plus", &status, 62).unwrap());
        assert!(text::width(&tight) <= 62 && tight.ends_with("main") && !tight.contains("cached"), "{tight}");
    }

    #[test]
    fn a_task_list_update_gets_no_row_unless_it_failed() {
        assert!(is_bookkeeping("todo_write", false));
        assert!(!is_bookkeeping("todo_write", true));
        assert!(!is_bookkeeping("todo", false));
        assert!(!is_bookkeeping("bash cargo test", false));
    }

    #[test]
    fn bash_rows_lead_with_a_faint_dollar_instead_of_the_word() {
        assert_eq!(tool_text("bash cargo test"), "$ cargo test");
        assert_eq!(tool_text("bash [src] ls"), "$ [src] ls");
        assert_eq!(tool_text("bash"), "$");
        assert_eq!(tool_text("bashful x"), "bashful x");
        assert_eq!(tool_text("read src/lib.rs"), "read src/lib.rs");
        assert_eq!(tool_summary(&tool_text("bash cargo test")), format!("{FAINT}${RESET} cargo test"));
        assert_eq!(tool_summary(&tool_text("bash")), format!("{FAINT}${RESET}"));
    }

    #[test]
    fn the_empty_prompt_hints_at_commands_and_steering_unless_something_else_waits() {
        assert_eq!(placeholder(true, false, false), Some("ask, / for commands and skills, @ for files"));
        assert!(placeholder(true, true, false).unwrap().contains("/btw"));
        assert!(placeholder(true, true, false).unwrap().contains("Ctrl+S"));
        assert_eq!(placeholder(false, false, false), None);
        assert_eq!(placeholder(true, false, true), None);
    }

    #[test]
    fn a_queued_message_says_ctrl_s_sends_it_now() {
        assert_eq!(unstyled(&queued_row("fix\nthis")), "❯ fix this (sent at the next step · Ctrl+S sends it now)");
    }

    #[test]
    fn the_working_rows_name_the_action_and_put_the_time_with_the_hint_below() {
        let secs = Duration::from_secs;
        let rows: Vec<String> = working_rows(20, secs(45), "thinking", "The internal § references").iter().map(|row| unstyled(row)).collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].ends_with("thinking The internal § references"), "{}", rows[0]);
        assert_eq!(rows[1], "45s · Esc to interrupt", "{}", rows[1]);
    }

    #[test]
    fn the_snapshot_word_advances_one_per_turn() {
        assert_eq!(snapshot_word(0, 0), "chiaroscuro");
        assert_eq!(snapshot_word(1, 0), "contrapposto", "each snapshot starts on the next word");
        assert_eq!(snapshot_word(2, 50), "illuminating");
        assert_eq!(snapshot_word(4, 0), "chiaroscuro", "the words cycle");
        assert_eq!(snapshot_word(0, 99), "chiaroscuro", "a word holds for a hundred frames");
        assert_eq!(snapshot_word(0, 100), "contrapposto", "only a very long snapshot advances on time");
        assert!((0..4).map(|seq| snapshot_word(seq, 0)).all(|word| word.len() >= 11), "long spellings only");
    }

    #[test]
    fn the_snapshot_rows_show_the_whole_word_at_once() {
        let secs = Duration::from_secs;
        let rows: Vec<String> = snapshot_rows(1, 8, secs(3)).iter().map(|row| unstyled(row)).collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].ends_with("contrapposto"), "{}", rows[0]);
        assert!(!rows[0].contains('·'), "no letter-by-letter placeholders: {}", rows[0]);
        assert_eq!(rows[1], "3s · Esc to interrupt", "{}", rows[1]);
    }

    #[test]
    fn the_working_rows_leave_out_an_empty_detail() {
        let secs = Duration::from_secs;
        let rows: Vec<String> = working_rows(20, secs(45), "writing", "").iter().map(|row| unstyled(row)).collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].ends_with("writing"), "{}", rows[0]);
        assert!(rows[1].ends_with("45s · Esc to interrupt"), "{}", rows[1]);
    }

    #[test]
    fn the_context_meter_turns_warn_at_80_percent() {
        assert_eq!(unstyled(&gauge(30, 100)), "━━━╌╌╌╌╌╌╌");
        assert!(gauge(79, 100).contains(&LAGOON.to_string()));
        assert!(gauge(80, 100).contains(&WARN.to_string()));
    }

    #[test]
    fn durations_move_up_a_unit() {
        let secs = Duration::from_secs;
        assert_eq!(clock(secs(0)), "0s");
        assert_eq!(clock(secs(59)), "59s");
        assert_eq!(clock(secs(60)), "1m00s");
        assert_eq!(clock(secs(125)), "2m05s");
        assert_eq!(clock(secs(3_600)), "1h00m");
        assert_eq!(clock(secs(3_725)), "1h02m");
        assert_eq!(precise_clock(Duration::from_millis(2_400)), "2.4s");
        assert_eq!(precise_clock(secs(70)), "1m10s");
    }

    #[test]
    fn short_numbers() {
        assert_eq!(short(950), "950");
        assert_eq!(short(38_000), "38k");
        assert_eq!(short(1_250_000), "1.2M");
    }

    #[test]
    fn a_side_answer_is_one_block_hung_off_the_gutter() {
        let rows = aside_rows("btw", "what\nnow?", "It **reads** first.\n\n- one\n- two\n", "Not kept · 4k in", 40);
        let rows: Vec<String> = rows.iter().map(|row| unstyled(row)).collect();
        assert_eq!(rows, ["btw what now?", "  │ It reads first.", "  │", "  │ • one", "  │ • two", "  │", "  │ Not kept · 4k in"]);
        let long = aside_rows("btw", "q", &"word ".repeat(30), "a note too long for one row of thirty", 30);
        assert!(long.iter().all(|row| text::width(row) <= 30));
        assert!(long.iter().skip(1).all(|row| unstyled(row).starts_with("  │")), "the note stays inside the gutter too");
        let told = aside_rows("session 7 says", "", "pushed", "For the agent's next step", 40);
        assert_eq!(told.iter().map(|row| unstyled(row)).collect::<Vec<_>>(), ["session 7 says", "  │ pushed", "  │", "  │ For the agent's next step"]);
    }

    #[test]
    fn a_recap_is_a_labelled_block_that_fits_the_width() {
        let rows = recap_rows("Fixed the repeat breaker.", "12k in, 98% cached", 60);
        assert_eq!(rows.iter().map(|row| unstyled(row)).collect::<Vec<_>>(), ["recap Fixed the repeat breaker. · 12k in, 98% cached"]);
        let chinese = recap_rows(&"已修复重复调用熔断器，并让会话脚本按正确顺序配对。".repeat(3), "12k in", 30);
        assert!(chinese.len() > 1 && chinese.iter().all(|row| text::width(row) <= 30), "{chinese:?}");
        assert!(chinese.iter().skip(1).all(|row| unstyled(row).starts_with("      ")));
    }

    #[test]
    fn each_kind_of_status_figure_has_its_own_color() {
        let status = Status {
            window: Some(200_000),
            context: 38_000,
            prompt: 38_000,
            cached: 30_000,
            completion: 2_000,
            branch: Some("main*".into()),
            ..Status::default()
        };
        let line = status_layout(&format!("model · effort {}", tint(EFFORT, "high")), &status, 140).unwrap();
        assert!(line.contains(&format!("{EFFORT}high")));
        assert!(line.contains(&format!("{TOKENS}38k")) && line.contains(&format!("{TOKENS}78%")) && line.contains(&format!("{TOKENS}2k")));
        assert!(line.contains(&format!("{BRANCH}⎇ main")) && line.contains(&format!("{WARN}*")));
        assert!(!line.contains('│'), "groups are told apart by space, not by rules");
        assert!(!line.contains(&format!("{WARN}38k/200k")));
        let full = status_layout("model", &Status { context: 170_000, ..status }, 140).unwrap();
        assert!(full.contains(&format!("{WARN}170k/200k")));
    }

    #[test]
    fn a_tick_repaints_only_the_rows_that_changed() {
        let rows = |texts: &[&str]| texts.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        let before = rows(&["working 1s", "rule", "❯", "rule", "status"]);
        assert_eq!(changed_rows(&before, &rows(&["working 2s", "rule", "❯", "rule", "status"])), Some(vec![0]));
        assert_eq!(changed_rows(&before, &before), Some(vec![]));
        assert_eq!(changed_rows(&before, &rows(&["rule", "❯", "rule", "status"])), None);
    }

    #[test]
    fn first_sentence_stops_at_the_end_of_a_sentence() {
        assert_eq!(first_sentence("The bug is in pop. Then fix it."), "The bug is in pop.");
        assert_eq!(first_sentence("先看代码。再修复。"), "先看代码。");
    }

    #[test]
    fn ask_options_wrap_instead_of_truncating() {
        let names = vec![
            "1. Move both `subagent_call` and `subworkflow_call` from Logic to AI (smallest diff, 1 file)".to_string(),
            "2. Something else - tell me what you meant.".to_string(),
        ];
        let rows = ask_rows(&names, 0, 2, 40);
        assert!(rows.len() > 2, "{rows:?}");
        assert!(rows.iter().all(|row| text::width(row) <= 40), "{rows:?}");
        let plain = rows.iter().map(|row| unstyled(row)).collect::<Vec<_>>().join("\n");
        assert!(plain.contains("subworkflow_call"), "{plain:?}");
        assert!(!plain.contains('…'), "{plain:?}");
        assert_eq!(text::width(&rows[0]), 40, "the selected block runs to the window's edge");
    }

    #[test]
    fn a_question_of_three_lines_renders_three_rows() {
        let rows: Vec<String> = question_rows("first line\nsecond line\nthird one", 80).iter().map(|row| unstyled(row)).collect();
        assert_eq!(rows, ["? first line", "second line", "third one"]);
        let evil = question_rows("a\x1b[2Jb\nc\td", 80);
        assert_eq!(evil.iter().map(|row| unstyled(row)).collect::<Vec<_>>(), ["? ab", "c   d"]);
        let wrapped = question_rows("one two three four five six seven", 12);
        assert!(wrapped.len() > 1 && wrapped.iter().all(|row| text::width(row) <= 12), "{wrapped:?}");
    }
