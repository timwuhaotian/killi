//! Styled terminal text: display width, wrapping, and a line-at-a-time Markdown styler that
//! works while text is still streaming in.

use unicode_width::UnicodeWidthChar;

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const ITALIC: &str = "\x1b[3m";
pub const RED: &str = "\x1b[31m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
/// Sky blue, where the banner's gradient starts; one 256-color shade on every theme.
pub const ACCENT: &str = "\x1b[38;5;75m";
pub const MAGENTA: &str = "\x1b[35m";
pub const CYAN: &str = "\x1b[36m";

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
    let marker = ["• ", "│ "].iter().find(|m| text.starts_with(**m)).map(|_| 2).or_else(|| {
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        (digits > 0 && text[digits..].starts_with(". ")).then_some(digits + 2)
    });
    lead + marker.unwrap_or(0)
}

/// Styles complete lines of Markdown as they arrive, remembering whether a code block is open.
#[derive(Default)]
pub struct Markdown {
    in_code: bool,
}

impl Markdown {
    pub fn line(&mut self, line: &str) -> String {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            self.in_code = !self.in_code;
            return String::new();
        }
        if self.in_code {
            return format!("  {CYAN}{line}{RESET}");
        }
        if let Some(rest) = heading(trimmed) {
            return format!("{BOLD}{}{RESET}", inline(rest));
        }
        if let Some(rest) = trimmed.strip_prefix("> ") {
            return format!("{DIM}│ {}{RESET}", inline(rest));
        }
        let indent = &line[..line.len() - trimmed.len()];
        for bullet in ["- ", "* ", "+ "] {
            if let Some(rest) = trimmed.strip_prefix(bullet) {
                return format!("{indent}• {}", inline(rest));
            }
        }
        if trimmed == "---" || trimmed == "***" {
            return format!("{DIM}{}{RESET}", "─".repeat(40));
        }
        inline(line)
    }
}

fn heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    (1..=6).contains(&hashes).then(|| line[hashes..].strip_prefix(' ')).flatten()
}

/// `code`, **bold** and *italic* inside a line.
fn inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('`')
            && let Some(end) = after.find('`')
        {
            out.push_str(&format!("{CYAN}{}{RESET}", &after[..end]));
            rest = &after[end + 1..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("**")
            && let Some(end) = after.find("**")
            && end > 0
        {
            out.push_str(&format!("{BOLD}{}{RESET}", &after[..end]));
            rest = &after[end + 2..];
            continue;
        }
        let next = rest.char_indices().skip(1).find(|(_, c)| *c == '`' || *c == '*').map_or(rest.len(), |(i, _)| i);
        out.push_str(&rest[..next]);
        rest = &rest[next..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_words_breaks_at_spaces_and_keeps_styles() {
        assert_eq!(wrap_words("hello brave new world", 11, 0), ["hello brave", "new world"]);
        assert_eq!(wrap_words("• one two three four", 11, 2), ["• one two", "  three", "  four"]);
        assert_eq!(wrap_words("  indented words here", 12, 2), ["  indented", "  words here"]);
        let rows = wrap_words(&format!("a {BOLD}bold words{RESET} end"), 9, 0);
        assert_eq!(rows, [format!("a {BOLD}bold{RESET}"), format!("{BOLD}words{RESET} end")]);
        assert_eq!(wrap_words("abcdefghij", 4, 0), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn hanging_indent_follows_the_list_marker() {
        assert_eq!(hanging_indent("plain"), 0);
        assert_eq!(hanging_indent(&format!("  • {BOLD}item{RESET}")), 4);
        assert_eq!(hanging_indent("12. step"), 4);
    }

    #[test]
    fn width_skips_escapes_and_counts_wide_characters() {
        assert_eq!(width(&format!("{BOLD}ab{RESET}")), 2);
        assert_eq!(width("中文"), 4);
    }

    #[test]
    fn wrap_carries_styles_across_rows() {
        let rows = wrap(&format!("{RED}abcdef{RESET}"), 4);
        assert_eq!(rows.len(), 2);
        assert_eq!(width(&rows[0]), 4);
        assert!(rows[1].starts_with(RED));
        assert_eq!(wrap("中文中文", 5).len(), 2);
    }

    #[test]
    fn markdown_styles_lines() {
        let mut md = Markdown::default();
        assert_eq!(md.line("## Title"), format!("{BOLD}Title{RESET}"));
        assert_eq!(md.line("- use `cargo` **now**"), format!("• use {CYAN}cargo{RESET} {BOLD}now{RESET}"));
        assert_eq!(md.line("```rust"), "", "a fence becomes a blank line around the block");
        assert_eq!(md.line("let x = 1;"), format!("  {CYAN}let x = 1;{RESET}"));
        md.line("```");
        assert_eq!(md.line("a * b"), "a * b");
    }
}
