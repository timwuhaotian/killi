//! The prompt editor: multi-line text with a cursor, readline-style keys and history.

use super::text::{self, LAGOON, RESET, REVERSE};
use crate::agent::Image;
use unicode_width::UnicodeWidthChar;

mod render;
mod selection;

#[derive(Default)]
pub struct Editor {
    text: String,
    /// Byte offset of the cursor in `text`.
    cursor: usize,
    /// The selection's other end, the cursor being one; none, or the cursor itself, when
    /// nothing is selected.
    anchor: Option<usize>,
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
        Self {
            history,
            ..Self::default()
        }
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
        self.anchor = None;
        self.browsing = None;
    }

    /// The text is a history entry that hasn't been typed into.
    pub fn browsing(&self) -> bool {
        self.browsing.is_some()
    }

    /// Inserts `s` at the cursor, in place of the selection when there is one.
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
        self.browsing = None;
    }

    /// Inserts `word` at the cursor as a word of its own: a space before it when the cursor
    /// follows one, and a space after.
    pub fn insert_word(&mut self, word: &str) {
        let lead = if self.before_cursor().ends_with(|c: char| !c.is_whitespace()) {
            " "
        } else {
            ""
        };
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
        let (text, images) = self.take_draft();
        self.remember(&text);
        (text, images)
    }

    /// Takes the text out with its images like `take_with_images`, without adding it to
    /// history: a draft set aside to be put back.
    pub fn take_draft(&mut self) -> (String, Vec<Image>) {
        let images = std::mem::take(&mut self.images);
        let text = self.take_quiet();
        let images = images
            .into_iter()
            .filter(|image| text.contains(&image.label))
            .collect();
        (text, images)
    }

    /// Takes the text out, adding it to history; pasted images are dropped.
    pub fn take(&mut self) -> String {
        let text = self.take_quiet();
        self.remember(&text);
        text
    }

    fn remember(&mut self, text: &str) {
        if !text.trim().is_empty() && self.history.last().map(String::as_str) != Some(text) {
            self.history.push(text.to_string());
        }
    }

    /// Takes the text out without adding it to history, for answers that must not be recalled.
    pub fn take_quiet(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.anchor = None;
        self.browsing = None;
        self.images.clear();
        text
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.anchor = None;
        self.browsing = None;
        self.images.clear();
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some(prev) = self.prev_boundary(self.cursor) {
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if let Some(next) = self.next_boundary(self.cursor) {
            self.text.replace_range(self.cursor..next, "");
        }
    }

    /// Moves left a character, or to the selection's start, ending it.
    pub fn left(&mut self) {
        if let Some((start, _)) = self.take_selection() {
            self.cursor = start;
            return;
        }
        if let Some(prev) = self.prev_boundary(self.cursor) {
            self.cursor = prev;
        }
    }

    /// Moves right a character, or to the selection's end, ending it.
    pub fn right(&mut self) {
        if let Some((_, end)) = self.take_selection() {
            self.cursor = end;
            return;
        }
        if let Some(next) = self.next_boundary(self.cursor) {
            self.cursor = next;
        }
    }

    pub fn home(&mut self) {
        self.anchor = None;
        self.cursor = self.line_start(self.cursor);
    }

    pub fn end(&mut self) {
        self.anchor = None;
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i);
    }

    pub fn word_left(&mut self) {
        self.anchor = None;
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches(|c: char| !c.is_alphanumeric());
        self.cursor = trimmed
            .rfind(|c: char| !c.is_alphanumeric())
            .map_or(0, |i| {
                i + trimmed[i..].chars().next().map_or(1, char::len_utf8)
            });
    }

    pub fn word_right(&mut self) {
        self.anchor = None;
        let after = &self.text[self.cursor..];
        let skip = after
            .find(|c: char| c.is_alphanumeric())
            .unwrap_or(after.len());
        let end = after[skip..]
            .find(|c: char| !c.is_alphanumeric())
            .map_or(after.len(), |i| skip + i);
        self.cursor += end;
    }

    /// Deletes the word before the cursor, or the selection when there is one.
    pub fn delete_word(&mut self) {
        if self.delete_selection() {
            return;
        }
        let end = self.cursor;
        self.word_left();
        self.text.replace_range(self.cursor..end, "");
    }

    pub fn kill_to_start(&mut self) {
        self.anchor = None;
        let start = self.line_start(self.cursor);
        let start = if start == self.cursor && start > 0 {
            self.line_start(start - 1)
        } else {
            start
        };
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    pub fn kill_to_end(&mut self) {
        self.anchor = None;
        let end = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i);
        let end = if end == self.cursor && end < self.text.len() {
            end + 1
        } else {
            end
        };
        self.text.replace_range(self.cursor..end, "");
    }

    /// Moves up a visual row, or to the previous history entry from the first row.
    pub fn up(&mut self) {
        self.anchor = None;
        if !self.row_up() {
            self.recall_previous();
        }
    }

    /// Moves up a visual row; false on the first row, where it stays. Before any render the
    /// room is unbounded, so this is the old logical-line movement.
    fn row_up(&mut self) -> bool {
        if self.room.get() != 0 {
            let rows = self.visual_rows();
            if let Some((row, column)) = self.visual_cursor(&rows) {
                if row == 0 {
                    return false;
                }
                self.cursor = Self::byte_at_column(&rows[row - 1], column);
                return true;
            }
        }
        let start = self.line_start(self.cursor);
        if start == 0 {
            return false;
        }
        let column = self.text[start..self.cursor].chars().count();
        let prev_start = self.line_start(start - 1);
        self.cursor = self.at_column(prev_start, column);
        true
    }

    fn recall_previous(&mut self) {
        let index = match &self.browsing {
            Some((i, _)) => i.checked_sub(1),
            None => self.history.len().checked_sub(1),
        };
        if let Some(index) = index {
            let draft = self
                .browsing
                .take()
                .map_or_else(|| self.text.clone(), |(_, draft)| draft);
            self.text = self.history[index].clone();
            self.cursor = self.text.len();
            self.browsing = Some((index, draft));
        }
    }

    /// Moves down a visual row, or to the next history entry from the last row.
    pub fn down(&mut self) {
        self.anchor = None;
        if !self.row_down() {
            self.recall_next();
        }
    }

    /// Moves down a visual row; false on the last row, where it stays. Before any render the
    /// room is unbounded, so this is the old logical-line movement.
    fn row_down(&mut self) -> bool {
        if self.room.get() != 0 {
            let rows = self.visual_rows();
            if let Some((row, column)) = self.visual_cursor(&rows) {
                if row + 1 == rows.len() {
                    return false;
                }
                self.cursor = Self::byte_at_column(&rows[row + 1], column);
                return true;
            }
        }
        let Some(i) = self.text[self.cursor..].find('\n') else {
            return false;
        };
        let column = self.text[self.line_start(self.cursor)..self.cursor]
            .chars()
            .count();
        self.cursor = self.at_column(self.cursor + i + 1, column);
        true
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
        let line_end = self.text[line_start..]
            .find('\n')
            .map_or(self.text.len(), |i| line_start + i);
        self.text[line_start..line_end]
            .char_indices()
            .nth(column)
            .map_or(line_end, |(i, _)| line_start + i)
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
            let bounds: Vec<usize> = std::iter::once(0)
                .chain(row_starts(&chars, room))
                .chain([chars.len()])
                .collect();
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
                let end = if span[1] == chars.len() {
                    offset + line.len()
                } else {
                    offset + chars[span[1]].0
                };
                rows.push(VisualRow {
                    positions,
                    end: (end, column),
                    last: r + 2 == bounds.len(),
                });
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
        if let Some(row) = rows
            .iter()
            .position(|row| row.last && row.end.0 == self.cursor)
        {
            return Some((row, rows[row].end.1));
        }
        let mut found = None;
        for (row, visual) in rows.iter().enumerate() {
            if let Some(&(_, column)) = visual
                .positions
                .iter()
                .find(|&&(byte, _)| byte == self.cursor)
            {
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
            let at = if word_start > row_start {
                word_start
            } else {
                k
            };
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
mod tests;
