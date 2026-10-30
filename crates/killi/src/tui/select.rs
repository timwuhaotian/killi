//! Text selection in fullscreen mode. killi asks the terminal for button reports (1002), so the
//! selection is killi's own: its points are rows of the conversation rather than screen cells, so
//! it stays on its words as the view scrolls, and it can span more than one screen. Releasing the
//! button copies it, and a second press on the same point soon after the first selects the word
//! under it, a third the whole row. The wheel still scrolls, and a drag held at an edge of the
//! view scrolls it.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use unicode_width::UnicodeWidthChar;

use super::*;

/// How close together two presses on the same point land to count as a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// Reverse video on and off, around the selected columns.
const REVERSE: &str = "\x1b[7m";
const NOT_REVERSE: &str = "\x1b[27m";

/// A selection in the conversation, from where the button went down to where the pointer is. A
/// point is (a row of `History`, a display column of that row). `epoch` is the layout the rows
/// were numbered in: when the conversation is laid out again its rows move, so the selection goes.
pub(super) struct Selection {
    anchor: (usize, usize),
    head: (usize, usize),
    epoch: u64,
}

impl Selection {
    /// Where the press landed, before the pointer has moved.
    fn at(point: (usize, usize), epoch: u64) -> Self {
        Self {
            anchor: point,
            head: point,
            epoch,
        }
    }

    fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The first and the last point, in conversation order.
    fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// The columns of `row` the selection covers, both ends included; None for rows outside it.
    fn columns_on(&self, row: usize) -> Option<(usize, usize)> {
        let ((first, from), (last, to)) = self.ordered();
        if row < first || row > last {
            return None;
        }
        let from = if row == first { from } else { 0 };
        let to = if row == last { to } else { usize::MAX };
        Some((from, to))
    }
}

/// A styled row split into its escape sequences and its characters, each character with the
/// column it starts at and the columns it takes.
enum Piece<'a> {
    Escape(&'a str),
    Char {
        text: &'a str,
        col: usize,
        width: usize,
    },
}

fn pieces(line: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut col = 0;
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        if c == '\x1b' {
            let len = escape_len(rest);
            out.push(Piece::Escape(&rest[..len]));
            rest = &rest[len..];
        } else {
            let width = c.width().unwrap_or(0);
            out.push(Piece::Char {
                text: &rest[..c.len_utf8()],
                col,
                width,
            });
            col += width;
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// The length of the escape sequence `text` starts with: a CSI runs to its final byte, anything
/// else is the escape and the character after it.
fn escape_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    if bytes.get(1) == Some(&b'[') {
        bytes[2..]
            .iter()
            .position(|b| (0x40..=0x7e).contains(b))
            .map_or(bytes.len(), |end| end + 3)
    } else {
        1 + text[1..].chars().next().map_or(0, char::len_utf8)
    }
}

/// Whether a character `width` columns wide at `col` lies within the columns `from` to `to`,
/// both included. A wide character is taken whole when either of its columns is.
fn covers(col: usize, width: usize, from: usize, to: usize) -> bool {
    col <= to && col + width.max(1) > from
}

/// `line` with the columns `from` to `to` in reverse video; its other styles are kept.
pub(super) fn highlight(line: &str, from: usize, to: usize) -> String {
    let mut out = String::with_capacity(line.len() + 16);
    let mut on = false;
    for piece in pieces(line) {
        match piece {
            Piece::Escape(sequence) => {
                out.push_str(sequence);
                // A reset inside the range would end the reverse video there, so it goes on again.
                if on && (sequence == RESET || sequence == "\x1b[m") {
                    out.push_str(REVERSE);
                }
            }
            Piece::Char { text, col, width } => {
                let inside = covers(col, width, from, to);
                if inside != on {
                    out.push_str(if inside { REVERSE } else { NOT_REVERSE });
                    on = inside;
                }
                out.push_str(text);
            }
        }
    }
    if on {
        out.push_str(NOT_REVERSE);
    }
    out
}

/// The characters of `line` in the columns `from` to `to`, without any escape sequence.
pub(super) fn columns_text(line: &str, from: usize, to: usize) -> String {
    pieces(line)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Char { text, col, width } if covers(col, width, from, to) => Some(text),
            _ => None,
        })
        .collect()
}

/// The columns of the word around column `col` of `line`, both ends included: a run of letters,
/// digits and underscores, or the one character clicked when it is neither. Blank space is None.
fn word_on(line: &str, col: usize) -> Option<(usize, usize)> {
    let chars: Vec<(char, usize, usize)> = pieces(line)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Char { text, col, width } => text.chars().next().map(|c| (c, col, width)),
            _ => None,
        })
        .collect();
    let at = chars
        .iter()
        .position(|&(_, start, width)| covers(start, width, col, col))?;
    if chars[at].0.is_whitespace() {
        return None;
    }
    // A character with no width of its own, like a combining mark, belongs to its neighbor's word.
    let joins = |(c, _, width): (char, usize, usize)| width == 0 || c.is_alphanumeric() || c == '_';
    let mut first = at;
    let mut last = at;
    if joins(chars[at]) {
        while first > 0 && joins(chars[first - 1]) {
            first -= 1;
        }
        while last + 1 < chars.len() && joins(chars[last + 1]) {
            last += 1;
        }
    }
    Some((chars[first].1, chars[last].1 + chars[last].2.max(1) - 1))
}

impl App {
    /// Mouse reports, which come only in fullscreen mode: the wheel scrolls the conversation, and
    /// a left-button drag that starts in it selects text, copied when the button is let go.
    pub(super) fn on_mouse(&mut self, mouse: MouseEvent) {
        self.forget_stale();
        let pointer = (usize::from(mouse.row), usize::from(mouse.column));
        match mouse.kind {
            MouseEventKind::ScrollUp => self.wheel(-3),
            MouseEventKind::ScrollDown => self.wheel(3),
            MouseEventKind::Down(MouseButton::Left) => self.press(pointer),
            MouseEventKind::Drag(MouseButton::Left) => self.drag_to(pointer),
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(text) = self.release() {
                    copy_to_clipboard(&text);
                }
            }
            _ => {}
        }
    }

    /// Ends the selection: a key press returns the view to the newest rows, which it was not
    /// meant to select, and input between presses ends their run of double- and triple-clicks.
    pub(super) fn clear_selection(&mut self) {
        self.selection = None;
        self.drag = None;
        self.pressed = None;
    }

    /// Scrolls the conversation by `rows`, and a drag in progress follows the pointer into what
    /// the view shows now.
    fn wheel(&mut self, rows: isize) {
        self.scroll(rows);
        self.extend_drag();
    }

    /// A press in the conversation starts a selection there, a second one on the same point soon
    /// after selects the word under it and a third the whole row; anywhere else a press only ends
    /// the last selection.
    fn press(&mut self, (row, col): (usize, usize)) {
        let point = (row < self.view_rows)
            .then(|| self.point_at(row, col))
            .flatten();
        let run = point
            .zip(self.pressed)
            .and_then(|(point, (last, at, n))| {
                (last == point && at.elapsed() < DOUBLE_CLICK).then(|| (n + 1).min(3))
            })
            .unwrap_or(1);
        self.clear_selection();
        self.pressed = point.map(|point| (point, Instant::now(), run));
        let Some(point) = point else { return };
        self.drag = Some((row, col));
        let span = if run >= 3 {
            Some((0, usize::MAX))
        } else if run == 2 {
            self.history
                .row(point.0)
                .and_then(|line| word_on(line, point.1))
        } else {
            None
        };
        self.selection = Some(match span {
            Some((from, to)) => Selection {
                anchor: (point.0, from),
                head: (point.0, to),
                epoch: self.history.epoch(),
            },
            None => Selection::at(point, self.history.epoch()),
        });
    }

    /// The pointer moved with the button held: the selection's head goes to it.
    fn drag_to(&mut self, pointer: (usize, usize)) {
        if self.drag.is_some() {
            self.drag = Some(pointer);
            self.extend_drag();
        }
    }

    /// Moves the selection's head to the drag's pointer, as the view shows it now.
    fn extend_drag(&mut self) {
        let Some((row, col)) = self.drag else { return };
        if let (Some(head), Some(selection)) = (self.point_at(row, col), self.selection.as_mut()) {
            selection.head = head;
        }
    }

    /// The button let go: the drag ends, and the text it selected comes back to be copied. A press
    /// and release without a move select nothing.
    fn release(&mut self) -> Option<String> {
        self.drag.take()?;
        if self.selection.as_ref()?.is_empty() {
            self.selection = None;
            return None;
        }
        self.selected_text()
    }

    /// The selected text: each row's selected columns, trailing spaces dropped, one row per line.
    fn selected_text(&self) -> Option<String> {
        let selection = self.selection.as_ref()?;
        let ((first, _), (last, _)) = selection.ordered();
        let text = (first..=last)
            .filter_map(|row| {
                let (from, to) = selection.columns_on(row)?;
                Some(
                    columns_text(self.history.row(row)?, from, to)
                        .trim_end()
                        .to_string(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    /// The conversation point under a screen cell of the view. A row below the view counts as its
    /// last row, and so does a row past the end of a short conversation.
    fn point_at(&self, row: usize, col: usize) -> Option<(usize, usize)> {
        if self.view_rows == 0 {
            return None;
        }
        let (first, end) = self.history.span(self.view_rows);
        let line = (first + row.min(self.view_rows - 1)).min(end.checked_sub(1)?);
        Some((line, col))
    }

    /// Drops the selection when the conversation was laid out again, since its rows moved.
    pub(super) fn forget_stale(&mut self) {
        if self
            .selection
            .as_ref()
            .is_some_and(|selection| selection.epoch != self.history.epoch())
        {
            self.clear_selection();
        }
    }

    /// Whether a drag holds the pointer at an edge of the view, so that the view should keep
    /// scrolling until it lets go or moves away.
    pub(super) fn edge_scrolling(&self) -> bool {
        self.edge_rows() != 0
    }

    /// One step of edge scrolling: the view moves, and the selection follows the pointer.
    pub(super) fn edge_scroll(&mut self) {
        self.forget_stale();
        let rows = self.edge_rows();
        if rows != 0 {
            self.scroll(rows);
            self.extend_drag();
        }
    }

    /// The rows the view scrolls for a drag at its edge: up on its first row, down on its last row
    /// or below the view. Zero otherwise.
    fn edge_rows(&self) -> isize {
        let Some((row, _)) = self
            .drag
            .filter(|_| self.selection.is_some() && self.view_rows > 0)
        else {
            return 0;
        };
        if row + 1 >= self.view_rows {
            1
        } else if row == 0 {
            -1
        } else {
            0
        }
    }

    /// The conversation's rows as a view `height` tall shows them, the selected columns in reverse
    /// video.
    pub(super) fn view_lines(&self, height: usize) -> Vec<String> {
        let (first, _) = self.history.span(height);
        self.history
            .view(height)
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let plain = text::plain(row);
                match self
                    .selection
                    .as_ref()
                    .and_then(|selection| selection.columns_on(first + i))
                {
                    Some((from, to)) => highlight(&plain, from, to),
                    None => plain.into_owned(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
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
        app.pressed = Some(((24, 1), Instant::now(), 1));
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
}
