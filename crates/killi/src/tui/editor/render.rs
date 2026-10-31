//! How the editor draws its text: rows that fit the width, commands lit, the cursor placed.

use super::*;

impl Editor {
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
    /// accepts are drawn in lagoon, like other commands, and the selection in reverse video.
    /// A tab stays in the text but is laid
    /// out as the spaces to the next 4-column stop, counted from its line's first column, so
    /// the row widths and the cursor agree with what the screen shows.
    pub fn render(
        &self,
        prompt: &str,
        width: usize,
        known: &dyn Fn(&str) -> bool,
    ) -> (Vec<String>, usize, usize) {
        let prompt_width = text::width(prompt);
        let room = width.saturating_sub(prompt_width).max(1);
        self.room.set(room);
        let marked = self.marked(known);
        let selected = self.selection();
        let mut rows = Vec::new();
        let mut cursor = (0, prompt_width);
        let mut offset = 0;
        for (n, line) in self.text.split('\n').enumerate() {
            let lead = if n == 0 {
                prompt.to_string()
            } else {
                " ".repeat(prompt_width)
            };
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
                let mut row = String::new();
                let mut row_width = 0;
                // Whether the characters drawn last were lit and were selected.
                let mut style = (false, false);
                let mut placed = usize::MAX;
                for &(i, c, w) in &chars[span[0]..span[1]] {
                    // A space that would pass the edge is not drawn: the row ends there.
                    let hanging = c == ' ' && row_width + w > room;
                    // The spaces a tab laid out to share its byte offset place the cursor once,
                    // at the tab's first column, as the cursor on the tab itself.
                    if offset + i == self.cursor && placed != i {
                        placed = i;
                        cursor = if hanging {
                            (rows.len() + 1, prompt_width)
                        } else {
                            (rows.len(), prompt_width + row_width)
                        };
                    }
                    if hanging {
                        continue;
                    }
                    let byte = offset + i;
                    let now = (
                        marked.iter().any(|&(s, e)| (s..e).contains(&byte)),
                        selected.is_some_and(|(s, e)| (s..e).contains(&byte)),
                    );
                    if now != style {
                        if style != (false, false) {
                            row.push_str(RESET);
                        }
                        if now.0 {
                            row.push_str(&LAGOON.to_string());
                        }
                        if now.1 {
                            row.push_str(REVERSE);
                        }
                        style = now;
                    }
                    row.push(c);
                    row_width += w;
                }
                if style != (false, false) {
                    row.push_str(RESET);
                }
                if span[1] == chars.len() && offset + line.len() == self.cursor {
                    cursor = if row_width >= room {
                        (rows.len() + 1, prompt_width)
                    } else {
                        (rows.len(), prompt_width + row_width)
                    };
                }
                rows.push(format!(
                    "{}{row}",
                    if r == 0 {
                        lead.clone()
                    } else {
                        " ".repeat(prompt_width)
                    }
                ));
            }
            offset += line.len() + 1;
        }
        if cursor.0 >= rows.len() {
            rows.push(" ".repeat(prompt_width));
        }
        (rows, cursor.0, cursor.1)
    }
}
