use crossterm::event::KeyModifiers;

use super::*;
use crate::tui::tests::test_app;

fn mouse(kind: MouseEventKind, row: u16, column: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn press(row: u16, column: u16) -> MouseEvent {
    mouse(MouseEventKind::Down(MouseButton::Left), row, column)
}

fn drag(row: u16, column: u16) -> MouseEvent {
    mouse(MouseEventKind::Drag(MouseButton::Left), row, column)
}

/// A fullscreen app whose conversation is `rows` rows, `view` of them on screen, following
/// the newest.
fn app_with_rows(rows: usize, view: usize) -> App {
    let mut app = test_app();
    app.fullscreen = true;
    app.view_rows = view;
    (0..rows).for_each(|n| app.history.push(Line::Row(format!("row {n}"))));
    app
}

/// `app_with_rows(30, 10)` with `text` in the prompt, laid out 10 columns wide after its
/// `❯ ` and drawn on screen rows 12 to 14.
fn app_with_prompt(text: &str) -> App {
    let mut app = app_with_rows(30, 10);
    app.editor.insert(text);
    app.editor.render("❯ ", 12, &|_| false);
    app.input_rows = InputRows {
        top: 12,
        first: 0,
        count: 3,
        indent: 2,
    };
    app
}

#[test]
fn a_drag_in_the_prompt_selects_its_text_and_release_copies_it() {
    // Rows "fix the ", "slug_test " and "now".
    let mut app = app_with_prompt("fix the slug_test now");
    app.on_mouse(press(12, 6));
    app.on_mouse(drag(13, 6));
    assert_eq!(app.release().as_deref(), Some("the slug"));
    assert_eq!(
        app.editor.selected_text(),
        Some("the slug"),
        "it stays selected"
    );
    assert!(app.selection.is_none(), "the conversation has none");

    app.on_mouse(press(12, 2));
    app.on_mouse(drag(5, 40));
    assert_eq!(
        app.release().as_deref(),
        Some("fix the "),
        "a drag out of the prompt stays on its rows"
    );
}

#[test]
fn double_and_triple_clicks_in_the_prompt_take_a_word_and_a_row() {
    let mut app = app_with_prompt("fix the slug_test now");
    (0..2).for_each(|_| app.on_mouse(press(13, 4)));
    assert_eq!(app.release().as_deref(), Some("slug_test"));
    app.on_mouse(press(13, 4));
    assert_eq!(app.release().as_deref(), Some("slug_test "));
    app.on_mouse(press(12, 3));
    assert_eq!(app.release(), None, "a press elsewhere starts over");
    assert_eq!(app.editor.before_cursor(), "f", "and puts the cursor down");
}

#[test]
fn one_selection_at_a_time() {
    let mut app = app_with_prompt("fix the slug_test now");
    app.on_mouse(press(2, 0));
    app.on_mouse(drag(4, 3));
    app.on_mouse(press(12, 2));
    app.on_mouse(drag(12, 5));
    assert!(app.selection.is_none());
    assert_eq!(app.release().as_deref(), Some("fix"));
    app.on_mouse(press(2, 0));
    assert_eq!(app.editor.selection(), None);
}

#[test]
fn the_prompt_rows_move_with_the_rows_above_and_the_rows_cut() {
    let rows = InputRows {
        top: 3,
        first: 0,
        count: 4,
        indent: 2,
    };
    let placed = rows.placed(10, 0);
    assert_eq!((placed.top, placed.first, placed.count), (13, 0, 4));
    let placed = rows.placed(10, 5);
    assert_eq!((placed.top, placed.first, placed.count), (10, 2, 2));
}

#[test]
fn highlight_reverses_exactly_the_selected_columns() {
    assert_eq!(highlight("abcdef", 2, 4), "ab\x1b[7mcde\x1b[27mf");
    assert_eq!(highlight("abc", 0, usize::MAX), "\x1b[7mabc\x1b[27m");
}

#[test]
fn highlight_keeps_the_reverse_video_across_a_reset_inside_the_range() {
    assert_eq!(
        highlight("\x1b[1mab\x1b[0mcd", 1, 2),
        "\x1b[1ma\x1b[7mb\x1b[0m\x1b[7mc\x1b[27md"
    );
}

#[test]
fn a_wide_character_is_selected_whole_by_either_of_its_columns() {
    assert_eq!(highlight("中文ab", 1, 2), "\x1b[7m中文\x1b[27mab");
    assert_eq!(columns_text("中文ab", 1, 1), "中");
    assert_eq!(columns_text("中文ab", 3, 4), "文a");
}

#[test]
fn columns_text_leaves_out_every_escape_sequence() {
    assert_eq!(columns_text("\x1b[1mhello\x1b[0m world", 0, 6), "hello w");
}

#[test]
fn the_columns_of_a_selection_run_from_its_first_point_to_its_last_either_way() {
    let forward = Selection {
        anchor: (1, 2),
        head: (3, 5),
        epoch: 0,
    };
    let backward = Selection {
        anchor: (3, 5),
        head: (1, 2),
        epoch: 0,
    };
    for selection in [forward, backward] {
        assert_eq!(selection.columns_on(0), None);
        assert_eq!(selection.columns_on(1), Some((2, usize::MAX)));
        assert_eq!(selection.columns_on(2), Some((0, usize::MAX)));
        assert_eq!(selection.columns_on(3), Some((0, 5)));
        assert_eq!(selection.columns_on(4), None);
    }
    assert!(Selection::at((1, 1), 0).is_empty());
}

#[test]
fn a_double_click_selects_the_word_under_it_and_release_copies_it() {
    let mut app = app_with_rows(30, 10);
    app.on_mouse(press(2, 1));
    app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, 1));
    assert!(app.selection.is_none(), "a single press selects nothing");
    app.on_mouse(press(2, 1));
    assert_eq!(app.release().as_deref(), Some("row"));
    assert!(
        app.selection.is_some(),
        "the word stays selected after the copy"
    );
}

#[test]
fn a_second_press_too_late_or_elsewhere_starts_an_empty_selection() {
    let mut app = app_with_rows(30, 10);
    app.on_mouse(press(2, 1));
    app.pressed.as_mut().unwrap().1 -= DOUBLE_CLICK + Duration::from_millis(1);
    app.on_mouse(press(2, 1));
    assert_eq!(app.release(), None);

    app.on_mouse(press(2, 1));
    app.pressed = Some((Spot::View((24, 1)), Instant::now(), 1));
    app.on_mouse(press(2, 1));
    assert_eq!(
        app.release(),
        None,
        "a press on another point is not a double-click"
    );
}

#[test]
fn a_triple_click_selects_the_whole_row() {
    let mut app = app_with_rows(30, 10);
    (0..3).for_each(|_| app.on_mouse(press(2, 1)));
    assert_eq!(app.release().as_deref(), Some("row 22"));

    app.on_mouse(press(2, 1));
    assert_eq!(
        app.release().as_deref(),
        Some("row 22"),
        "a fourth press keeps the row"
    );
    assert!(app.selection.is_some());
}

#[test]
fn a_word_is_letters_digits_and_underscores_and_any_other_character_alone() {
    assert_eq!(word_on("hello world_2!", 1), Some((0, 4)));
    assert_eq!(word_on("hello world_2!", 8), Some((6, 12)));
    assert_eq!(word_on("hello world_2!", 13), Some((13, 13)));
    assert_eq!(word_on("hello world", 5), None);
    assert_eq!(word_on("hello world", 40), None);
    assert_eq!(word_on("\x1b[1m中文\x1b[0m ab", 2), Some((0, 3)));
}

#[test]
fn a_drag_selects_the_rows_it_passes_and_release_copies_them() {
    let mut app = app_with_rows(30, 10);
    app.on_mouse(press(2, 0));
    app.on_mouse(drag(4, 3));
    assert_eq!(app.release().as_deref(), Some("row 22\nrow 23\nrow"));
    assert!(
        app.selection.is_some(),
        "the selection stays on screen after the copy"
    );
    assert!(app.drag.is_none());
}

#[test]
fn a_press_and_release_without_a_move_select_and_copy_nothing() {
    let mut app = app_with_rows(30, 10);
    app.on_mouse(press(2, 0));
    assert_eq!(app.release(), None);
    assert!(app.selection.is_none());
}

#[test]
fn a_press_in_the_live_area_only_ends_the_selection() {
    let mut app = app_with_rows(30, 10);
    app.on_mouse(press(2, 0));
    app.on_mouse(drag(4, 3));
    app.on_mouse(press(12, 0));
    assert!(app.selection.is_none() && app.drag.is_none());
}

#[test]
fn the_wheel_scrolls_and_the_drag_follows_the_pointer_into_the_rows_it_now_shows() {
    let mut app = app_with_rows(30, 10);
    app.on_mouse(press(2, 0));
    app.on_mouse(drag(4, 3));
    app.on_mouse(mouse(MouseEventKind::ScrollUp, 4, 3));
    assert_eq!(app.history.span(10), (17, 27));
    assert_eq!(app.selection.as_ref().map(|s| s.head), Some((21, 3)));
    assert_eq!(
        app.selection.as_ref().map(|s| s.anchor),
        Some((22, 0)),
        "the press stays on its row"
    );
}

#[test]
fn a_drag_held_at_an_edge_scrolls_the_view_on_its_own() {
    let mut app = app_with_rows(30, 10);
    app.on_mouse(press(2, 0));
    app.on_mouse(drag(0, 5));
    assert!(app.edge_scrolling());
    app.edge_scroll();
    assert_eq!(app.history.span(10), (19, 29));
    assert_eq!(app.selection.as_ref().map(|s| s.head), Some((19, 5)));

    app.on_mouse(drag(9, 0));
    assert_eq!(app.edge_rows(), 1, "the last row scrolls down");
    app.on_mouse(drag(5, 0));
    assert!(!app.edge_scrolling(), "the middle does not");
    app.on_mouse(drag(12, 0));
    assert_eq!(app.edge_rows(), 1, "below the view scrolls down too");
}

#[test]
fn a_relayout_drops_the_selection_it_was_made_on() {
    let mut app = app_with_rows(30, 10);
    app.on_mouse(press(2, 0));
    app.on_mouse(drag(4, 3));
    app.history.set_width(40);
    app.on_mouse(drag(5, 3));
    assert!(app.selection.is_none() && app.drag.is_none());
}

#[test]
fn the_selected_rows_are_painted_in_reverse_video() {
    let mut app = app_with_rows(30, 10);
    app.on_mouse(press(2, 1));
    app.on_mouse(drag(3, 2));
    let lines = app.view_lines(10);
    assert_eq!(lines.len(), 10);
    assert!(!lines[1].contains(REVERSE));
    assert_eq!(lines[2], "r\x1b[7mow 22\x1b[27m");
    assert_eq!(lines[3], "\x1b[7mrow\x1b[27m 23");
    assert!(!lines[4].contains(REVERSE));
}

#[test]
fn a_key_press_ends_the_selection() {
    let mut app = app_with_rows(30, 10);
    app.on_mouse(press(2, 0));
    app.on_mouse(drag(4, 3));
    app.clear_selection();
    assert!(app.selection.is_none() && app.drag.is_none());
}
