//! Fullscreen mode's conversation: everything printed so far, kept as what it says rather than
//! as rows, so it is laid out again when the window changes width; and how far the view has
//! scrolled back from the newest row.

use std::borrow::Cow;
use std::rc::Rc;

use super::text::{hanging_indent, wrap, wrap_words};
use super::fit;

/// Entries kept; the oldest are dropped past this, a few at a time.
const KEPT: usize = 20_000;
const DROPPED_AT_ONCE: usize = 2_000;

/// A piece of the conversation and how it is laid out.
#[derive(Clone)]
pub enum Entry {
    /// One line, wrapped at word breaks under its own indent.
    Row(String),
    /// A line wrapped at word breaks, its later rows indented by the second field.
    Words(String, usize),
    /// One row, cut where the window ends.
    Fit(String),
    /// Anything else: the rows for a width.
    Block(Rc<dyn Fn(usize) -> Vec<String>>),
}

impl Entry {
    pub fn block(render: impl Fn(usize) -> Vec<String> + 'static) -> Self {
        Entry::Block(Rc::new(render))
    }

    /// The screen rows of this entry in a window `width` columns wide.
    pub fn render(&self, width: usize) -> Vec<String> {
        match self {
            Entry::Row(line) => clean(line).split('\n').flat_map(|part| wrap_words(part, width, hanging_indent(part))).collect(),
            Entry::Words(line, indent) => wrap_words(&clean(line), width, *indent),
            Entry::Fit(line) => vec![fit(&clean(line), width)],
            Entry::Block(render) => render(width).iter().flat_map(|row| wrap(&clean(row), width)).collect(),
        }
    }
}

/// `text` without what moves the cursor on its own: tabs become spaces, and carriage returns and
/// other control characters (not a line break or an escape sequence) go. Left in a row painted
/// at a fixed place, they would run it over its neighbours.
fn clean(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| c.is_control() && c != '\n' && c != '\x1b') {
        return Cow::Borrowed(text);
    }
    let mut clean = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => clean.push_str("    "),
            c if c.is_control() && c != '\n' && c != '\x1b' => {}
            c => clean.push(c),
        }
    }
    Cow::Owned(clean)
}

#[derive(Default)]
pub struct History {
    entries: Vec<Entry>,
    /// The width `rows` were laid out for.
    width: usize,
    /// Every entry's rows, never two blank ones in a row.
    rows: Vec<String>,
    /// How many rows the view is scrolled back from the newest; 0 follows the conversation.
    back: usize,
    /// Where the banner starts in `entries` and how many entries it has, while they are kept.
    banner: Option<(usize, usize)>,
}

impl History {
    pub fn new(width: usize) -> Self {
        Self { width, ..Self::default() }
    }

    /// Adds an entry. A view scrolled back stays on what it shows.
    pub fn push(&mut self, entry: Entry) {
        let before = self.rows.len();
        self.add(entry);
        if self.back > 0 {
            self.back += self.rows.len() - before;
        }
        if self.entries.len() >= KEPT + DROPPED_AT_ONCE {
            self.entries.drain(..DROPPED_AT_ONCE);
            self.banner = self.banner.and_then(|(start, len)| start.checked_sub(DROPPED_AT_ONCE).map(|start| (start, len)));
            self.layout();
        }
    }

    fn add(&mut self, entry: Entry) {
        for row in entry.render(self.width) {
            if !(row.is_empty() && self.rows.last().is_some_and(String::is_empty)) {
                self.rows.push(row);
            }
        }
        self.entries.push(entry);
    }

    /// Lays every entry out again for the current width.
    fn layout(&mut self) {
        self.rows.clear();
        for entry in &self.entries {
            for row in entry.render(self.width) {
                if !(row.is_empty() && self.rows.last().is_some_and(String::is_empty)) {
                    self.rows.push(row);
                }
            }
        }
        self.back = self.back.min(self.rows.len());
    }

    /// Lays the conversation out for a window `width` columns wide.
    pub fn set_width(&mut self, width: usize) {
        if width != self.width {
            self.width = width;
            self.layout();
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new(self.width);
    }

    #[cfg(test)]
    pub fn rows(&self) -> &[String] {
        &self.rows
    }

    /// Every row, for leaving the screen with the conversation printed above the prompt.
    pub fn all_rows(&self) -> &[String] {
        &self.rows
    }

    /// Scrolls back by `rows`, at most to where the oldest row is the view's first.
    pub fn scroll_up(&mut self, rows: usize, height: usize) {
        self.back = (self.back + rows).min(self.rows.len().saturating_sub(height));
    }

    pub fn scroll_down(&mut self, rows: usize) {
        self.back = self.back.saturating_sub(rows);
    }

    pub fn follow(&mut self) {
        self.back = 0;
    }

    /// The rows a view `height` tall shows: the newest that fit, or those a scrolled-back view
    /// ends on. Fewer than `height` when the conversation is shorter.
    pub fn view(&self, height: usize) -> &[String] {
        let end = self.rows.len() - self.below(height);
        &self.rows[end.saturating_sub(height)..end]
    }

    /// Rows under a view `height` tall.
    pub fn below(&self, height: usize) -> usize {
        self.back.min(self.rows.len().saturating_sub(height))
    }

    /// Marks that the banner starts with the next entry.
    pub fn banner_starts(&mut self) {
        self.banner = Some((self.entries.len(), 0));
    }

    /// Marks that the banner ended with the last entry.
    pub fn banner_ends(&mut self) {
        if let Some((start, len)) = &mut self.banner {
            *len = self.entries.len() - *start;
        }
    }

    /// Removes the banner and what follows it, returning what followed; None when the banner
    /// is no longer kept.
    pub fn cut_banner(&mut self) -> Option<Vec<Entry>> {
        let (start, len) = self.banner.take()?;
        let rest = self.entries.split_off(start + len);
        self.entries.truncate(start);
        self.layout();
        Some(rest)
    }

    /// Puts back the entries `cut_banner` returned.
    pub fn restore(&mut self, entries: Vec<Entry>) {
        for entry in entries {
            self.add(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(from: usize, to: usize) -> Vec<Entry> {
        (from..to).map(|n| Entry::Row(format!("row {n}"))).collect()
    }

    fn history(width: usize, entries: Vec<Entry>) -> History {
        let mut history = History::new(width);
        entries.into_iter().for_each(|entry| history.push(entry));
        history
    }

    #[test]
    fn the_view_shows_the_newest_rows_or_all_of_a_short_conversation() {
        let mut history = history(80, rows(0, 3));
        assert_eq!(history.view(5), ["row 0", "row 1", "row 2"]);
        rows(3, 10).into_iter().for_each(|entry| history.push(entry));
        assert_eq!(history.view(4), ["row 6", "row 7", "row 8", "row 9"]);
    }

    #[test]
    fn scrolling_back_stops_at_the_oldest_row_and_stays_put_as_rows_arrive() {
        let mut history = history(80, rows(0, 10));
        history.scroll_up(3, 4);
        assert_eq!(history.view(4), ["row 3", "row 4", "row 5", "row 6"]);
        assert_eq!(history.below(4), 3);
        rows(10, 12).into_iter().for_each(|entry| history.push(entry));
        assert_eq!(history.view(4), ["row 3", "row 4", "row 5", "row 6"]);
        history.scroll_up(100, 4);
        assert_eq!(history.view(4), ["row 0", "row 1", "row 2", "row 3"]);
        history.scroll_down(1);
        assert_eq!(history.view(4)[0], "row 1");
        history.follow();
        assert_eq!(history.view(4)[3], "row 11");
    }

    #[test]
    fn a_taller_window_pulls_a_scrolled_view_back_to_what_exists() {
        let mut history = history(80, rows(0, 10));
        history.scroll_up(6, 4);
        assert_eq!(history.below(8), 2);
        assert_eq!(history.view(8).len(), 8);
    }

    #[test]
    fn never_two_blank_rows_in_a_row() {
        let blank = || Entry::Row(String::new());
        let history = history(80, vec![Entry::Row("a".into()), blank(), blank(), Entry::Block(Rc::new(|_| vec![String::new(), "b".into()]))]);
        assert_eq!(history.rows(), ["a", "", "b"]);
    }

    #[test]
    fn a_wider_window_lays_entries_out_again_and_a_narrower_one_too() {
        let line = "one two three four five six".to_string();
        let mut history = history(80, vec![Entry::Words(line.clone(), 2), Entry::Fit(line.clone()), Entry::Row(line)]);
        assert_eq!(history.rows(), ["one two three four five six"; 3]);
        history.set_width(12);
        let plain: Vec<String> = history.rows().iter().map(|row| row.replace("\x1b[0m", "")).collect();
        assert_eq!(plain, ["one two", "  three four", "  five six", "one two thre", "one two", "three four", "five six"]);
        history.set_width(80);
        assert_eq!(history.rows(), ["one two three four five six"; 3]);
    }

    #[test]
    fn tabs_and_carriage_returns_cannot_move_the_cursor() {
        let history = history(80, vec![Entry::Row("a\tb\rc\x07d".into()), Entry::Fit("\x1b[1mx\ty".into())]);
        assert_eq!(history.rows(), ["a    bcd", "\x1b[1mx    y"]);
    }

    #[test]
    fn a_block_is_asked_for_the_width() {
        let mut history = history(10, vec![Entry::block(|width| vec!["x".repeat(width)])]);
        assert_eq!(history.rows(), ["xxxxxxxxxx"]);
        history.set_width(4);
        assert_eq!(history.rows(), ["xxxx"]);
    }

    #[test]
    fn only_the_newest_entries_are_kept() {
        let history = history(80, rows(0, KEPT + DROPPED_AT_ONCE));
        assert_eq!(history.rows().len(), KEPT);
        assert_eq!(history.rows()[0], format!("row {DROPPED_AT_ONCE}"));
    }

    #[test]
    fn the_banner_and_what_followed_can_be_cut_and_put_back() {
        let mut history = History::new(80);
        history.push(Entry::Row("before".into()));
        history.banner_starts();
        rows(0, 2).into_iter().for_each(|entry| history.push(entry));
        history.banner_ends();
        rows(2, 4).into_iter().for_each(|entry| history.push(entry));
        let rest = history.cut_banner().unwrap();
        assert_eq!(rest.len(), 2);
        assert_eq!(history.rows(), ["before"]);
        assert!(history.cut_banner().is_none());
        history.restore(rest);
        assert_eq!(history.rows(), ["before", "row 2", "row 3"]);
    }

    #[test]
    fn a_banner_pushed_out_of_the_kept_entries_is_forgotten() {
        let mut history = History::new(80);
        history.banner_starts();
        rows(0, 2).into_iter().for_each(|entry| history.push(entry));
        history.banner_ends();
        rows(2, KEPT + DROPPED_AT_ONCE).into_iter().for_each(|entry| history.push(entry));
        assert!(history.cut_banner().is_none());
    }
}
