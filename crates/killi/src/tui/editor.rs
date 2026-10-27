//! The prompt editor: multi-line text with a cursor, readline-style keys and history.

use super::text::{LAGOON, RESET};
use crate::agent::Image;
use unicode_width::UnicodeWidthChar;

#[derive(Default)]
pub struct Editor {
    text: String,
    /// Byte offset of the cursor in `text`.
    cursor: usize,
    history: Vec<String>,
    /// Position while browsing history, and the unsent text to come back to.
    browsing: Option<(usize, String)>,
    /// Images pasted into the text, each standing there as its placeholder.
    images: Vec<Image>,
    /// Images pasted this session, so a placeholder's number is never used twice.
    pasted: usize,
    /// Text columns from the most recent `render`; 0 before any render, meaning unbounded.
    room: std::cell::Cell<usize>,
}

impl Editor {
    pub fn with_history(history: Vec<String>) -> Self {
        Self { history, ..Self::default() }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn before_cursor(&self) -> &str {
        &self.text[..self.cursor]
    }

    /// Replaces the text from byte `start` to the cursor with `with`, leaving the cursor after it.
    pub fn replace_before(&mut self, start: usize, with: &str) {
        self.text.replace_range(start..self.cursor, with);
        self.cursor = start + with.len();
        self.browsing = None;
    }

    /// The text is a history entry that hasn't been typed into.
    pub fn browsing(&self) -> bool {
        self.browsing.is_some()
    }

    pub fn insert(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
        self.browsing = None;
    }

    /// Inserts `word` at the cursor as a word of its own: a space before it when the cursor
    /// follows one, and a space after.
    pub fn insert_word(&mut self, word: &str) {
        let lead = if self.before_cursor().ends_with(|c: char| !c.is_whitespace()) { " " } else { "" };
        self.insert(&format!("{lead}{word} "));
    }

    /// Puts an image in at the cursor as an `[Image #N]` placeholder, set off by spaces.
    pub fn attach(&mut self, url: String) {
        self.pasted += 1;
        let label = format!("[Image #{}]", self.pasted);
        self.insert_word(&label);
        self.images.push(Image { label, url });
    }

    /// Takes the text out like `take`, with the images whose placeholders are still in it.
    pub fn take_with_images(&mut self) -> (String, Vec<Image>) {
        let images = std::mem::take(&mut self.images);
        let text = self.take();
        let images = images.into_iter().filter(|image| text.contains(&image.label)).collect();
        (text, images)
    }

    /// Takes the text out, adding it to history; pasted images are dropped.
    pub fn take(&mut self) -> String {
        let text = self.take_quiet();
        if !text.trim().is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        text
    }

    /// Takes the text out without adding it to history, for answers that must not be recalled.
    pub fn take_quiet(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.browsing = None;
        self.images.clear();
        text
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.browsing = None;
        self.images.clear();
    }

    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_boundary(self.cursor) {
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if let Some(next) = self.next_boundary(self.cursor) {
            self.text.replace_range(self.cursor..next, "");
        }
    }

    pub fn left(&mut self) {
        if let Some(prev) = self.prev_boundary(self.cursor) {
            self.cursor = prev;
        }
    }

    pub fn right(&mut self) {
        if let Some(next) = self.next_boundary(self.cursor) {
            self.cursor = next;
        }
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start(self.cursor);
    }

    pub fn end(&mut self) {
        self.cursor = self.text[self.cursor..].find('\n').map_or(self.text.len(), |i| self.cursor + i);
    }

    pub fn word_left(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches(|c: char| !c.is_alphanumeric());
        self.cursor = trimmed.rfind(|c: char| !c.is_alphanumeric()).map_or(0, |i| i + trimmed[i..].chars().next().map_or(1, char::len_utf8));
    }

    pub fn word_right(&mut self) {
        let after = &self.text[self.cursor..];
        let skip = after.find(|c: char| c.is_alphanumeric()).unwrap_or(after.len());
        let end = after[skip..].find(|c: char| !c.is_alphanumeric()).map_or(after.len(), |i| skip + i);
        self.cursor += end;
    }

    pub fn delete_word(&mut self) {
        let end = self.cursor;
        self.word_left();
        self.text.replace_range(self.cursor..end, "");
    }

    pub fn kill_to_start(&mut self) {
        let start = self.line_start(self.cursor);
        let start = if start == self.cursor && start > 0 { self.line_start(start - 1) } else { start };
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn kill_to_end(&mut self) {
        let end = self.text[self.cursor..].find('\n').map_or(self.text.len(), |i| self.cursor + i);
        let end = if end == self.cursor && end < self.text.len() { end + 1 } else { end };
        self.text.replace_range(self.cursor..end, "");
    }

    /// Moves up a visual row, or to the previous history entry from the first row. Before
    /// any render the room is unbounded, so this is the old logical-line movement.
    pub fn up(&mut self) {
        if self.room.get() != 0 {
            let rows = self.visual_rows();
            if let Some((row, column)) = self.visual_cursor(&rows) {
                if row > 0 {
                    self.cursor = Self::byte_at_column(&rows[row - 1], column);
                    return;
                }
                self.recall_previous();
                return;
            }
        }
        let start = self.line_start(self.cursor);
        if start > 0 {
            let column = self.text[start..self.cursor].chars().count();
            let prev_start = self.line_start(start - 1);
            self.cursor = self.at_column(prev_start, column);
            return;
        }
        self.recall_previous();
    }

    fn recall_previous(&mut self) {
        let index = match &self.browsing {
            Some((i, _)) => i.checked_sub(1),
            None => self.history.len().checked_sub(1),
        };
        if let Some(index) = index {
            let draft = self.browsing.take().map_or_else(|| self.text.clone(), |(_, draft)| draft);
            self.text = self.history[index].clone();
            self.cursor = self.text.len();
            self.browsing = Some((index, draft));
        }
    }

    /// Moves down a visual row, or to the next history entry from the last row. Before
    /// any render the room is unbounded, so this is the old logical-line movement.
    pub fn down(&mut self) {
        if self.room.get() != 0 {
            let rows = self.visual_rows();
            if let Some((row, column)) = self.visual_cursor(&rows) {
                if row + 1 < rows.len() {
                    self.cursor = Self::byte_at_column(&rows[row + 1], column);
                    return;
                }
                self.recall_next();
                return;
            }
        }
        if let Some(i) = self.text[self.cursor..].find('\n') {
            let column = self.text[self.line_start(self.cursor)..self.cursor].chars().count();
            self.cursor = self.at_column(self.cursor + i + 1, column);
            return;
        }
        self.recall_next();
    }

    fn recall_next(&mut self) {
        if let Some((index, draft)) = self.browsing.take() {
            if index + 1 < self.history.len() {
                self.text = self.history[index + 1].clone();
                self.browsing = Some((index + 1, draft));
            } else {
                self.text = draft;
            }
            self.cursor = self.text.len();
        }
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn at_column(&self, line_start: usize, column: usize) -> usize {
        let line_end = self.text[line_start..].find('\n').map_or(self.text.len(), |i| line_start + i);
        self.text[line_start..line_end].char_indices().nth(column).map_or(line_end, |(i, _)| line_start + i)
    }

    /// The visual rows at the room from the last render, laid out as `render` lays them
    /// out: tabs expand to the next 4-column stop and `row_starts` splits each line.
    fn visual_rows(&self) -> Vec<VisualRow> {
        let room = match self.room.get() {
            0 => usize::MAX,
            room => room,
        };
        let mut rows = Vec::new();
        let mut offset = 0;
        for line in self.text.split('\n') {
            let mut chars: Vec<(usize, char, usize)> = Vec::new();
            let mut column = 0;
            for (i, c) in line.char_indices() {
                if c == '\t' {
                    for _ in 0..TAB - column % TAB {
                        chars.push((i, ' ', 1));
                    }
                    column += TAB - column % TAB;
                } else {
                    let w = c.width().unwrap_or(0);
                    chars.push((i, c, w));
                    column += w;
                }
            }
            let bounds: Vec<usize> = std::iter::once(0).chain(row_starts(&chars, room)).chain([chars.len()]).collect();
            for (r, span) in bounds.windows(2).enumerate() {
                let mut positions = Vec::new();
                let mut column = 0;
                for &(i, _, w) in &chars[span[0]..span[1]] {
                    let byte = offset + i;
                    if positions.last().map(|&(b, _)| b) != Some(byte) {
                        positions.push((byte, column));
                    }
                    column += w;
                }
                let end = if span[1] == chars.len() { offset + line.len() } else { offset + chars[span[1]].0 };
                rows.push(VisualRow { positions, end: (end, column), last: r + 2 == bounds.len() });
            }
            offset += line.len() + 1;
        }
        rows
    }

    /// The cursor's visual row and its display column in that row. A cursor at a logical
    /// line's end is on that line's last row; otherwise the last row holding its byte
    /// holds it, so a byte starting a wrapped row is on that row and a tab byte is at
    /// the tab's first column in its row.
    fn visual_cursor(&self, rows: &[VisualRow]) -> Option<(usize, usize)> {
        if let Some(row) = rows.iter().position(|row| row.last && row.end.0 == self.cursor) {
            return Some((row, rows[row].end.1));
        }
        let mut found = None;
        for (row, visual) in rows.iter().enumerate() {
            if let Some(&(_, column)) = visual.positions.iter().find(|&&(byte, _)| byte == self.cursor) {
                found = Some((row, column));
            }
        }
        found
    }

    /// The closest byte in `row` whose display column is at most `goal`, or the row's
    /// end when `goal` is past it.
    fn byte_at_column(row: &VisualRow, goal: usize) -> usize {
        let mut at = row.positions.first().map_or(row.end.0, |&(byte, _)| byte);
        for &(byte, column) in &row.positions {
            if column > goal {
                break;
            }
            at = byte;
        }
        if row.end.1 <= goal {
            at = row.end.0;
        }
        at
    }

    fn prev_boundary(&self, at: usize) -> Option<usize> {
        self.text[..at].char_indices().next_back().map(|(i, _)| i)
    }

    fn next_boundary(&self, at: usize) -> Option<usize> {
        self.text[at..].chars().next().map(|c| at + c.len_utf8())
    }

    /// Byte ranges of the words `/name` where `known` accepts `name`.
    fn marked(&self, known: &dyn Fn(&str) -> bool) -> Vec<(usize, usize)> {
        let mut ranges = Vec::new();
        let mut start = None;
        for (i, c) in self.text.char_indices().chain([(self.text.len(), ' ')]) {
            match (start, c.is_whitespace()) {
                (None, false) => start = Some(i),
                (Some(s), true) => {
                    if self.text[s..i].strip_prefix('/').is_some_and(known) {
                        ranges.push((s, i));
                    }
                    start = None;
                }
                _ => {}
            }
        }
        ranges
    }

    /// The text as rows no wider than `width` after `prompt`, and the cursor's row and column.
    /// Rows break after a space where they can, so words stay whole. Words `/name` that `known`
    /// accepts are drawn in lagoon, like other commands. A tab stays in the text but is laid
    /// out as the spaces to the next 4-column stop, counted from its line's first column, so
    /// the row widths and the cursor agree with what the screen shows.
    pub fn render(&self, prompt: &str, width: usize, known: &dyn Fn(&str) -> bool) -> (Vec<String>, usize, usize) {
        let prompt_width = super::text::width(prompt);
        let room = width.saturating_sub(prompt_width).max(1);
        self.room.set(room);
        let marked = self.marked(known);
        let mut rows = Vec::new();
        let mut cursor = (0, prompt_width);
        let mut offset = 0;
        for (n, line) in self.text.split('\n').enumerate() {
            let lead = if n == 0 { prompt.to_string() } else { " ".repeat(prompt_width) };
            let mut chars: Vec<(usize, char, usize)> = Vec::new();
            let mut column = 0;
            for (i, c) in line.char_indices() {
                if c == '\t' {
                    for _ in 0..TAB - column % TAB {
                        chars.push((i, ' ', 1));
                    }
                    column += TAB - column % TAB;
                } else {
                    let w = c.width().unwrap_or(0);
                    chars.push((i, c, w));
                    column += w;
                }
            }
            let bounds: Vec<usize> = std::iter::once(0).chain(row_starts(&chars, room)).chain([chars.len()]).collect();
            for (r, span) in bounds.windows(2).enumerate() {
                let mut row = String::new();
                let mut row_width = 0;
                let mut lit = false;
                let mut placed = usize::MAX;
                for &(i, c, w) in &chars[span[0]..span[1]] {
                    // A space that would pass the edge is not drawn: the row ends there.
                    let hanging = c == ' ' && row_width + w > room;
                    // The spaces a tab laid out to share its byte offset place the cursor once,
                    // at the tab's first column, as the cursor on the tab itself.
                    if offset + i == self.cursor && placed != i {
                        placed = i;
                        cursor = if hanging { (rows.len() + 1, prompt_width) } else { (rows.len(), prompt_width + row_width) };
                    }
                    if hanging {
                        continue;
                    }
                    let on = marked.iter().any(|&(s, e)| (s..e).contains(&(offset + i)));
                    if on != lit {
                        row.push_str(&if on { LAGOON.to_string() } else { RESET.to_string() });
                        lit = on;
                    }
                    row.push(c);
                    row_width += w;
                }
                if lit {
                    row.push_str(RESET);
                }
                if span[1] == chars.len() && offset + line.len() == self.cursor {
                    cursor = if row_width >= room { (rows.len() + 1, prompt_width) } else { (rows.len(), prompt_width + row_width) };
                }
                rows.push(format!("{}{row}", if r == 0 { lead.clone() } else { " ".repeat(prompt_width) }));
            }
            offset += line.len() + 1;
        }
        if cursor.0 >= rows.len() {
            rows.push(" ".repeat(prompt_width));
        }
        (rows, cursor.0, cursor.1)
    }
}

/// One visual row: the first display column of each byte in it, its end byte and
/// column, and whether it is its logical line's last row.
struct VisualRow {
    positions: Vec<(usize, usize)>,
    end: (usize, usize),
    last: bool,
}

/// The column a tab in the editor lays out to next.
const TAB: usize = 4;

/// Where each row of a line after the first begins, as indexes into `chars` (byte offset, char,
/// width): at the start of the word that would pass `room` columns, or at the character itself
/// when the row is a single word. Spaces never start a row; one that doesn't fit hangs.
fn row_starts(chars: &[(usize, char, usize)], room: usize) -> Vec<usize> {
    let mut starts = Vec::new();
    let (mut row_start, mut word_start, mut width) = (0, 0, 0);
    for (k, &(_, c, w)) in chars.iter().enumerate() {
        if c == ' ' {
            if width + w <= room {
                width += w;
            }
            word_start = k + 1;
            continue;
        }
        if width + w > room {
            let at = if word_start > row_start { word_start } else { k };
            starts.push(at);
            row_start = at;
            width = chars[at..k].iter().map(|&(.., w)| w).sum();
            if width + w > room {
                starts.push(k);
                row_start = k;
                width = 0;
            }
            word_start = row_start;
        }
        width += w;
    }
    starts
}

#[cfg(test)]
mod tests {
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
}
