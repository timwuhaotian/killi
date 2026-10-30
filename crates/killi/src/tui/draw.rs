use super::*;

/// The rows a printed line takes on a screen `width` columns wide, wrapping included.
pub(super) fn screen_rows(line: &str, width: usize) -> usize {
    line.split('\n')
        .map(|part| text::width(part).max(1).div_ceil(width.max(1)))
        .sum()
}

impl App {
    /// Commits a line of the model's text, wrapped between words, list items under their text.
    pub(super) fn commit_wrapped(&mut self, line: String) {
        let indent = text::hanging_indent(&line);
        self.emit(Line::Words(line, indent));
    }

    /// Prints what Markdown made of a line: its rows wrapped at word breaks, a table laid out
    /// for the width.
    pub(super) fn commit_parts(&mut self, parts: Vec<Part>) {
        for part in parts {
            match part {
                Part::Row(row) => self.commit_wrapped(row),
                Part::Code(row) => self.emit(Line::block(move |width| wrap_code(&row, width))),
                Part::Table(rows) => self.emit(Line::block(move |width| {
                    text::render_table(&rows, width)
                        .into_iter()
                        .flat_map(|row| wrap_words(&row, width, text::hanging_indent(&row)))
                        .collect()
                })),
            }
        }
    }

    /// Prints a line, never two blank lines in a row.
    pub(super) fn commit(&mut self, line: String) {
        let blank = line.is_empty();
        if blank && self.last_blank {
            return;
        }
        self.last_blank = blank;
        if self.fullscreen {
            self.history.push(Line::Row(line));
        } else {
            self.pending.push(line);
        }
    }

    /// Prints something laid out for the window's width: queued for the scrollback as rows at
    /// the width now, or in fullscreen mode kept to be laid out again at any other.
    pub(super) fn emit(&mut self, entry: Line) {
        if self.fullscreen {
            self.last_blank = false;
            self.history.push(entry);
            return;
        }
        for row in entry.render(self.width) {
            self.commit(row);
        }
    }

    /// Starts a block of the conversation: a blank row first, unless it continues the block
    /// above (prose after prose, a tool call after a tool call, a note after a note).
    pub(super) fn begin(&mut self, kind: Kind) {
        if self.last_kind != kind {
            self.commit(String::new());
        }
        self.last_kind = kind;
    }

    pub(super) fn notice(&mut self, text: &str) {
        self.begin(Kind::Note);
        self.commit(format!("{DIM}{}{RESET}", text::sanitize(text)));
    }

    pub(super) fn error(&mut self, text: &str) {
        self.begin(Kind::Note);
        let why = gist(text);
        self.commit(format!("{DANGER}✗ Error:{RESET} {}", text::sanitize(&why)));
        if let Some(hint) = error_hint(&anyhow::anyhow!("{why}")) {
            self.commit(format!("{DIM}{hint}{RESET}"));
        }
    }

    /// A failed turn: the hint follows the provider's kind and status, never digits in the text.
    pub(super) fn error_report(&mut self, error: &anyhow::Error) {
        self.error_from("", error);
    }

    /// A failed command with its reason: the gist of the error, and the hint its kind earns.
    pub(super) fn error_from(&mut self, context: &str, error: &anyhow::Error) {
        let why = gist(&format!("{error:#}"));
        let what = if context.is_empty() {
            why
        } else {
            format!("{context}: {why}")
        };
        self.begin(Kind::Note);
        self.commit(format!("{DANGER}✗ Error:{RESET} {}", text::sanitize(&what)));
        if let Some(hint) = error_hint(error) {
            self.commit(format!("{DIM}{hint}{RESET}"));
        }
    }

    /// The screen rows the lines waiting to be printed take.
    pub(super) fn pending_rows(&self) -> usize {
        self.pending
            .iter()
            .map(|line| screen_rows(line, self.width))
            .sum()
    }

    /// Paints the live area, in one write inside a synchronized update so no half-drawn frame
    /// is ever shown. When only rows changed, as on a spinner tick, just those are rewritten;
    /// a committed line scrolls the area, so then it is painted whole.
    pub(super) fn draw(&mut self) -> Result<()> {
        self.sync_files();
        if self.fullscreen {
            return self.draw_screen();
        }
        if RESUMED.swap(false, Ordering::Relaxed) {
            // The shell wrote below the area killi erased before it stopped; draw afresh here.
            self.cursor_row = 0;
            self.shown.clear();
        }
        let committed = self.pending_rows();
        let (rows, cursor_row, cursor_col) = self.live();
        let rows: Vec<String> = rows
            .iter()
            .map(|row| text::plain(row).into_owned())
            .collect();
        let changed = self
            .pending
            .is_empty()
            .then(|| changed_rows(&self.shown, &rows))
            .flatten();
        if changed.as_ref().is_some_and(|changed| changed.is_empty())
            && cursor_row == self.cursor_row
            && cursor_col == self.shown_col
        {
            return Ok(());
        }
        let mut frame: Vec<u8> = Vec::new();
        queue!(frame, BeginSynchronizedUpdate, Hide)?;
        match changed {
            Some(changed) => {
                let mut at = self.cursor_row;
                for i in changed {
                    move_rows(&mut frame, at, i)?;
                    queue!(frame, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                    write!(frame, "{}{RESET}", rows[i])?;
                    at = i;
                }
                move_rows(&mut frame, at, cursor_row)?;
            }
            None => {
                if self.cursor_row > 0 {
                    queue!(frame, MoveUp(self.cursor_row as u16))?;
                }
                queue!(frame, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
                for line in self.pending.drain(..) {
                    write!(frame, "{}{RESET}\r\n", text::plain(&line))?;
                    if let Some(since) = &mut self.since_banner {
                        since.rows += screen_rows(&line, self.width);
                        since.lines.push(line);
                    }
                }
                write!(frame, "{}", rows.join("\r\n"))?;
                move_rows(&mut frame, rows.len().saturating_sub(1), cursor_row)?;
            }
        }
        if self
            .since_banner
            .as_ref()
            .is_some_and(|since| since.rows + rows.len() > self.height)
        {
            self.since_banner = None;
        }
        queue!(
            frame,
            MoveToColumn(cursor_col as u16),
            Show,
            EndSynchronizedUpdate
        )?;
        self.out.write_all(&frame)?;
        self.out.flush()?;
        self.content_rows =
            (self.content_rows + committed).min(self.height.saturating_sub(rows.len()));
        self.shown = rows;
        self.shown_col = cursor_col;
        self.cursor_row = cursor_row;
        CURSOR_ROW.store(cursor_row, Ordering::Relaxed);
        Ok(())
    }

    /// Paints the whole screen in fullscreen mode: the conversation's view above, the live area
    /// on the bottom rows. Only the rows that changed are written, in one synchronized update.
    fn draw_screen(&mut self) -> Result<()> {
        if RESUMED.swap(false, Ordering::Relaxed) {
            self.shown.clear();
        }
        self.forget_stale();
        let (live, cursor_row, cursor_col) = self.live();
        let cut = live.len().saturating_sub(self.height);
        let (live, cursor_row) = (&live[cut..], cursor_row.saturating_sub(cut));
        let area = self.height - live.len();
        let scrolled = self.history.below(area) > 0 && area > 1;
        let view = area - usize::from(scrolled);
        let mut rows = self.view_lines(view);
        rows.resize(view, String::new());
        if scrolled {
            let below = self.history.below(view);
            let plural = if below == 1 { "" } else { "s" };
            rows.push(fit(
                &format!(
                    "  {FAINT}↓ {below} row{plural} below · PageDown scrolls, typing returns{RESET}"
                ),
                self.width,
            ));
        }
        self.view_rows = view;
        let cursor_row = rows.len() + cursor_row;
        rows.extend(live.iter().map(|row| text::plain(row).into_owned()));
        let whole = self.shown.len() != rows.len();
        if !whole
            && self.shown == rows
            && cursor_row == self.cursor_row
            && cursor_col == self.shown_col
        {
            return Ok(());
        }
        let mut frame: Vec<u8> = Vec::new();
        queue!(frame, BeginSynchronizedUpdate, Hide)?;
        if whole {
            queue!(frame, Clear(ClearType::All))?;
        }
        for (i, row) in rows.iter().enumerate() {
            if whole && row.is_empty() || !whole && self.shown[i] == *row {
                continue;
            }
            queue!(frame, MoveTo(0, i as u16), Clear(ClearType::CurrentLine))?;
            write!(frame, "{row}{RESET}")?;
        }
        queue!(
            frame,
            MoveTo(cursor_col as u16, cursor_row as u16),
            Show,
            EndSynchronizedUpdate
        )?;
        self.out.write_all(&frame)?;
        self.out.flush()?;
        self.shown = rows;
        self.shown_col = cursor_col;
        self.cursor_row = cursor_row;
        Ok(())
    }
}
