use super::*;

#[test]
fn edits_and_moves_by_words() {
    let mut e = Editor::default();
    e.insert("hello brave world");
    e.word_left();
    e.delete_word();
    assert_eq!(e.text(), "hello world");
    e.end();
    e.backspace();
    assert_eq!(e.text(), "hello worl");
    e.home();
    e.kill_to_end();
    assert!(e.is_empty());
}

#[test]
fn a_quiet_take_leaves_no_history() {
    let mut e = Editor::default();
    e.insert("first");
    e.take();
    e.insert("sk-secret");
    assert_eq!(e.take_quiet(), "sk-secret");
    e.up();
    assert_eq!(e.text(), "first", "the secret was never history");
}

#[test]
fn history_comes_back_with_the_draft() {
    let mut e = Editor::with_history(vec!["first".into(), "second".into()]);
    e.insert("draft");
    e.up();
    assert_eq!(e.text(), "second");
    e.up();
    assert_eq!(e.text(), "first");
    e.down();
    e.down();
    assert_eq!(e.text(), "draft");
}

#[test]
fn up_and_down_move_between_visual_rows_of_a_wrapped_line() {
    let mut e = Editor::with_history(vec!["earlier".into()]);
    e.insert("hello there world");
    e.render("› ", 12, &|_| false);
    assert_eq!(e.cursor, 17);
    e.up();
    assert_eq!(
        (e.text(), e.cursor),
        ("hello there world", 11),
        "up stays in the text instead of recalling history"
    );
    e.up();
    assert_eq!((e.text(), e.cursor), ("hello there world", 5));
    e.down();
    assert_eq!((e.text(), e.cursor), ("hello there world", 11));
    e.down();
    assert_eq!((e.text(), e.cursor), ("hello there world", 17));
    e.up();
    e.up();
    assert_eq!(e.cursor, 5);
    e.up();
    assert_eq!(
        e.text(),
        "earlier",
        "up from the top visual row recalls history"
    );
    e.down();
    assert_eq!(
        e.text(),
        "hello there world",
        "down from the recalled entry returns the draft"
    );
}

#[test]
fn up_and_down_still_move_between_lines_after_a_render() {
    let mut e = Editor::default();
    e.insert("hello\nworld!");
    e.render("", 20, &|_| false);
    e.up();
    assert_eq!(e.cursor, 5);
    e.down();
    assert_eq!(e.cursor, 11);
    let mut e = Editor::default();
    e.insert("ab cd\nef gh");
    e.render("", 3, &|_| false);
    assert_eq!(e.cursor, 11);
    e.up();
    assert_eq!(e.cursor, 8, "up crosses into the wrapped row above");
    e.up();
    assert_eq!(e.cursor, 5, "up reaches the end of the line above");
    e.up();
    assert_eq!(e.cursor, 2);
    e.down();
    assert_eq!(e.cursor, 5);
    e.down();
    assert_eq!(e.cursor, 8);
    e.down();
    assert_eq!(e.cursor, 11);
}

#[test]
fn history_draft_round_trip_after_a_render() {
    let mut e = Editor::with_history(vec!["first".into(), "second".into()]);
    e.insert("draft");
    e.render("", 20, &|_| false);
    e.up();
    assert_eq!(e.text(), "second");
    e.up();
    assert_eq!(e.text(), "first");
    e.down();
    assert_eq!(e.text(), "second");
    e.down();
    assert_eq!(e.text(), "draft");
}

#[test]
fn pasted_images_go_with_the_placeholders_left_in() {
    let mut e = Editor::default();
    e.insert("what is");
    e.attach("data:image/png;base64,AA".into());
    e.attach("data:image/png;base64,BB".into());
    assert_eq!(e.text(), "what is [Image #1] [Image #2] ");
    e.backspace();
    e.delete_word();
    let (text, images) = e.take_with_images();
    assert_eq!(text, "what is [Image #1] [Image #");
    assert_eq!(
        images,
        vec![Image {
            label: "[Image #1]".into(),
            url: "data:image/png;base64,AA".into()
        }]
    );
    e.attach("data:image/png;base64,CC".into());
    assert_eq!(e.text(), "[Image #3] ", "numbers aren't used twice");
    e.clear();
    e.insert("[Image #3]");
    assert!(
        e.take_with_images().1.is_empty(),
        "clearing drops the image"
    );
}

#[test]
fn render_wraps_and_places_the_cursor() {
    let mut e = Editor::default();
    e.insert("abcdef\n中文");
    let (rows, row, col) = e.render("› ", 6, &|_| false);
    assert_eq!(
        rows,
        vec!["› abcd", "  ef", "  中文", "  "],
        "a full row moves the cursor to the next one"
    );
    assert_eq!((row, col), (3, 2));
    e.home();
    assert_eq!(e.render("› ", 6, &|_| false).1, 2);
    assert_eq!(e.render("› ", 6, &|_| false).2, 2);
}

#[test]
fn render_breaks_rows_between_words() {
    let mut e = Editor::default();
    e.insert("hello there world");
    let (rows, row, col) = e.render("› ", 12, &|_| false);
    assert_eq!(rows, vec!["› hello ", "  there ", "  world"]);
    assert_eq!((row, col), (2, 7));
    e.home();
    assert_eq!(
        (
            e.render("› ", 12, &|_| false).1,
            e.render("› ", 12, &|_| false).2
        ),
        (0, 2)
    );
    for _ in 0..6 {
        e.right();
    }
    assert_eq!(
        (
            e.render("› ", 12, &|_| false).1,
            e.render("› ", 12, &|_| false).2
        ),
        (1, 2),
        "the cursor on a word that moved down is on its row"
    );
    let mut e = Editor::default();
    e.insert("a extraordinarily b");
    let (rows, ..) = e.render("", 8, &|_| false);
    assert_eq!(
        rows,
        vec!["a ", "extraord", "inarily ", "b"],
        "a word longer than a row is still split"
    );
    let mut e = Editor::default();
    e.insert("abcdefgh ij");
    let (rows, row, col) = e.render("", 8, &|_| false);
    assert_eq!(
        rows,
        vec!["abcdefgh", "ij"],
        "a space past the edge hangs instead of starting a row"
    );
    assert_eq!((row, col), (1, 2));
}

#[test]
fn known_slash_words_are_colored() {
    let mut e = Editor::default();
    e.insert("a /pdf /nope b/pdf\n/pdf");
    let (rows, ..) = e.render("", 40, &|name| name == "pdf");
    assert_eq!(rows[0], format!("a {LAGOON}/pdf{RESET} /nope b/pdf"));
    assert_eq!(rows[1], format!("{LAGOON}/pdf{RESET}"));
    let (rows, ..) = e.render("", 7, &|name| name == "pdf");
    assert_eq!(rows[0], format!("a {LAGOON}/pdf{RESET} "));
}

#[test]
fn a_tab_lays_out_to_the_next_stop_with_the_cursor_on_it() {
    let mut e = Editor::default();
    e.insert("a\tbc");
    assert_eq!(e.text(), "a\tbc", "the text keeps its tab");
    let (rows, row, col) = e.render("", 20, &|_| false);
    assert_eq!(
        rows,
        ["a   bc"],
        "the tab takes the columns to the next 4-column stop"
    );
    assert_eq!((row, col), (0, 6));
    e.home();
    e.right();
    assert_eq!(
        e.render("", 20, &|_| false).2,
        1,
        "the cursor after the first character"
    );
    e.right();
    assert_eq!(
        e.render("", 20, &|_| false).2,
        4,
        "and just past the tab, where its spaces end"
    );
    e.right();
    assert_eq!(
        e.render("", 20, &|_| false).2,
        5,
        "and before the character after it"
    );
    let mut e = Editor::default();
    e.insert("x\n\ty");
    let (rows, ..) = e.render("", 20, &|_| false);
    assert_eq!(
        rows,
        ["x", "    y"],
        "a line's stops count from its own first column"
    );
}

#[test]
fn shift_moves_select_and_typing_replaces_the_selection() {
    let mut e = Editor::default();
    e.insert("hello brave world");
    e.select(Editor::word_left);
    assert_eq!(e.selected_text(), Some("world"));
    e.select(Editor::word_left);
    assert_eq!(e.selected_text(), Some("brave world"));
    e.select(Editor::right);
    assert_eq!(
        e.selected_text(),
        Some("rave world"),
        "the anchor stays put"
    );
    e.insert("new");
    assert_eq!(e.text(), "hello bnew");
    assert_eq!(e.selection(), None);

    e.select(Editor::home);
    e.backspace();
    assert!(e.is_empty(), "Backspace takes the selection out");
}

#[test]
fn a_plain_move_ends_the_selection_at_its_edge() {
    let mut e = Editor::default();
    e.insert("one two three");
    e.word_left();
    e.select(Editor::word_left);
    e.left();
    assert_eq!((e.selection(), e.before_cursor()), (None, "one "));
    e.select(Editor::word_right);
    e.right();
    assert_eq!((e.selection(), e.before_cursor()), (None, "one two"));
}

#[test]
fn select_all_cuts_whole_and_shift_up_stays_in_the_text() {
    let mut e = Editor::with_history(vec!["older".into()]);
    e.insert("line one\nline two");
    e.select(Editor::up_within);
    e.select(Editor::up_within);
    assert_eq!(
        e.text(),
        "line one\nline two",
        "Shift+Up never recalls history"
    );
    assert_eq!(e.selected_text(), Some("line one\nline two"));
    e.end();
    e.select_all();
    assert_eq!(e.cut().as_deref(), Some("line one\nline two"));
    assert!(e.is_empty());
    assert_eq!(e.cut(), None);
}

#[test]
fn the_mouse_presses_drags_and_picks_words_and_rows() {
    let mut e = Editor::default();
    e.insert("fix the slug_test now");
    e.render("❯ ", 12, &|_| false);
    // Rows at 10 columns: "fix the " and "slug_test " and "now".
    let at = e.point(1, 2);
    assert_eq!(&e.text()[at..], "ug_test now");
    e.press(at);
    assert_eq!(e.selection(), None, "a press selects nothing");
    e.drag_to(e.point(2, 1));
    assert_eq!(e.selected_text(), Some("ug_test n"));

    e.select_word(e.point(1, 4));
    assert_eq!(e.selected_text(), Some("slug_test"));
    e.select_word(e.point(0, 3));
    assert_eq!(e.selection(), None, "blank space is no word");
    e.select_row(0);
    assert_eq!(e.selected_text(), Some("fix the "));
    assert_eq!(
        e.point(9, 40),
        e.text().len(),
        "below the last row is its end"
    );
}

#[test]
fn the_selection_is_drawn_in_reverse_video_across_a_lit_command() {
    let mut e = Editor::default();
    e.insert("/help me");
    e.select(Editor::word_left);
    e.select(Editor::word_left);
    let (rows, _, _) = e.render("> ", 40, &|name| name == "help");
    let lagoon = LAGOON.to_string();
    assert_eq!(
        rows[0],
        format!("> {lagoon}/{RESET}{lagoon}{REVERSE}help{RESET}{REVERSE} me{RESET}"),
        "the selection starts after the slash and its reverse video outlasts the command's reset"
    );
}
