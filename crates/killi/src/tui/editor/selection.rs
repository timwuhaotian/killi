//! The selection in the prompt: the bytes between an anchor and the cursor. Shift with a move
//! extends it, typing or a paste replaces it, Backspace takes it out and a plain move ends it.
//! The mouse sets it as it does in the conversation: a press puts the cursor down, a drag
//! selects from there, a double-click takes a word and a triple-click a row.

use super::*;

impl Editor {
    /// The selected bytes, start to end; None when nothing is selected.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor.filter(|&anchor| anchor != self.cursor)?;
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(start, end)| &self.text[start..end])
    }

    /// Moves the cursor with `step` and keeps the selection's other end where it was: a move
    /// with Shift held.
    pub fn select(&mut self, step: fn(&mut Self)) {
        let anchor = self.anchor.take().unwrap_or(self.cursor);
        step(self);
        self.anchor = Some(anchor);
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    /// Moves up a visual row, or to the start of the text from the first row: Shift+Up never
    /// leaves the text for history.
    pub fn up_within(&mut self) {
        if !self.row_up() {
            self.cursor = 0;
        }
    }

    /// Moves down a visual row, or to the end of the text from the last row.
    pub fn down_within(&mut self) {
        if !self.row_down() {
            self.cursor = self.text.len();
        }
    }

    /// Takes the selected text out of the editor and returns it.
    pub fn cut(&mut self) -> Option<String> {
        let cut = self.selected_text()?.to_string();
        self.delete_selection();
        self.browsing = None;
        Some(cut)
    }

    pub fn deselect(&mut self) {
        self.anchor = None;
    }

    /// The byte at display column `column` of visual row `row`, as the last render laid the rows
    /// out and counted from the text's first column: the character there, or the row's end past
    /// its text. A row below the last counts as the last.
    pub fn point(&self, row: usize, column: usize) -> usize {
        let rows = self.visual_rows();
        rows.get(row.min(rows.len().saturating_sub(1)))
            .map_or(self.text.len(), |row| Self::byte_at_column(row, column))
    }

    /// A press at byte `at`: the cursor goes there, and a drag selects from it.
    pub fn press(&mut self, at: usize) {
        self.cursor = at;
        self.anchor = Some(at);
    }

    /// The pointer dragged to byte `at`: the selection runs from the press to it.
    pub fn drag_to(&mut self, at: usize) {
        self.anchor.get_or_insert(self.cursor);
        self.cursor = at;
    }

    /// A double-click at byte `at`: the word around it, as the conversation picks one — a run
    /// of letters, digits and underscores, or the one other character clicked. Blank space
    /// selects nothing.
    pub fn select_word(&mut self, at: usize) {
        self.press(at);
        let Some(clicked) = self.text[at..]
            .chars()
            .next()
            .filter(|c| !c.is_whitespace())
        else {
            return;
        };
        // A character with no width of its own, like a combining mark, belongs to its word.
        let joins = |c: char| c.is_alphanumeric() || c == '_' || c.width() == Some(0);
        let (mut start, mut end) = (at, at + clicked.len_utf8());
        if joins(clicked) {
            start = self.text[..at]
                .char_indices()
                .rev()
                .take_while(|&(_, c)| joins(c))
                .last()
                .map_or(at, |(i, _)| i);
            end = self.text[at..]
                .char_indices()
                .find(|&(_, c)| !joins(c))
                .map_or(self.text.len(), |(i, _)| at + i);
        }
        self.anchor = Some(start);
        self.cursor = end;
    }

    /// A triple-click on visual row `row`: the whole row.
    pub fn select_row(&mut self, row: usize) {
        let rows = self.visual_rows();
        let Some(visual) = rows.get(row.min(rows.len().saturating_sub(1))) else {
            return;
        };
        self.anchor = Some(
            visual
                .positions
                .first()
                .map_or(visual.end.0, |&(byte, _)| byte),
        );
        self.cursor = visual.end.0;
    }

    /// Takes the selected text out, the cursor where it began. False when nothing is selected.
    pub(super) fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.take_selection() else {
            return false;
        };
        self.text.replace_range(start..end, "");
        self.cursor = start;
        true
    }

    /// Ends the selection and returns what it covered.
    pub(super) fn take_selection(&mut self) -> Option<(usize, usize)> {
        let selection = self.selection();
        self.anchor = None;
        selection
    }
}
