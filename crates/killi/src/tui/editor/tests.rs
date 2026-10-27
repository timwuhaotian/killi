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
    assert_eq!((e.text(), e.cursor), ("hello there world", 11), "up stays in the text instead of recalling history");
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
    assert_eq!(e.text(), "earlier", "up from the top visual row recalls history");
    e.down();
    assert_eq!(e.text(), "hello there world", "down from the recalled entry returns the draft");
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
    assert_eq!(images, vec![Image { label: "[Image #1]".into(), url: "data:image/png;base64,AA".into() }]);
    e.attach("data:image/png;base64,CC".into());
    assert_eq!(e.text(), "[Image #3] ", "numbers aren't used twice");
    e.clear();
    e.insert("[Image #3]");
    assert!(e.take_with_images().1.is_empty(), "clearing drops the image");
}

#[test]
fn render_wraps_and_places_the_cursor() {
    let mut e = Editor::default();
    e.insert("abcdef\n中文");
    let (rows, row, col) = e.render("› ", 6, &|_| false);
    assert_eq!(rows, vec!["› abcd", "  ef", "  中文", "  "], "a full row moves the cursor to the next one");
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
    assert_eq!((e.render("› ", 12, &|_| false).1, e.render("› ", 12, &|_| false).2), (0, 2));
    for _ in 0..6 {
        e.right();
    }
    assert_eq!((e.render("› ", 12, &|_| false).1, e.render("› ", 12, &|_| false).2), (1, 2), "the cursor on a word that moved down is on its row");
    let mut e = Editor::default();
    e.insert("a extraordinarily b");
    let (rows, ..) = e.render("", 8, &|_| false);
    assert_eq!(rows, vec!["a ", "extraord", "inarily ", "b"], "a word longer than a row is still split");
    let mut e = Editor::default();
    e.insert("abcdefgh ij");
    let (rows, row, col) = e.render("", 8, &|_| false);
    assert_eq!(rows, vec!["abcdefgh", "ij"], "a space past the edge hangs instead of starting a row");
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
    assert_eq!(rows, ["a   bc"], "the tab takes the columns to the next 4-column stop");
    assert_eq!((row, col), (0, 6));
    e.home();
    e.right();
    assert_eq!(e.render("", 20, &|_| false).2, 1, "the cursor after the first character");
    e.right();
    assert_eq!(e.render("", 20, &|_| false).2, 4, "and just past the tab, where its spaces end");
    e.right();
    assert_eq!(e.render("", 20, &|_| false).2, 5, "and before the character after it");
    let mut e = Editor::default();
    e.insert("x\n\ty");
    let (rows, ..) = e.render("", 20, &|_| false);
    assert_eq!(rows, ["x", "    y"], "a line's stops count from its own first column");
}
