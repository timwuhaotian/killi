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
pub const BLUE: &str = "\x1b[34m";
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
            return format!("{DIM}{line}{RESET}");
        }
        if self.in_code {
            return format!("{CYAN}{line}{RESET}");
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
        assert!(md.line("```rust").starts_with(DIM));
        assert_eq!(md.line("let x = 1;"), format!("{CYAN}let x = 1;{RESET}"));
        md.line("```");
        assert_eq!(md.line("a * b"), "a * b");
    }
}
