use super::*;

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
        Self {
            width,
            ..Self::default()
        }
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

    pub(super) fn rows(&self, parts: Vec<Part>) -> Vec<String> {
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
            let lang = info
                .trim()
                .split(|c: char| c.is_whitespace() || c == ',')
                .next()
                .unwrap_or("");
            self.in_code = !self.in_code;
            self.lang = if self.in_code {
                Lang::of(lang)
            } else {
                Lang::Plain
            };
            let label = if self.in_code { lang } else { "" };
            return (!label.is_empty())
                .then(|| Part::Row(format!("    {DIM}{label}{RESET}")));
        }
        if self.in_code {
            let code = match self.lang {
                Lang::Plain => line.to_string(),
                lang => highlight(lang, line),
            };
            return Some(Part::Code(format!("    {code}")));
        }
        if let Some((level, rest)) = heading(trimmed) {
            let color = if level <= 2 {
                LAGOON.to_string()
            } else {
                String::new()
            };
            return Some(Part::Row(format!("{BOLD}{color}{}{RESET}", inline(rest))));
        }
        if let Some(rest) = trimmed.strip_prefix("> ") {
            return Some(Part::Row(format!(
                "{FAINT}│{RESET} {DIM}{}{RESET}",
                inline(rest)
            )));
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
            return Some(Part::Row(format!(
                "{indent}{DIM}{}.{RESET} {}",
                &trimmed[..digits],
                inline(&trimmed[digits + 2..])
            )));
        }
        if matches!(trimmed, "---" | "***" | "___") {
            return Some(Part::Row(format!(
                "{RULE}{}{RESET}",
                "─".repeat(self.width.clamp(10, 60))
            )));
        }
        Some(Part::Row(inline(line)))
    }
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    (1..=6)
        .contains(&hashes)
        .then(|| line[hashes..].strip_prefix(' '))
        .flatten()
        .map(|rest| (hashes, rest))
}

/// `code`, **bold**, *italic*, ~~struck~~ and [links](url) inside a line.
pub(super) fn inline(text: &str) -> String {
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
                let shown = if url.is_empty() { label } else { url };
                out.push_str(&hyperlink(
                    url,
                    &format!("{LAGOON}{UNDERLINE}{shown}{RESET}"),
                ));
            } else {
                out.push_str(&hyperlink(
                    url,
                    &format!("{LAGOON}{UNDERLINE}{label}{RESET}"),
                ));
                out.push_str(&format!(" {DIM}({}){RESET}", hyperlink(url, url)));
            }
            rest = &rest[used..];
            continue;
        }
        let next = rest
            .char_indices()
            .skip(1)
            .find(|(_, c)| matches!(c, '`' | '*' | '~' | '['))
            .map_or(rest.len(), |(i, _)| i);
        out.push_str(&rest[..next]);
        rest = &rest[next..];
    }
    out
}

/// Inline code: text on a chip where 24-bit color can draw one softly, else in lagoon.
pub(super) fn code(span: &str) -> String {
    if truecolor() {
        format!("{CHIP}{span}{RESET}")
    } else {
        format!("{LAGOON}{span}{RESET}")
    }
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
        .map(|row| {
            (0..count)
                .map(|i| row.get(i).map_or(String::new(), |c| inline(c)))
                .collect()
        })
        .collect();
    let mut widths: Vec<usize> = (0..count)
        .map(|i| styled.iter().map(|row| width(&row[i])).max().unwrap_or(0))
        .collect();
    let overhead = 2 + 3 * count.saturating_sub(1);
    if max_width > 0 {
        while widths.iter().sum::<usize>() + overhead > max_width {
            let Some((widest, w)) = widths.iter().copied().enumerate().max_by_key(|&(_, w)| w)
            else {
                break;
            };
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
                let cell = if bold {
                    format!("{BOLD}{cell}{RESET}")
                } else {
                    cell
                };
                let pad = widths[i].saturating_sub(width(&cell));
                match aligns.get(i) {
                    Some('r') => format!("{}{cell}", " ".repeat(pad)),
                    Some('c') => {
                        format!("{}{cell}{}", " ".repeat(pad / 2), " ".repeat(pad - pad / 2))
                    }
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

/// A language whose code blocks get colors. Rust and the JavaScript family (TypeScript
/// included) get the full treatment — types and calls colored too — in code blocks and
/// in an edit's diff; the rest keep the keyword, string and number colors in blocks.
#[derive(Default, Clone, Copy, PartialEq)]
pub enum Lang {
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
    /// The language of the file at `path`, by its extension.
    pub fn of_path(path: &str) -> Lang {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        match name.rsplit_once('.') {
            Some((_, extension)) => Lang::of(extension),
            None => Lang::Plain,
        }
    }

    /// Whether the language gets types and calls colored, and its diff lines highlighted.
    pub fn rich(self) -> bool {
        matches!(self, Lang::Rust | Lang::Js)
    }

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
                "fn", "let", "mut", "pub", "struct", "enum", "impl", "trait", "use", "mod",
                "match", "if", "else", "for", "while", "loop", "return", "self", "Self", "async",
                "await", "move", "const", "static", "type", "where", "crate", "super", "as", "in",
                "ref", "dyn", "unsafe", "break", "continue", "true", "false", "extern", "union",
                "macro_rules", "yield", "gen", "try",
            ],
            Lang::Js => &[
                "function",
                "const",
                "let",
                "var",
                "return",
                "if",
                "else",
                "for",
                "while",
                "class",
                "new",
                "import",
                "export",
                "from",
                "default",
                "async",
                "await",
                "try",
                "catch",
                "throw",
                "switch",
                "case",
                "break",
                "continue",
                "typeof",
                "interface",
                "type",
                "extends",
                "true",
                "false",
                "null",
                "undefined",
                "this",
                "of",
                "in",
                "enum",
                "namespace",
                "declare",
                "readonly",
                "abstract",
                "implements",
                "keyof",
                "infer",
                "satisfies",
                "is",
                "as",
                "private",
                "public",
                "protected",
                "static",
                "delete",
                "instanceof",
                "yield",
                "finally",
                "do",
            ],
            Lang::Py => &[
                "def", "class", "return", "if", "elif", "else", "for", "while", "import", "from",
                "as", "with", "try", "except", "finally", "raise", "lambda", "pass", "break",
                "continue", "yield", "async", "await", "in", "not", "and", "or", "is", "None",
                "True", "False", "self",
            ],
            Lang::Sh => &[
                "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac",
                "function", "in", "echo", "cd", "export", "return", "local",
            ],
            Lang::Go => &[
                "func",
                "var",
                "const",
                "type",
                "struct",
                "interface",
                "package",
                "import",
                "return",
                "if",
                "else",
                "for",
                "range",
                "switch",
                "case",
                "default",
                "go",
                "defer",
                "chan",
                "map",
                "select",
                "nil",
                "true",
                "false",
                "break",
                "continue",
            ],
            Lang::Json | Lang::Config => &["true", "false", "null"],
            Lang::Plain | Lang::Diff => &[],
        }
    }

    /// The built-in type names that don't start with a capital.
    fn types(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &[
                "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8",
                "i16", "i32", "i64", "i128", "isize", "f32", "f64",
            ],
            Lang::Js => &[
                "string", "number", "boolean", "bigint", "symbol", "any", "unknown", "never",
                "void", "object",
            ],
            _ => &[],
        }
    }
}

/// One line of code with comments, strings, numbers and keywords colored; a rich language
/// (see `Lang::rich`) also gets its types, calls, lifetimes and attributes colored. State
/// never crosses a line, so a block comment or string spanning lines colors each on its own.
pub fn highlight(lang: Lang, line: &str) -> String {
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
    let types = lang.types();
    let rich = lang.rich();
    let chars: Vec<char> = line.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let at = |i: usize| chars.get(i).copied();
    let word_end = |from: usize| {
        chars[from..]
            .iter()
            .position(|&ch| !is_word(ch))
            .map_or(chars.len(), |p| from + p)
    };
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(marker) = comment
            && chars[i..].iter().take(marker.len()).collect::<String>() == marker
        {
            out.push_str(&format!(
                "{DIM}{}{RESET}",
                chars[i..].iter().collect::<String>()
            ));
            break;
        }
        if rich && c == '/' && at(i + 1) == Some('*') {
            let mut end = i + 2;
            while end + 1 < chars.len() && !(chars[end] == '*' && chars[end + 1] == '/') {
                end += 1;
            }
            let end = (end + 2).min(chars.len());
            out.push_str(&format!(
                "{DIM}{}{RESET}",
                chars[i..end].iter().collect::<String>()
            ));
            i = end;
            continue;
        }
        if lang == Lang::Rust
            && c == '#'
            && (at(i + 1) == Some('[') || (at(i + 1) == Some('!') && at(i + 2) == Some('[')))
        {
            let mut depth = 0usize;
            let mut end = i;
            while end < chars.len() {
                match chars[end] {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            end += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                end += 1;
            }
            out.push_str(&format!(
                "{KEYWORD}{}{RESET}",
                chars[i..end].iter().collect::<String>()
            ));
            i = end;
            continue;
        }
        if lang == Lang::Js && c == '@' && at(i + 1).is_some_and(is_word) {
            let end = word_end(i + 1);
            out.push_str(&format!(
                "{KEYWORD}{}{RESET}",
                chars[i..end].iter().collect::<String>()
            ));
            i = end;
            continue;
        }
        if lang == Lang::Rust && c == '\'' {
            let end = word_end(i + 1);
            if at(i + 1) == Some('\\') || (end == i + 2 && at(end) == Some('\'')) {
                let mut close = i + 1;
                while close < chars.len() && chars[close] != '\'' {
                    close += if chars[close] == '\\' { 2 } else { 1 };
                }
                let close = (close + 1).min(chars.len());
                out.push_str(&format!(
                    "{STRING}{}{RESET}",
                    chars[i..close].iter().collect::<String>()
                ));
                i = close;
            } else if end > i + 1 {
                out.push_str(&format!(
                    "{TYPE}{}{RESET}",
                    chars[i..end].iter().collect::<String>()
                ));
                i = end;
            } else {
                out.push(c);
                i += 1;
            }
            continue;
        }
        if quotes.contains(&c) {
            let mut end = i + 1;
            while end < chars.len() && chars[end] != c {
                end += if chars[end] == '\\' { 2 } else { 1 };
            }
            let end = (end + 1).min(chars.len());
            out.push_str(&format!(
                "{STRING}{}{RESET}",
                chars[i..end].iter().collect::<String>()
            ));
            i = end;
            continue;
        }
        if c.is_ascii_digit() && (i == 0 || !is_word(chars[i - 1])) {
            let end = chars[i..]
                .iter()
                .position(|&ch| !(ch.is_alphanumeric() || ch == '.' || ch == '_'))
                .map_or(chars.len(), |p| i + p);
            out.push_str(&format!(
                "{NUMBER}{}{RESET}",
                chars[i..end].iter().collect::<String>()
            ));
            i = end;
            continue;
        }
        if is_word(c) {
            let end = word_end(i);
            let word: String = chars[i..end].iter().collect();
            if lang == Lang::Rust && matches!(word.as_str(), "r" | "br") {
                let mut open = end;
                while at(open) == Some('#') {
                    open += 1;
                }
                if at(open) == Some('"') {
                    let hashes = open - end;
                    let mut close = open + 1;
                    let raw_end = loop {
                        match chars[close..].iter().position(|&ch| ch == '"') {
                            Some(found) => {
                                close += found;
                                let after = chars[close + 1..]
                                    .iter()
                                    .take(hashes)
                                    .take_while(|&&ch| ch == '#')
                                    .count();
                                if after == hashes {
                                    break close + 1 + hashes;
                                }
                                close += 1;
                            }
                            None => break chars.len(),
                        }
                    };
                    out.push_str(&format!(
                        "{STRING}{}{RESET}",
                        chars[i..raw_end].iter().collect::<String>()
                    ));
                    i = raw_end;
                    continue;
                }
            }
            let called = rich
                && (at(end) == Some('(')
                    || lang == Lang::Rust
                        && at(end) == Some('!')
                        && matches!(at(end + 1), Some('(' | '[' | '{')));
            if words.contains(&word.as_str()) {
                out.push_str(&format!("{KEYWORD}{word}{RESET}"));
            } else if rich
                && (types.contains(&word.as_str())
                    || word.chars().next().is_some_and(char::is_uppercase))
            {
                out.push_str(&format!("{TYPE}{word}{RESET}"));
            } else if called {
                out.push_str(&format!("{CALL}{word}{RESET}"));
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
