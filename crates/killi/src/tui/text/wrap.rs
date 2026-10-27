use super::*;

use unicode_width::UnicodeWidthChar;

/// The columns `text` takes on screen, skipping escape sequences.
pub fn width(text: &str) -> usize {
    let mut columns = 0;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        columns += c.width().unwrap_or(0);
    }
    columns
}

/// The column a tab expands to next.
const TAB: usize = 4;

/// `text` as it may go to the terminal: escape sequences (CSI, OSC, DCS and a lone ESC) and
/// C0/C1 control characters other than a line break go, and a tab becomes the spaces to the
/// next 4-column stop, counted from the text's own first column. Untrusted text is cleaned
/// here, before killi adds its own styling, so what this leaves is what `width` and the wrap
/// functions see.
pub fn sanitize(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| c.is_control() && c != '\n') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut column = 0;
    sanitize_body(text, &mut column, &mut out);
    Cow::Owned(out)
}

/// `sanitize` for a field that has one row: each line break becomes ` ⏎ ` instead.
pub fn sanitize_row(text: &str) -> Cow<'_, str> {
    let clean = sanitize(text);
    if !clean.contains('\n') {
        return clean;
    }
    Cow::Owned(clean.replace('\n', " ⏎ "))
}

/// Writes what stays of `text` to `out`, tracking the column for tab stops, and returns what
/// it left unjudged: an escape sequence the text ends in the middle of, which the caller drops
/// or holds for what follows.
fn sanitize_body<'a>(mut text: &'a str, column: &mut usize, out: &mut String) -> &'a str {
    while let Some(c) = text.chars().next() {
        let after = &text[c.len_utf8()..];
        match c {
            '\t' => {
                let spaces = TAB - *column % TAB;
                out.push_str(&" ".repeat(spaces));
                *column += spaces;
                text = after;
            }
            '\n' => {
                out.push('\n');
                *column = 0;
                text = after;
            }
            '\x1b' => match after.chars().next() {
                None => return text,
                // CSI: parameter and intermediate bytes through a final one in 0x40–0x7E. A byte
                // outside them ends the sequence without being part of it.
                Some('[') => {
                    let mut rest = &after[1..];
                    text = loop {
                        match rest.chars().next() {
                            Some(c) if ('\x30'..='\x3f').contains(&c) || ('\x20'..='\x2f').contains(&c) => rest = &rest[c.len_utf8()..],
                            Some(c) if ('\x40'..='\x7e').contains(&c) => break &rest[c.len_utf8()..],
                            Some(_) => break rest,
                            None => return text,
                        }
                    };
                }
                // OSC, DCS, SOS, PM and APC: strings through BEL or ST (ESC \).
                Some(opener @ (']' | 'P' | 'X' | '^' | '_')) => {
                    let mut rest = &after[opener.len_utf8()..];
                    text = loop {
                        match rest.chars().next() {
                            Some(c) if c == '\x07' => break &rest[c.len_utf8()..],
                            Some(c) if c == '\x1b' && rest[1..].starts_with('\\') => break &rest[2..],
                            Some(c) if c.is_control() => break rest,
                            Some(c) => rest = &rest[c.len_utf8()..],
                            None => return text,
                        }
                    };
                }
                Some(second) => text = &after[second.len_utf8()..],
            },
            c if c.is_control() => text = after,
            c => {
                out.push(c);
                *column += c.width().unwrap_or(0);
                text = after;
            }
        }
    }
    ""
}

/// `sanitize` across the deltas a reply streams in: an escape sequence one delta ends in the
/// middle of waits inside for the next, and tab stops count from the text that already went
/// through. `finish` ends the text; a sequence still waiting at the end goes, since nothing
/// the terminal should act on is left half-written.
#[derive(Default)]
pub struct Sanitizer {
    /// The tail of the last push: an escape sequence that may continue.
    held: String,
    column: usize,
}

impl Sanitizer {
    /// Cleans `delta`, holding back what may still grow into an escape sequence.
    pub fn push(&mut self, delta: &str) -> String {
        self.held.push_str(delta);
        let mut out = String::with_capacity(self.held.len());
        let rest = sanitize_body(&self.held, &mut self.column, &mut out);
        self.held = rest.to_string();
        out
    }

    /// Ends the text and gives back what `push` had held, as far as it can be judged.
    pub fn finish(&mut self) -> String {
        let held = std::mem::take(&mut self.held);
        self.column = 0;
        let mut out = String::new();
        let mut column = 0;
        sanitize_body(&held, &mut column, &mut out);
        out
    }
}

/// Splits a styled line into rows of at most `max` columns. Each row ends with a reset and
/// the next one starts with the styles that were open, so colors carry across the break.
pub fn wrap(line: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    if width(line) <= max {
        return vec![line.to_string()];
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut row_width = 0;
    let mut active = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            let mut seq = String::from(c);
            for c in chars.by_ref() {
                seq.push(c);
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            if seq == RESET {
                active.clear();
            } else {
                active.push_str(&seq);
            }
            row.push_str(&seq);
            continue;
        }
        let w = c.width().unwrap_or(0);
        if row_width + w > max {
            row.push_str(RESET);
            rows.push(std::mem::take(&mut row));
            row.push_str(&active);
            row_width = 0;
        }
        row.push(c);
        row_width += w;
    }
    rows.push(row);
    rows
}

/// Like `wrap`, but breaks at spaces where it can, so words stay whole; rows after the first
/// start with `indent` spaces. A word longer than a row is still split.
pub fn wrap_words(line: &str, max: usize, indent: usize) -> Vec<String> {
    let max = max.max(1);
    if width(line) <= max {
        return vec![line.to_string()];
    }
    let mut rows = Rows { rows: Vec::new(), row: String::new(), width: 0, active: String::new(), indent: indent.min(max / 2), max };
    let mut word: Vec<(String, usize)> = Vec::new();
    let mut spaces = 0;
    let mut placed = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            let mut seq = String::from(c);
            for c in chars.by_ref() {
                seq.push(c);
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            word.push((seq, 0));
        } else if c == ' ' {
            if rows.place(&std::mem::take(&mut word), spaces, placed) {
                spaces = 0;
                placed = true;
            }
            spaces += 1;
        } else {
            word.push((c.to_string(), c.width().unwrap_or(0)));
        }
    }
    rows.place(&word, spaces, placed);
    rows.finish()
}

/// Rows being filled by `wrap_words`.
struct Rows {
    rows: Vec<String>,
    row: String,
    width: usize,
    /// Styles open at the end of `row`.
    active: String,
    indent: usize,
    max: usize,
}

impl Rows {
    /// Places a word after `spaces` spaces, on a new row when it doesn't fit; the spaces go
    /// at a break. A word of only escape sequences takes effect without placing the spaces.
    /// Returns whether the word had any width.
    fn place(&mut self, word: &[(String, usize)], spaces: usize, placed: bool) -> bool {
        let wide: usize = word.iter().map(|(_, w)| w).sum();
        if wide == 0 {
            word.iter().for_each(|(s, _)| self.push(s, 0));
            return false;
        }
        if placed && self.width + spaces + wide > self.max {
            self.break_row();
        } else {
            self.push(&" ".repeat(spaces), spaces);
        }
        for (s, w) in word {
            if self.width + w > self.max && self.width > self.indent {
                self.break_row();
            }
            self.push(s, *w);
        }
        true
    }

    fn push(&mut self, s: &str, w: usize) {
        if s.starts_with('\x1b') {
            if s == RESET {
                self.active.clear();
            } else {
                self.active.push_str(s);
            }
        }
        self.row.push_str(s);
        self.width += w;
    }

    fn break_row(&mut self) {
        if !self.active.is_empty() {
            self.row.push_str(RESET);
        }
        self.rows.push(std::mem::take(&mut self.row));
        self.row = format!("{}{}", " ".repeat(self.indent), self.active);
        self.width = self.indent;
    }

    fn finish(mut self) -> Vec<String> {
        self.rows.push(self.row);
        self.rows
    }
}

/// A code row wrapped for `max` columns: continuation rows keep the `▏` gutter, four columns
/// wide like the indent they stand in for. Too narrow to hang the gutter, the row stays whole.
pub fn wrap_code(line: &str, max: usize) -> Vec<String> {
    if max < 8 {
        return vec![line.to_string()];
    }
    let gutter = format!("  {FAINT}▏{RESET} ");
    wrap_words(line, max, 4).into_iter().map(|row| match row.strip_prefix("    ") {
        Some(rest) => format!("{gutter}{rest}"),
        None => row,
    }).collect()
}

/// The columns a line's leading spaces and list marker take, for wrapping its later rows
/// under its text.
pub fn hanging_indent(line: &str) -> usize {
    let mut plain = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            chars.by_ref().find(char::is_ascii_alphabetic);
        } else {
            plain.push(c);
        }
    }
    let text = plain.trim_start_matches(' ');
    let lead = plain.len() - text.len();
    let marker = ["• ", "◦ ", "☐ ", "☑ ", "│ ", "▏ "].iter().find(|m| text.starts_with(**m)).map(|_| 2).or_else(|| {
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        (digits > 0 && text[digits..].starts_with(". ")).then_some(digits + 2)
    });
    lead + marker.unwrap_or(0)
}

/// `text` on one row of at most `max` columns: cut with an ellipsis where it runs out, the
/// ellipsis in the style the text had there, so a tinted row stays tinted to its end.
pub fn ellipsize(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    let mut styled = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            let mut seq = String::from(c);
            for c in chars.by_ref() {
                seq.push(c);
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            styled = seq != RESET;
            out.push_str(&seq);
            continue;
        }
        let w = c.width().unwrap_or(0);
        if used + w > max - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    if styled {
        out.push_str(RESET);
    }
    out
}
