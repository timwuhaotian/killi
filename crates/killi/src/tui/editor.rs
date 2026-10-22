//! The prompt editor: multi-line text with a cursor, readline-style keys and history.

use super::text::{ACCENT, RESET};
use unicode_width::UnicodeWidthChar;

#[derive(Default)]
pub struct Editor {
    text: String,
    /// Byte offset of the cursor in `text`.
    cursor: usize,
    history: Vec<String>,
    /// Position while browsing history, and the unsent text to come back to.
    browsing: Option<(usize, String)>,
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

    /// Takes the text out, adding it to history.
    pub fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.browsing = None;
        if !text.trim().is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        text
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.browsing = None;
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

    /// Moves up a line, or to the previous history entry from the first line.
    pub fn up(&mut self) {
        let start = self.line_start(self.cursor);
        if start > 0 {
            let column = self.text[start..self.cursor].chars().count();
            let prev_start = self.line_start(start - 1);
            self.cursor = self.at_column(prev_start, column);
            return;
        }
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

    /// Moves down a line, or to the next history entry from the last line.
    pub fn down(&mut self) {
        if let Some(i) = self.text[self.cursor..].find('\n') {
            let column = self.text[self.line_start(self.cursor)..self.cursor].chars().count();
            self.cursor = self.at_column(self.cursor + i + 1, column);
            return;
        }
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
    /// Words `/name` that `known` accepts are drawn in the accent color.
    pub fn render(&self, prompt: &str, width: usize, known: &dyn Fn(&str) -> bool) -> (Vec<String>, usize, usize) {
        let prompt_width = super::text::width(prompt);
        let room = width.saturating_sub(prompt_width).max(1);
        let marked = self.marked(known);
        let mut rows = Vec::new();
        let mut cursor = (0, prompt_width);
        let mut offset = 0;
        for (n, line) in self.text.split('\n').enumerate() {
            let lead = if n == 0 { prompt.to_string() } else { " ".repeat(prompt_width) };
            let mut row = String::new();
            let mut row_width = 0;
            let mut first = true;
            let mut lit = false;
            for (i, c) in line.char_indices() {
                let w = c.width().unwrap_or(0);
                if row_width + w > room {
                    if lit {
                        row.push_str(RESET);
                        lit = false;
                    }
                    rows.push(format!("{}{row}", if first { lead.clone() } else { " ".repeat(prompt_width) }));
                    row.clear();
                    row_width = 0;
                    first = false;
                }
                if offset + i == self.cursor {
                    cursor = (rows.len(), prompt_width + row_width);
                }
                let on = marked.iter().any(|&(s, e)| (s..e).contains(&(offset + i)));
                if on != lit {
                    row.push_str(&if on { ACCENT.to_string() } else { RESET.to_string() });
                    lit = on;
                }
                row.push(c);
                row_width += w;
            }
            if lit {
                row.push_str(RESET);
            }
            if offset + line.len() == self.cursor {
                cursor = if row_width >= room { (rows.len() + 1, prompt_width) } else { (rows.len(), prompt_width + row_width) };
            }
            rows.push(format!("{}{row}", if first { lead } else { " ".repeat(prompt_width) }));
            offset += line.len() + 1;
        }
        if cursor.0 >= rows.len() {
            rows.push(" ".repeat(prompt_width));
        }
        (rows, cursor.0, cursor.1)
    }
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
    fn known_slash_words_are_colored() {
        let mut e = Editor::default();
        e.insert("a /pdf /nope b/pdf\n/pdf");
        let (rows, ..) = e.render("", 40, &|name| name == "pdf");
        assert_eq!(rows[0], format!("a {ACCENT}/pdf{RESET} /nope b/pdf"));
        assert_eq!(rows[1], format!("{ACCENT}/pdf{RESET}"));
        let (rows, ..) = e.render("", 7, &|name| name == "pdf");
        assert_eq!(rows[0], format!("a {ACCENT}/pdf{RESET} "));
    }
}
