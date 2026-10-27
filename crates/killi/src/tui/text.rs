//! Styled terminal text: display width, wrapping, and a line-at-a-time Markdown styler that
//! works while text is still streaming in.

use std::borrow::Cow;
use std::fmt;
use std::sync::OnceLock;

use unicode_width::UnicodeWidthChar;

use super::palette::{self, Ground, Role};

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
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

/// Fixes the theme for the session: `"dark"` or `"light"`, anything else goes by the
/// terminal's `ground` when it answered the color queries, else by `COLORFGBG`, which most
/// terminals that have a light background set. A ground also gives the grays, tints and stripes
/// their shades (see `palette`); a pinned theme has none, and keeps the fixed shades.
pub fn init_theme(setting: Option<&str>, ground: Option<Ground>) {
    let pinned = matches!(setting, Some("dark" | "light"));
    let ground = ground.filter(|_| !pinned);
    let theme = match setting {
        Some("dark") => Theme::Dark,
        Some("light") => Theme::Light,
        _ => ground.map_or_else(detect_theme, |ground| theme_for(ground.background)),
    };
    let _ = THEME.set(theme);
    palette::init(ground);
}

/// The theme for a background color: light when it reflects more than half the light.
fn theme_for((r, g, b): (u8, u8, u8)) -> Theme {
    let luminance = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b);
    if luminance > 127.5 { Theme::Light } else { Theme::Dark }
}

fn detect_theme() -> Theme {
    let background = std::env::var("COLORFGBG").ok().and_then(|v| v.rsplit(';').next().and_then(|bg| bg.parse::<u8>().ok()));
    match background {
        Some(7 | 9..=15) => Theme::Light,
        _ => Theme::Dark,
    }
}

/// Whether the session runs on a light terminal.
pub fn is_light() -> bool {
    *THEME.get_or_init(detect_theme) == Theme::Light
}

/// Whether the terminal takes 24-bit colors, as `COLORTERM` announces.
pub fn truecolor() -> bool {
    static TRUECOLOR: OnceLock<bool> = OnceLock::new();
    *TRUECOLOR.get_or_init(|| std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit"))
}

/// A color escape that comes out in the shade for the current theme when it is printed: the
/// design token itself on a truecolor terminal, else its nearest 256-color shade. A derived
/// role takes the shade `palette` made from the terminal's ground when there is one.
#[derive(Clone, Copy)]
pub struct Sgr {
    dark: &'static str,
    light: &'static str,
    dark_true: &'static str,
    light_true: &'static str,
    derived: Option<Role>,
}

impl Sgr {
    const fn fixed(dark: &'static str, light: &'static str, dark_true: &'static str, light_true: &'static str) -> Sgr {
        Sgr { dark, light, dark_true, light_true, derived: None }
    }

    /// A role that follows the ground; the fixed shades are for when the terminal gave none.
    const fn derived(role: Role, dark: &'static str, light: &'static str, dark_true: &'static str, light_true: &'static str) -> Sgr {
        Sgr { dark, light, dark_true, light_true, derived: Some(role) }
    }

    fn shade(&self, theme: Theme, truecolor: bool) -> &'static str {
        match (theme, truecolor) {
            (Theme::Dark, true) => self.dark_true,
            (Theme::Dark, false) => self.dark,
            (Theme::Light, true) => self.light_true,
            (Theme::Light, false) => self.light,
        }
    }
}

impl fmt::Display for Sgr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.derived {
            // Colors are about to be stripped; what recedes keeps receding, as dim.
            Some(Role::Faint | Role::Rule | Role::Muted) if no_color() => return f.write_str("\x1b[2m"),
            Some(role) if !no_color() => {
                if let Some(palette) = palette::get()
                    && let Some(written) = palette.write(f, role, truecolor())
                {
                    return written;
                }
            }
            _ => {}
        }
        f.write_str(self.shade(*THEME.get_or_init(detect_theme), truecolor()))
    }
}

// The roles of the design system (`docs/killi-design-system.html`): each token's exact color
// where the terminal shows 24-bit color, else its nearest 256-color shade.

/// Killi: running work, the active mode, commands, links and the selected row's mark.
pub const LAGOON: Sgr = Sgr::fixed("\x1b[38;5;80m", "\x1b[38;5;23m", "\x1b[38;2;79;209;197m", "\x1b[38;2;10;110;101m");
/// The human: your messages, the prompt, questions and anything else waiting on you.
pub const FIN: Sgr = Sgr::fixed("\x1b[38;5;209m", "\x1b[38;5;130m", "\x1b[38;2;255;140;90m", "\x1b[38;2;173;63;20m");
/// Retries, a context window nearly full, an interrupted turn.
pub const WARN: Sgr = Sgr::fixed("\x1b[38;5;221m", "\x1b[38;5;94m", "\x1b[38;2;242;193;78m", "\x1b[38;2;138;90;0m");
/// Failed tool calls, errors and removed lines.
pub const DANGER: Sgr = Sgr::fixed("\x1b[38;5;204m", "\x1b[38;5;124m", "\x1b[38;2;255;111;125m", "\x1b[38;2;179;38;30m");
/// Marks the eye may skip: a finished call's tick, gutters, hints, the empty part of a meter.
pub const FAINT: Sgr = Sgr::derived(Role::Faint, "\x1b[38;5;242m", "\x1b[38;5;245m", "\x1b[38;2;92;107;122m", "\x1b[38;2;117;131;138m");
/// Hairlines: the rules around the prompt. As faint as a mark until the ground is known.
pub const RULE: Sgr = Sgr::derived(Role::Rule, "\x1b[38;5;242m", "\x1b[38;5;245m", "\x1b[38;2;92;107;122m", "\x1b[38;2;117;131;138m");
/// Secondary text. SGR dim is the fallback, not the design: Kil and Alacritty scale each
/// channel to two thirds, which makes dim text darker than body text on a light theme.
pub const DIM: Sgr = Sgr::derived(Role::Muted, "\x1b[2m", "\x1b[2m", "\x1b[2m", "\x1b[2m");
/// The thinking effort in the status line and the banner.
pub const EFFORT: Sgr = Sgr::fixed("\x1b[38;5;147m", "\x1b[38;5;60m", "\x1b[38;2;170;169;235m", "\x1b[38;2;94;90;154m");
/// Token counts and the cache hit rate in the status line.
pub const TOKENS: Sgr = Sgr::fixed("\x1b[38;5;110m", "\x1b[38;5;24m", "\x1b[38;2;129;188;229m", "\x1b[38;2;43;105;145m");
/// The git branch in the status line.
pub const BRANCH: Sgr = Sgr::fixed("\x1b[38;5;182m", "\x1b[38;5;96m", "\x1b[38;2;215;160;202m", "\x1b[38;2;134;76;122m");
/// A background a step off the terminal's own, for your messages.
pub const TINT: Sgr = Sgr::derived(Role::Tint, "\x1b[48;5;237m", "\x1b[48;5;254m", "\x1b[48;2;23;30;35m", "\x1b[48;2;223;229;226m");
/// The selected row of a list: a little more than a tint, with more lagoon in it.
pub const SELECT: Sgr = Sgr::derived(Role::Select, "\x1b[48;5;237m", "\x1b[48;5;254m", "\x1b[48;2;27;41;45m", "\x1b[48;2;210;220;217m");
/// Behind an added line of a diff. Only drawn in 24-bit color, where it can be this soft.
pub const ADD: Sgr = Sgr::derived(Role::Add, "\x1b[49m", "\x1b[49m", "\x1b[48;2;21;35;37m", "\x1b[48;2;224;239;231m");
/// Behind a removed line of a diff.
pub const DEL: Sgr = Sgr::derived(Role::Del, "\x1b[49m", "\x1b[49m", "\x1b[48;2;41;30;35m", "\x1b[48;2;248;229;226m");
/// Behind inline code, in 24-bit color.
pub const CHIP: Sgr = Sgr::derived(Role::Chip, "\x1b[49m", "\x1b[49m", "\x1b[48;2;30;35;40m", "\x1b[48;2;217;219;218m");

// Syntax colors for code blocks only; they carry no meaning in the rest of the UI.
const STRING: Sgr = Sgr::fixed("\x1b[32m", "\x1b[38;5;28m", "\x1b[32m", "\x1b[38;5;28m");
const NUMBER: Sgr = Sgr::fixed("\x1b[33m", "\x1b[38;5;130m", "\x1b[33m", "\x1b[38;5;130m");
const KEYWORD: Sgr = Sgr::fixed("\x1b[35m", "\x1b[38;5;90m", "\x1b[35m", "\x1b[38;5;90m");

/// Whether `NO_COLOR` asks for no colors.
pub fn no_color() -> bool {
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

/// What a line of Markdown adds: a styled row, a code row wrapped with its gutter kept, or
/// a finished table, left as its source rows so it can be laid out for any width.
pub enum Part {
    Row(String),
    Code(String),
    Table(Vec<String>),
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
    /// Bumped on every styling step, so a cached preview knows the state it was made from.
    generation: u64,
}

impl Markdown {
    pub fn new(width: usize) -> Self {
        Self { width, ..Self::default() }
    }

    /// What `line` adds. A table row adds nothing until a line that isn't one follows.
    pub fn parts(&mut self, line: &str) -> Vec<Part> {
        self.generation += 1;
        let trimmed = line.trim_start();
        if !self.in_code && is_table_row(trimmed) {
            self.table.push(trimmed.to_string());
            return Vec::new();
        }
        let mut parts = self.finish_parts();
        if let Some(part) = self.block(line, trimmed) {
            parts.push(part);
        }
        parts
    }

    /// The table still being held, at the end of the text or a blank line.
    pub fn finish_parts(&mut self) -> Vec<Part> {
        self.generation += 1;
        if self.table.is_empty() {
            return Vec::new();
        }
        vec![Part::Table(std::mem::take(&mut self.table))]
    }

    /// The message is over: lays out the table still being held, and closes an unclosed
    /// fence, so the next message doesn't inherit this one's code styling.
    pub fn end_message(&mut self) -> Vec<Part> {
        self.generation += 1;
        let parts = self.finish_parts();
        self.in_code = false;
        self.lang = Lang::Plain;
        parts
    }

    /// How many styling steps went into the state: a preview styled from it is fresh while
    /// this, its text and its width are unchanged.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The rows `line` adds, a table laid out for `width`.
    pub fn line(&mut self, line: &str) -> Vec<String> {
        let parts = self.parts(line);
        self.rows(parts)
    }

    /// The rows of the table still being held, laid out for `width`.
    pub fn finish(&mut self) -> Vec<String> {
        let parts = self.finish_parts();
        self.rows(parts)
    }

    fn rows(&self, parts: Vec<Part>) -> Vec<String> {
        parts
            .into_iter()
            .flat_map(|part| match part {
                Part::Row(row) => vec![row],
                Part::Code(row) => wrap_code(&row, self.width),
                Part::Table(rows) => render_table(&rows, self.width),
            })
            .collect()
    }

    /// The row a line becomes; None for a code fence with nothing to say (no language, or the
    /// one that closes the block), so the block starts and ends on its code.
    fn block(&mut self, line: &str, trimmed: &str) -> Option<Part> {
        if let Some(info) = trimmed.strip_prefix("```") {
            let lang = info.trim().split(|c: char| c.is_whitespace() || c == ',').next().unwrap_or("");
            self.in_code = !self.in_code;
            self.lang = if self.in_code { Lang::of(lang) } else { Lang::Plain };
            let label = if self.in_code { lang } else { "" };
            return (!label.is_empty()).then(|| Part::Row(format!("  {FAINT}▏{RESET} {DIM}{label}{RESET}")));
        }
        if self.in_code {
            let code = match self.lang {
                Lang::Plain => line.to_string(),
                lang => highlight(lang, line),
            };
            return Some(Part::Code(format!("  {FAINT}▏{RESET} {code}")));
        }
        if let Some((level, rest)) = heading(trimmed) {
            let color = if level <= 2 { LAGOON.to_string() } else { String::new() };
            return Some(Part::Row(format!("{BOLD}{color}{}{RESET}", inline(rest))));
        }
        if let Some(rest) = trimmed.strip_prefix("> ") {
            return Some(Part::Row(format!("{FAINT}│{RESET} {DIM}{}{RESET}", inline(rest))));
        }
        let indent = &line[..line.len() - trimmed.len()];
        for bullet in ["- ", "* ", "+ "] {
            if let Some(rest) = trimmed.strip_prefix(bullet) {
                let (mark, rest) = match rest {
                    r if r.starts_with("[ ] ") => ("☐", &r[4..]),
                    r if r.starts_with("[x] ") || r.starts_with("[X] ") => ("☑", &r[4..]),
                    r => (if indent.is_empty() { "•" } else { "◦" }, r),
                };
                return Some(Part::Row(format!("{indent}{mark} {}", inline(rest))));
            }
        }
        let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && trimmed[digits..].starts_with(". ") {
            return Some(Part::Row(format!("{indent}{DIM}{}.{RESET} {}", &trimmed[..digits], inline(&trimmed[digits + 2..]))));
        }
        if matches!(trimmed, "---" | "***" | "___") {
            return Some(Part::Row(format!("{RULE}{}{RESET}", "─".repeat(self.width.clamp(10, 60)))));
        }
        Some(Part::Row(inline(line)))
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
            out.push_str(&code(&after[..end]));
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

/// Inline code: text on a chip where 24-bit color can draw one softly, else in lagoon.
fn code(span: &str) -> String {
    if truecolor() { format!("{CHIP}{span}{RESET}") } else { format!("{LAGOON}{span}{RESET}") }
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

/// A table as aligned rows: a bold header over a rule, columns shrunk to fit `max_width`.
pub fn render_table(rows: &[String], max_width: usize) -> Vec<String> {
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
                let cell = ellipsize(cell, widths[i]);
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
    fn sanitize_takes_out_escape_sequences_and_controls() {
        assert_eq!(&*sanitize("plain, 中文 and ✓"), "plain, 中文 and ✓");
        assert_eq!(&*sanitize("a\x1b[31mred\x1b[0mb"), "aredb");
        assert_eq!(&*sanitize("a\x1b[2Jbc"), "abc");
        assert_eq!(&*sanitize("\x1b[3;7Hx"), "x");
        assert_eq!(&*sanitize("\x1b]0;title\x07rest"), "rest");
        assert_eq!(&*sanitize("\x1b]8;;http://a\x1b\\link\x1b]8;;\x1b\\!"), "link!");
        assert_eq!(&*sanitize("a\x1bPq;1;2\x1b\\b"), "ab");
        assert_eq!(&*sanitize("a\x1bMb"), "ab");
        assert_eq!(&*sanitize("a\x1b"), "a", "a lone escape goes");
        assert_eq!(&*sanitize("a\x1b[31"), "a", "so does one the text ends in the middle of");
        assert_eq!(&*sanitize("x\u{9b}31my"), "x31my", "a C1 control is no safer");
        assert_eq!(&*sanitize("a\x07b\rc\x7fd"), "abcd", "other C0 controls go, line breaks stay");
        assert_eq!(&*sanitize("a\nb"), "a\nb");
        // A tab that would eat the line break neither eats it nor the text after it.
        assert_eq!(&*sanitize("a\x1b[\nb"), "a\nb");
        assert_eq!(&*sanitize("\x1b]0;still open\nb"), "\nb");
    }

    #[test]
    fn sanitize_expands_tabs_to_the_next_four_column_stop() {
        assert_eq!(&*sanitize("a\tb"), "a   b");
        assert_eq!(&*sanitize("ab\tc"), "ab  c");
        assert_eq!(&*sanitize("abc\td"), "abc d");
        assert_eq!(&*sanitize("\tx"), "    x");
        assert_eq!(&*sanitize("a\n\tb"), "a\n    b", "the column starts over on each line");
        assert_eq!(&*sanitize("中文\t."), "中文    .");
        assert_eq!(&*sanitize_row("one\ntwo"), "one ⏎ two");
        assert_eq!(&*sanitize_row("a\tb"), "a   b");
        assert_eq!(&*sanitize_row("plain"), "plain");
    }

    #[test]
    fn a_sequence_split_across_deltas_is_still_taken_out() {
        let mut stream = Sanitizer::default();
        assert_eq!(stream.push("ok \x1b["), "ok ");
        assert_eq!(stream.push("31;41mred"), "red");
        assert_eq!(stream.push("\x1b]0;ti"), "");
        assert_eq!(stream.push("tle\x07!"), "!");
        assert_eq!(stream.push("ab"), "ab");
        assert_eq!(stream.push("\tc"), "   c", "a tab counts every column before it, delta or not");
        assert_eq!(stream.push("d\x1b"), "d");
        assert_eq!(stream.push("[2K"), "");
        assert_eq!(stream.finish(), "");
        let mut ending = Sanitizer::default();
        assert_eq!(ending.push("end\x1b[12"), "end");
        assert_eq!(ending.finish(), "", "a sequence still waiting at the end goes");
    }

    #[test]
    fn wrapped_code_rows_keep_the_gutter() {
        let rows = wrap_code(&format!("  {FAINT}▏{RESET} {}", "let x = 1;".repeat(20)), 30);
        assert!(rows.len() > 1);
        assert!(rows.iter().all(|row| width(row) <= 30), "{rows:?}");
        assert!(rows.iter().all(|row| row.contains('▏')), "{rows:?}");
    }

    #[test]
    fn code_marks_a_part_that_wraps_with_its_gutter() {
        let mut md = Markdown::new(30);
        md.line("```rust");
        let parts = md.parts(&"let x = 1;".repeat(20));
        assert!(matches!(parts.as_slice(), [Part::Code(_)]));
        let rows = md.rows(parts);
        assert!(rows.len() > 1 && rows.iter().all(|row| width(row) <= 30 && row.contains('▏')), "{rows:?}");
    }

    #[test]
    fn the_256_color_light_shades_read_at_four_point_five_to_one() {
        for sgr in [LAGOON, WARN] {
            let index: u8 = sgr.light["\x1b[38;5;".len()..sgr.light.len() - 1].parse().unwrap();
            let ratio = contrast_256(index, (245, 247, 244));
            assert!(ratio >= 4.5, "{index}: {ratio}");
        }
    }

    /// The RGB of a 256-color palette index.
    fn palette_256(index: u8) -> (u8, u8, u8) {
        match index {
            16..=231 => {
                let n = index - 16;
                let level = |k: u8| [0, 95, 135, 175, 215, 255][usize::from(k)];
                (level(n / 36), level((n % 36) / 6), level(n % 6))
            }
            232..=255 => {
                let v = 8 + 10 * (index - 232);
                (v, v, v)
            }
            _ => (0, 0, 0),
        }
    }

    /// The WCAG contrast ratio of a palette index on a background color.
    fn contrast_256(index: u8, background: (u8, u8, u8)) -> f64 {
        let luminance = |(r, g, b): (u8, u8, u8)| {
            let linear = |c: u8| {
                let c = f64::from(c) / 255.0;
                if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
        };
        let (a, b) = (luminance(palette_256(index)), luminance(background));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
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
        assert_eq!(md.line("- use `cargo` **now**"), [format!("• use {} {BOLD}now{RESET}", code("cargo"))]);
        assert_eq!(md.line("  - nested"), ["  ◦ nested"]);
        assert_eq!(md.line("- [x] done"), ["☑ done"]);
        assert_eq!(md.line("2. second"), [format!("{DIM}2.{RESET} second")]);
        assert!(md.line("```").is_empty(), "a fence with no language has nothing to say");
        assert_eq!(md.line("let x = 1;"), [format!("  {FAINT}▏{RESET} let x = 1;")]);
        assert!(md.line("```").is_empty(), "nor does the one that closes the block");
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
    fn an_unclosed_fence_does_not_leak_into_the_next_message() {
        let mut md = Markdown::default();
        assert!(md.line("```rust").iter().any(|row| row.contains("rust")));
        assert!(md.line("let x = 1;").iter().all(|row| row.contains('▏')));
        let held = md.end_message();
        assert!(held.is_empty(), "no table was held");
        assert_eq!(md.line("plain"), ["plain"]);
        let reopened = md.line("```rust");
        assert!(reopened.iter().any(|row| row.contains("rust")), "a fence still opens after the reset: {reopened:?}");
        assert!(md.line("let y = 2;").iter().all(|row| row.contains('▏')));
    }

    #[test]
    fn a_background_picks_the_theme_by_its_brightness() {
        assert!(matches!(theme_for((11, 15, 20)), Theme::Dark));
        assert!(matches!(theme_for((245, 247, 244)), Theme::Light));
        assert!(matches!(theme_for((0, 43, 54)), Theme::Dark));
        assert!(matches!(theme_for((253, 246, 227)), Theme::Light));
    }

    #[test]
    fn roles_print_their_tokens_exactly_on_truecolor_terminals() {
        assert_eq!(LAGOON.shade(Theme::Dark, true), "\x1b[38;2;79;209;197m");
        assert_eq!(LAGOON.shade(Theme::Dark, false), "\x1b[38;5;80m");
        assert_eq!(TINT.shade(Theme::Light, true), "\x1b[48;2;223;229;226m");
        assert_eq!(TINT.shade(Theme::Light, false), "\x1b[48;5;254m");
        assert_eq!(EFFORT.shade(Theme::Dark, true), "\x1b[38;2;170;169;235m");
        assert_eq!(TOKENS.shade(Theme::Dark, true), "\x1b[38;2;129;188;229m");
        assert_eq!(BRANCH.shade(Theme::Dark, true), "\x1b[38;2;215;160;202m");
        for role in [LAGOON, FIN, WARN, DANGER, FAINT, RULE, EFFORT, TOKENS, BRANCH] {
            assert!(role.dark_true.starts_with("\x1b[38;2;") && role.light_true.starts_with("\x1b[38;2;"));
            assert!(role.dark.starts_with("\x1b[38;5;") && role.light.starts_with("\x1b[38;5;"));
        }
        for ground in [TINT, SELECT, ADD, DEL, CHIP] {
            assert!(ground.dark_true.starts_with("\x1b[48;2;") && ground.light_true.starts_with("\x1b[48;2;"));
        }
        for ground in [ADD, DEL, CHIP] {
            assert_eq!((ground.dark, ground.light), ("\x1b[49m", "\x1b[49m"), "no stripe or chip without 24-bit color");
        }
        assert_eq!((DIM.dark, DIM.light_true), ("\x1b[2m", "\x1b[2m"), "without a ground, dim is SGR dim");
    }

    #[test]
    fn the_fixed_grounds_are_what_the_palette_derives_for_the_design_systems_own_ground() {
        let digits = |shade: &str| -> Vec<u8> { shade.trim_start_matches("\x1b[48;2;").trim_end_matches('m').split(';').map(|n| n.parse().unwrap()).collect() };
        let dark = palette::derive(Ground { background: (11, 15, 20), foreground: Some((232, 238, 242)) });
        let light = palette::derive(Ground { background: (245, 247, 244), foreground: Some((19, 26, 32)) });
        for (role, sgr) in [(Role::Tint, TINT), (Role::Select, SELECT), (Role::Add, ADD), (Role::Del, DEL), (Role::Chip, CHIP)] {
            for (derived, shade) in [(dark.rgb(role), sgr.dark_true), (light.rgb(role), sgr.light_true)] {
                let fixed = digits(shade);
                assert!([derived.0, derived.1, derived.2].iter().zip(&fixed).all(|(a, b)| a.abs_diff(*b) <= 1), "{role:?}: {derived:?} against {fixed:?}");
            }
        }
    }

    #[test]
    fn an_ellipsis_ends_a_row_that_runs_out_in_the_style_it_was_in() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("exactly ten", 11), "exactly ten");
        assert_eq!(ellipsize("one two three", 8), "one two…");
        assert_eq!(width(&ellipsize("one two three", 8)), 8);
        assert_eq!(ellipsize("anything", 0), "");
        assert_eq!(ellipsize("中文中文", 5), "中文…", "a wide character is never split");
        let tinted = ellipsize(&format!("{TINT}{BOLD}a long selected row{RESET}"), 10);
        assert_eq!(tinted, format!("{TINT}{BOLD}a long se…{RESET}"), "the ellipsis keeps the band");
        assert_eq!(width(&tinted), 10);
        let closed = ellipsize(&format!("{BOLD}bold{RESET} and plain text"), 12);
        assert_eq!(closed, format!("{BOLD}bold{RESET} and pl…"), "after a reset the ellipsis is plain");
    }

    #[test]
    fn inline_code_is_a_chip_in_24_bit_color_and_lagoon_elsewhere() {
        assert!(inline("a `b` c").contains(&code("b")));
        let expected = if truecolor() { format!("{CHIP}b{RESET}") } else { format!("{LAGOON}b{RESET}") };
        assert_eq!(code("b"), expected);
    }

    #[test]
    fn a_code_block_starts_and_ends_on_its_code() {
        let mut md = Markdown::default();
        assert_eq!(lines(&mut md, "```\nx\n```").len(), 1);
        assert_eq!(lines(&mut md, "```rust\nlet a = 1;\nlet b = 2;\n```").len(), 3, "the label and two lines");
        assert_eq!(lines(&mut md, "after").len(), 1);
    }

    #[test]
    fn styling_steps_bump_the_generation() {
        let mut md = Markdown::new(80);
        assert_eq!(md.generation(), 0);
        md.line("```rust");
        let styled = md.generation();
        assert!(styled > 0);
        md.line("let x = 1;");
        assert!(md.generation() > styled);
        md.finish();
        assert!(md.generation() > styled);
    }

    #[test]
    fn no_color_drops_colors_and_keeps_styles() {
        let line = format!("{BOLD}{LAGOON}x{RESET}\x1b[38;2;1;2;3m\x1b[48;5;9my\x1b[1;32mz{DIM}");
        assert_eq!(strip_colors(&line), format!("{BOLD}x{RESET}y\x1b[1mz{DIM}"));
        assert_eq!(strip_colors("plain"), "plain");
    }
}
