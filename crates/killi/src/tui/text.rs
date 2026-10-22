//! Styled terminal text: display width, wrapping, and a line-at-a-time Markdown styler that
//! works while text is still streaming in.

use std::borrow::Cow;
use std::fmt;
use std::sync::OnceLock;

use unicode_width::UnicodeWidthChar;

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const ITALIC: &str = "\x1b[3m";
pub const UNDERLINE: &str = "\x1b[4m";
pub const STRIKE: &str = "\x1b[9m";

/// The terminal's background: it decides which shade each color takes.
#[derive(Clone, Copy, PartialEq)]
enum Theme {
    Dark,
    Light,
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// Fixes the theme for the session: `"dark"` or `"light"`, anything else detects it from
/// `COLORFGBG`, which most terminals that have a light background set.
pub fn init_theme(setting: Option<&str>) {
    let theme = match setting {
        Some("dark") => Theme::Dark,
        Some("light") => Theme::Light,
        _ => detect_theme(),
    };
    let _ = THEME.set(theme);
}

fn detect_theme() -> Theme {
    let background = std::env::var("COLORFGBG").ok().and_then(|v| v.rsplit(';').next().and_then(|bg| bg.parse::<u8>().ok()));
    match background {
        Some(7 | 9..=15) => Theme::Light,
        _ => Theme::Dark,
    }
}

/// A color escape that comes out in the shade for the current theme when it is printed.
#[derive(Clone, Copy)]
pub struct Sgr {
    dark: &'static str,
    light: &'static str,
}

impl fmt::Display for Sgr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match *THEME.get_or_init(detect_theme) {
            Theme::Dark => self.dark,
            Theme::Light => self.light,
        })
    }
}

// The roles of the design system (`docs/killi-design-system.html`), each the nearest
// 256-color shade of its token so they read the same in every terminal.

/// Killi: the prompt caret, running and finished work, the active mode, commands and links.
pub const LAGOON: Sgr = Sgr { dark: "\x1b[38;5;80m", light: "\x1b[38;5;30m" };
/// The human: your messages, questions and anything else waiting on you.
pub const FIN: Sgr = Sgr { dark: "\x1b[38;5;209m", light: "\x1b[38;5;130m" };
/// Retries, a context window nearly full, an interrupted turn.
pub const WARN: Sgr = Sgr { dark: "\x1b[38;5;221m", light: "\x1b[38;5;136m" };
/// Failed tool calls, errors and removed lines.
pub const DANGER: Sgr = Sgr { dark: "\x1b[38;5;204m", light: "\x1b[38;5;124m" };
/// Marks the eye may skip: gutters, rules, the empty part of a meter.
pub const FAINT: Sgr = Sgr { dark: "\x1b[38;5;242m", light: "\x1b[38;5;245m" };
/// The thinking effort in the status line and the banner.
pub const EFFORT: Sgr = Sgr { dark: "\x1b[38;5;105m", light: "\x1b[38;5;61m" };
/// Token counts and the cache hit rate in the status line.
pub const TOKENS: Sgr = Sgr { dark: "\x1b[38;5;75m", light: "\x1b[38;5;25m" };
/// The git branch in the status line.
pub const BRANCH: Sgr = Sgr { dark: "\x1b[38;5;176m", light: "\x1b[38;5;127m" };
/// A background a step off the terminal's own, for the selected row and your messages.
pub const TINT: Sgr = Sgr { dark: "\x1b[48;5;237m", light: "\x1b[48;5;254m" };

// Syntax colors for code blocks only; they carry no meaning in the rest of the UI.
const STRING: Sgr = Sgr { dark: "\x1b[32m", light: "\x1b[38;5;28m" };
const NUMBER: Sgr = Sgr { dark: "\x1b[33m", light: "\x1b[38;5;130m" };
const KEYWORD: Sgr = Sgr { dark: "\x1b[35m", light: "\x1b[38;5;90m" };

/// Whether `NO_COLOR` asks for no colors.
fn no_color() -> bool {
    static NO_COLOR: OnceLock<bool> = OnceLock::new();
    *NO_COLOR.get_or_init(|| std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()))
}

/// `line` as it goes to the terminal: without its colors when `NO_COLOR` is set, keeping
/// bold, dim and the other text styles.
pub fn plain(line: &str) -> Cow<'_, str> {
    if no_color() { strip_colors(line) } else { Cow::Borrowed(line) }
}

fn strip_colors(line: &str) -> Cow<'_, str> {
    if !line.contains("\x1b[") {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("\x1b[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find(|c: char| c.is_ascii_alphabetic()) else {
            out.push_str(&rest[start..]);
            return Cow::Owned(out);
        };
        let (params, kind) = (&after[..end], after.as_bytes()[end] as char);
        rest = &after[end + 1..];
        if kind != 'm' {
            out.push_str(&format!("\x1b[{params}{kind}"));
            continue;
        }
        let kept = without_colors(params);
        if !kept.is_empty() {
            out.push_str(&format!("\x1b[{kept}m"));
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// The SGR parameters left once every foreground and background color is dropped.
fn without_colors(params: &str) -> String {
    if params.is_empty() {
        return "0".into();
    }
    let mut kept = Vec::new();
    let mut parts = params.split(';');
    while let Some(part) = parts.next() {
        match part {
            "38" | "48" => match parts.next() {
                Some("5") => {
                    parts.next();
                }
                Some("2") => {
                    parts.by_ref().take(3).for_each(drop);
                }
                _ => {}
            },
            _ if matches!(part.parse::<u32>(), Ok(30..=39 | 40..=49 | 90..=97 | 100..=107)) => {}
            _ => kept.push(part),
        }
    }
    kept.join(";")
}

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
    let marker = ["• ", "◦ ", "☐ ", "☑ ", "│ ", "▏ "].iter().find(|m| text.starts_with(**m)).map(|_| 2).or_else(|| {
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        (digits > 0 && text[digits..].starts_with(". ")).then_some(digits + 2)
    });
    lead + marker.unwrap_or(0)
}

/// Styles complete lines of Markdown as they arrive. It remembers whether a code block is
/// open, and holds back table rows until the table ends, so the columns can line up.
#[derive(Default, Clone)]
pub struct Markdown {
    /// The terminal's width, for rules and tables; zero until the UI sets it.
    pub width: usize,
    in_code: bool,
    lang: Lang,
    table: Vec<String>,
}

impl Markdown {
    pub fn new(width: usize) -> Self {
        Self { width, ..Self::default() }
    }

    /// The rows `line` adds. A table row adds none until a line that isn't one follows.
    pub fn line(&mut self, line: &str) -> Vec<String> {
        let trimmed = line.trim_start();
        if !self.in_code && is_table_row(trimmed) {
            self.table.push(trimmed.to_string());
            return Vec::new();
        }
        let mut rows = self.finish();
        rows.push(self.block(line, trimmed));
        rows
    }

    /// The rows of a table still being held, at the end of the text or a blank line.
    pub fn finish(&mut self) -> Vec<String> {
        if self.table.is_empty() {
            return Vec::new();
        }
        render_table(&std::mem::take(&mut self.table), self.width)
    }

    fn block(&mut self, line: &str, trimmed: &str) -> String {
        if let Some(info) = trimmed.strip_prefix("```") {
            let lang = info.trim().split(|c: char| c.is_whitespace() || c == ',').next().unwrap_or("");
            self.in_code = !self.in_code;
            self.lang = if self.in_code { Lang::of(lang) } else { Lang::Plain };
            let label = if self.in_code { lang } else { "" };
            return format!("  {FAINT}▏{RESET} {DIM}{label}{RESET}");
        }
        if self.in_code {
            let code = match self.lang {
                Lang::Plain => line.to_string(),
                lang => highlight(lang, line),
            };
            return format!("  {FAINT}▏{RESET} {code}");
        }
        if let Some((level, rest)) = heading(trimmed) {
            let color = if level <= 2 { LAGOON.to_string() } else { String::new() };
            return format!("{BOLD}{color}{}{RESET}", inline(rest));
        }
        if let Some(rest) = trimmed.strip_prefix("> ") {
            return format!("{FAINT}│{RESET} {DIM}{}{RESET}", inline(rest));
        }
        let indent = &line[..line.len() - trimmed.len()];
        for bullet in ["- ", "* ", "+ "] {
            if let Some(rest) = trimmed.strip_prefix(bullet) {
                let (mark, rest) = match rest {
                    r if r.starts_with("[ ] ") => ("☐", &r[4..]),
                    r if r.starts_with("[x] ") || r.starts_with("[X] ") => ("☑", &r[4..]),
                    r => (if indent.is_empty() { "•" } else { "◦" }, r),
                };
                return format!("{indent}{mark} {}", inline(rest));
            }
        }
        let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && trimmed[digits..].starts_with(". ") {
            return format!("{indent}{DIM}{}.{RESET} {}", &trimmed[..digits], inline(&trimmed[digits + 2..]));
        }
        if matches!(trimmed, "---" | "***" | "___") {
            return format!("{FAINT}{}{RESET}", "─".repeat(self.width.clamp(10, 60)));
        }
        inline(line)
    }
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    (1..=6).contains(&hashes).then(|| line[hashes..].strip_prefix(' ')).flatten().map(|rest| (hashes, rest))
}

/// `code`, **bold**, *italic*, ~~struck~~ and [links](url) inside a line.
fn inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('`')
            && let Some(end) = after.find('`')
        {
            out.push_str(&format!("{LAGOON}{}{RESET}", &after[..end]));
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
        if let Some(after) = rest.strip_prefix("~~")
            && let Some(end) = after.find("~~")
            && end > 0
        {
            out.push_str(&format!("{STRIKE}{}{RESET}", &after[..end]));
            rest = &after[end + 2..];
            continue;
        }
        if let Some(after) = rest.strip_prefix('*')
            && !after.starts_with(['*', ' '])
            && let Some(end) = after.find('*')
            && end > 0
            && !after[..end].ends_with(' ')
        {
            out.push_str(&format!("{ITALIC}{}{RESET}", &after[..end]));
            rest = &after[end + 1..];
            continue;
        }
        if rest.starts_with('[')
            && let Some((label, url, used)) = link(rest)
        {
            if url.is_empty() || label == url {
                out.push_str(&format!("{LAGOON}{UNDERLINE}{label}{RESET}"));
            } else {
                out.push_str(&format!("{LAGOON}{UNDERLINE}{label}{RESET} {DIM}({url}){RESET}"));
            }
            rest = &rest[used..];
            continue;
        }
        let next = rest.char_indices().skip(1).find(|(_, c)| matches!(c, '`' | '*' | '~' | '[')).map_or(rest.len(), |(i, _)| i);
        out.push_str(&rest[..next]);
        rest = &rest[next..];
    }
    out
}

/// The label, the address and the bytes used by a `[label](url)` at the start of `text`.
fn link(text: &str) -> Option<(&str, &str, usize)> {
    let close = text.find("](")?;
    let label = &text[1..close];
    if label.contains('[') || label.is_empty() {
        return None;
    }
    let tail = &text[close + 2..];
    let end = tail.find(')')?;
    Some((label, &tail[..end], close + 2 + end + 1))
}

fn is_table_row(line: &str) -> bool {
    line.starts_with('|') && line[1..].contains('|')
}

fn cells(row: &str) -> Vec<String> {
    let row = row.trim();
    let row = row.strip_prefix('|').unwrap_or(row);
    let row = row.strip_suffix('|').unwrap_or(row);
    row.split('|').map(|cell| cell.trim().to_string()).collect()
}

fn is_separator(cells: &[String]) -> bool {
    cells.iter().all(|c| {
        let c = c.trim_matches(':');
        !c.is_empty() && c.chars().all(|ch| ch == '-')
    })
}

/// `text` cut to `max` columns with an ellipsis, keeping its styles.
fn cut(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut row = wrap(text, max - 1).swap_remove(0);
    row.push('…');
    row
}

/// A table as aligned rows: a bold header over a rule, columns shrunk to fit `max_width`.
fn render_table(rows: &[String], max_width: usize) -> Vec<String> {
    let mut parsed: Vec<Vec<String>> = rows.iter().map(|r| cells(r)).collect();
    let header = parsed.len() > 1 && is_separator(&parsed[1]);
    let aligns: Vec<char> = if header {
        parsed[1]
            .iter()
            .map(|c| match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => 'c',
                (false, true) => 'r',
                _ => 'l',
            })
            .collect()
    } else {
        Vec::new()
    };
    if header {
        parsed.remove(1);
    }
    let count = parsed.iter().map(Vec::len).max().unwrap_or(0);
    let styled: Vec<Vec<String>> = parsed
        .iter()
        .map(|row| (0..count).map(|i| row.get(i).map_or(String::new(), |c| inline(c))).collect())
        .collect();
    let mut widths: Vec<usize> = (0..count).map(|i| styled.iter().map(|row| width(&row[i])).max().unwrap_or(0)).collect();
    let overhead = 2 + 3 * count.saturating_sub(1);
    if max_width > 0 {
        while widths.iter().sum::<usize>() + overhead > max_width {
            let Some((widest, w)) = widths.iter().copied().enumerate().max_by_key(|&(_, w)| w) else { break };
            if w <= 3 {
                break;
            }
            widths[widest] -= 1;
        }
    }
    let line = |row: &[String], bold: bool| {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let cell = cut(cell, widths[i]);
                let cell = if bold { format!("{BOLD}{cell}{RESET}") } else { cell };
                let pad = widths[i].saturating_sub(width(&cell));
                match aligns.get(i) {
                    Some('r') => format!("{}{cell}", " ".repeat(pad)),
                    Some('c') => format!("{}{cell}{}", " ".repeat(pad / 2), " ".repeat(pad - pad / 2)),
                    _ => format!("{cell}{}", " ".repeat(pad)),
                }
            })
            .collect();
        format!("  {}", cells.join(&format!(" {FAINT}│{RESET} ")))
    };
    let mut out = Vec::new();
    for (n, row) in styled.iter().enumerate() {
        out.push(line(row, header && n == 0));
        if header && n == 0 {
            let rule: Vec<String> = widths.iter().map(|&w| "─".repeat(w)).collect();
            out.push(format!("  {FAINT}{}{RESET}", rule.join("─┼─")));
        }
    }
    out
}

/// A language whose code blocks get colors.
#[derive(Default, Clone, Copy, PartialEq)]
enum Lang {
    #[default]
    Plain,
    Rust,
    Js,
    Py,
    Sh,
    Go,
    Json,
    Config,
    Diff,
}

impl Lang {
    fn of(info: &str) -> Lang {
        match info.to_lowercase().as_str() {
            "rust" | "rs" => Lang::Rust,
            "js" | "javascript" | "jsx" | "mjs" | "ts" | "typescript" | "tsx" => Lang::Js,
            "py" | "python" => Lang::Py,
            "sh" | "bash" | "zsh" | "shell" | "console" => Lang::Sh,
            "go" | "golang" => Lang::Go,
            "json" | "jsonc" => Lang::Json,
            "toml" | "yaml" | "yml" | "ini" => Lang::Config,
            "diff" | "patch" => Lang::Diff,
            _ => Lang::Plain,
        }
    }

    fn keywords(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &[
                "fn", "let", "mut", "pub", "struct", "enum", "impl", "trait", "use", "mod", "match", "if", "else", "for", "while", "loop", "return",
                "self", "Self", "async", "await", "move", "const", "static", "type", "where", "crate", "super", "as", "in", "ref", "dyn", "unsafe",
                "break", "continue", "true", "false",
            ],
            Lang::Js => &[
                "function", "const", "let", "var", "return", "if", "else", "for", "while", "class", "new", "import", "export", "from", "default",
                "async", "await", "try", "catch", "throw", "switch", "case", "break", "continue", "typeof", "interface", "type", "extends", "true",
                "false", "null", "undefined", "this", "of", "in",
            ],
            Lang::Py => &[
                "def", "class", "return", "if", "elif", "else", "for", "while", "import", "from", "as", "with", "try", "except", "finally", "raise",
                "lambda", "pass", "break", "continue", "yield", "async", "await", "in", "not", "and", "or", "is", "None", "True", "False", "self",
            ],
            Lang::Sh => &["if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac", "function", "in", "echo", "cd", "export", "return", "local"],
            Lang::Go => &[
                "func", "var", "const", "type", "struct", "interface", "package", "import", "return", "if", "else", "for", "range", "switch", "case",
                "default", "go", "defer", "chan", "map", "select", "nil", "true", "false", "break", "continue",
            ],
            Lang::Json | Lang::Config => &["true", "false", "null"],
            Lang::Plain | Lang::Diff => &[],
        }
    }
}

/// One line of code with comments, strings, numbers and keywords colored.
fn highlight(lang: Lang, line: &str) -> String {
    if lang == Lang::Diff {
        let color = match line.chars().next() {
            Some('+') if !line.starts_with("+++") => LAGOON.to_string(),
            Some('-') if !line.starts_with("---") => DANGER.to_string(),
            _ => DIM.to_string(),
        };
        return format!("{color}{line}{RESET}");
    }
    let comment = match lang {
        Lang::Rust | Lang::Js | Lang::Go => Some("//"),
        Lang::Py | Lang::Sh | Lang::Config => Some("#"),
        _ => None,
    };
    let quotes: &[char] = match lang {
        Lang::Rust | Lang::Json => &['"'],
        Lang::Js | Lang::Go => &['"', '\'', '`'],
        _ => &['"', '\''],
    };
    let words = lang.keywords();
    let chars: Vec<char> = line.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(marker) = comment
            && chars[i..].iter().take(marker.len()).collect::<String>() == marker
        {
            out.push_str(&format!("{DIM}{}{RESET}", chars[i..].iter().collect::<String>()));
            break;
        }
        if quotes.contains(&c) {
            let mut end = i + 1;
            while end < chars.len() && chars[end] != c {
                end += if chars[end] == '\\' { 2 } else { 1 };
            }
            let end = (end + 1).min(chars.len());
            out.push_str(&format!("{STRING}{}{RESET}", chars[i..end].iter().collect::<String>()));
            i = end;
            continue;
        }
        if c.is_ascii_digit() && (i == 0 || !is_word(chars[i - 1])) {
            let end = chars[i..].iter().position(|&ch| !(ch.is_alphanumeric() || ch == '.' || ch == '_')).map_or(chars.len(), |p| i + p);
            out.push_str(&format!("{NUMBER}{}{RESET}", chars[i..end].iter().collect::<String>()));
            i = end;
            continue;
        }
        if is_word(c) {
            let end = chars[i..].iter().position(|&ch| !is_word(ch)).map_or(chars.len(), |p| i + p);
            let word: String = chars[i..end].iter().collect();
            if words.contains(&word.as_str()) {
                out.push_str(&format!("{KEYWORD}{word}{RESET}"));
            } else {
                out.push_str(&word);
            }
            i = end;
            continue;
        }
        out.push(c);
        i += 1;
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
        let rows = wrap(&format!("{DANGER}abcdef{RESET}"), 4);
        assert_eq!(rows.len(), 2);
        assert_eq!(width(&rows[0]), 4);
        assert!(rows[1].starts_with(&DANGER.to_string()));
        assert_eq!(wrap("中文中文", 5).len(), 2);
    }

    fn lines(md: &mut Markdown, text: &str) -> Vec<String> {
        let mut rows: Vec<String> = text.lines().flat_map(|l| md.line(l)).collect();
        rows.extend(md.finish());
        rows
    }

    #[test]
    fn markdown_styles_lines() {
        let mut md = Markdown::default();
        assert_eq!(md.line("## Title"), [format!("{BOLD}{LAGOON}Title{RESET}")]);
        assert_eq!(md.line("### Small"), [format!("{BOLD}Small{RESET}")]);
        assert_eq!(md.line("- use `cargo` **now**"), [format!("• use {LAGOON}cargo{RESET} {BOLD}now{RESET}")]);
        assert_eq!(md.line("  - nested"), ["  ◦ nested"]);
        assert_eq!(md.line("- [x] done"), ["☑ done"]);
        assert_eq!(md.line("2. second"), [format!("{DIM}2.{RESET} second")]);
        assert_eq!(md.line("```"), [format!("  {FAINT}▏{RESET} {DIM}{RESET}")]);
        assert_eq!(md.line("let x = 1;"), [format!("  {FAINT}▏{RESET} let x = 1;")]);
        md.line("```");
        assert_eq!(md.line("a * b"), ["a * b"]);
        assert_eq!(md.line("some *emphasis* here"), [format!("some {ITALIC}emphasis{RESET} here")]);
        assert_eq!(md.line("~~gone~~"), [format!("{STRIKE}gone{RESET}")]);
    }

    #[test]
    fn markdown_links_show_their_address() {
        let mut md = Markdown::default();
        assert_eq!(md.line("see [docs](https://a.b/c)"), [format!("see {LAGOON}{UNDERLINE}docs{RESET} {DIM}(https://a.b/c){RESET}")]);
        assert_eq!(md.line("[https://a.b](https://a.b)"), [format!("{LAGOON}{UNDERLINE}https://a.b{RESET}")]);
        assert_eq!(md.line("[not a link] (x)"), ["[not a link] (x)"]);
    }

    #[test]
    fn code_blocks_are_colored_by_language() {
        let mut md = Markdown::default();
        let rows = lines(&mut md, "```rust\nlet n = 42; // note\n```");
        assert_eq!(rows[0], format!("  {FAINT}▏{RESET} {DIM}rust{RESET}"));
        assert!(rows[1].contains(&format!("{KEYWORD}let{RESET}")));
        assert!(rows[1].contains(&format!("{NUMBER}42{RESET}")));
        assert!(rows[1].contains(&format!("{DIM}// note{RESET}")));
        let rows = lines(&mut md, "```diff\n+new\n-old\n```");
        assert!(rows[1].contains(&format!("{LAGOON}+new")) && rows[2].contains(&format!("{DANGER}-old")));
    }

    #[test]
    fn tables_line_up_and_shrink_to_the_width() {
        let mut md = Markdown::new(40);
        assert!(md.line("| a | bb |").is_empty(), "rows wait for the end of the table");
        md.line("|---|---:|");
        md.line("| long cell | 1 |");
        let rows = md.line("after");
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[3], "after");
        assert!(rows[..3].iter().all(|r| width(r) == width(&rows[0])));
        assert!(rows[1].contains('┼'));
        let mut narrow = Markdown::new(14);
        narrow.line("| aaaaaaaaaa | bbbbbbbbbb |");
        let rows = narrow.finish();
        assert!(rows.iter().all(|r| width(r) <= 14), "{rows:?}");
    }

    #[test]
    fn no_color_drops_colors_and_keeps_styles() {
        let line = format!("{BOLD}{LAGOON}x{RESET}\x1b[38;2;1;2;3m\x1b[48;5;9my\x1b[1;32mz{DIM}");
        assert_eq!(strip_colors(&line), format!("{BOLD}x{RESET}y\x1b[1mz{DIM}"));
        assert_eq!(strip_colors("plain"), "plain");
    }
}
