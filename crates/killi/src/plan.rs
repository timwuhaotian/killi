//! Plan mode: the tool list stays the same, and calls are checked when they run. Writes are
//! refused, and bash runs only commands known to be read-only.

mod braces;
mod sed;
pub(crate) mod words;
mod writes;

/// Commands whose every form only reads. Matched as the start of each pipeline segment.
/// `cd` is here because a segment only moves the shell the other segments run in; each of
/// them is still checked on its own.
const READ_ONLY: &[&str] = &[
    "cd",
    "ls",
    "cat",
    "head",
    "tail",
    "wc",
    "rg",
    "grep",
    "egrep",
    "fgrep",
    "find",
    "fd",
    "tree",
    "file",
    "stat",
    "du",
    "df",
    "pwd",
    "which",
    "type",
    "whoami",
    "uname",
    "date",
    "echo",
    "printf",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "sort",
    "uniq",
    "cut",
    "tr",
    "diff",
    "cmp",
    "jq",
    "nl",
    "column",
    "less",
    "more",
    "sed -n",
    "git log",
    "git diff",
    "git status",
    "git show",
    "git branch",
    "git blame",
    "git rev-parse",
    "git ls-files",
    "git grep",
    "git remote",
    "git tag",
    "git describe",
    "git shortlog",
    "git config --get",
    "cargo check",
    "cargo clippy",
    "cargo tree",
    "cargo metadata",
    "cargo --version",
    "rustc --version",
    "npm ls",
    "npm view",
    "npm --version",
    "node --version",
    "pnpm ls",
    "tsc --noEmit",
    "go vet",
    "go list",
    "python3 --version",
    "swift --version",
    "xcodebuild -list",
    "killi sessions",
    "killi ask",
];

/// The parts of `command` between its control operators (`|`, `;`, `&`, a newline) that bash
/// would act on: one outside quotes and past a backslash is text to the command instead, as
/// in `grep -E 'a|b'`. `$'…'` is like `'…'` with backslash escapes; `'…'` has none. An
/// unquoted `#` that starts a word comments out the rest of its line, and a heredoc's body
/// (the lines after a `<<WORD` up to the line `WORD`) is input, so neither is in a part.
pub(crate) fn segments(command: &str) -> Vec<&str> {
    #[derive(PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
        Ansi,
    }
    let bytes = command.as_bytes();
    let mut parts = Vec::new();
    let mut heredocs = Vec::new();
    let (mut start, mut at, mut quote) = (0, 0, Quote::None);
    let mut word_start = true;
    while at < bytes.len() {
        let byte = bytes[at];
        match quote {
            Quote::None => {
                let starts_word = std::mem::replace(
                    &mut word_start,
                    matches!(
                        byte,
                        b' ' | b'\t' | b'\n' | b';' | b'|' | b'&' | b'(' | b')' | b'<' | b'>'
                    ),
                );
                match byte {
                    b'#' if starts_word => {
                        parts.push(&command[start..at]);
                        at = command[at..].find('\n').map_or(bytes.len(), |n| at + n);
                        start = at;
                        word_start = true;
                        continue;
                    }
                    b'\\' => at += 1,
                    b'\'' => quote = Quote::Single,
                    b'"' => quote = Quote::Double,
                    b'$' if matches!(bytes.get(at + 1), Some(b'\'' | b'"')) => {
                        quote = if bytes[at + 1] == b'\'' {
                            Quote::Ansi
                        } else {
                            Quote::Double
                        };
                        at += 1;
                    }
                    b'<' if bytes.get(at + 1) == Some(&b'<') => {
                        if bytes.get(at + 2) == Some(&b'<') {
                            at += 2;
                        } else {
                            let (delimiter, end) = heredoc_delimiter(command, at + 2);
                            heredocs.extend(delimiter);
                            at = end;
                            continue;
                        }
                    }
                    b'\n' => {
                        parts.push(&command[start..at]);
                        start = skip_heredocs(command, at + 1, &mut heredocs);
                        at = start;
                        word_start = true;
                        continue;
                    }
                    b'|' | b';' | b'&' => {
                        parts.push(&command[start..at]);
                        start = at + 1;
                    }
                    _ => {}
                }
            }
            Quote::Single if byte == b'\'' => quote = Quote::None,
            Quote::Double => match byte {
                b'\\' => at += 1,
                b'"' => quote = Quote::None,
                _ => {}
            },
            Quote::Ansi => match byte {
                b'\\' => at += 1,
                b'\'' => quote = Quote::None,
                _ => {}
            },
            _ => {}
        }
        at += 1;
    }
    parts.push(&command[start.min(bytes.len())..]);
    parts
}

/// The heredoc word that starts at `at`, just past `<<`, with its quotes removed, and the
/// index past it. The flag says `<<-`, whose body lines may start with tabs.
fn heredoc_delimiter(command: &str, mut at: usize) -> (Option<(String, bool)>, usize) {
    let bytes = command.as_bytes();
    let strip_tabs = bytes.get(at) == Some(&b'-');
    if strip_tabs {
        at += 1;
    }
    while matches!(bytes.get(at), Some(b' ' | b'\t')) {
        at += 1;
    }
    let mut word = Vec::new();
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b' ' | b'\t' | b'\n' | b';' | b'|' | b'&' | b'<' | b'>' | b'(' | b')' => break,
            b'\'' | b'"' => {
                let close = bytes[at + 1..]
                    .iter()
                    .position(|&b| b == byte)
                    .map_or(bytes.len(), |n| at + 1 + n);
                word.extend_from_slice(&bytes[at + 1..close]);
                at = close + 1;
            }
            b'\\' => {
                word.extend(bytes.get(at + 1));
                at += 2;
            }
            _ => {
                word.push(byte);
                at += 1;
            }
        }
    }
    let at = at.min(bytes.len());
    if word.is_empty() {
        return (None, at);
    }
    (
        Some((String::from_utf8_lossy(&word).into_owned(), strip_tabs)),
        at,
    )
}

/// Where `command` goes on past the bodies of `heredocs`, which start at `at`: each ends at
/// its word alone on a line, or at the end of the command.
fn skip_heredocs(command: &str, mut at: usize, heredocs: &mut Vec<(String, bool)>) -> usize {
    for (delimiter, strip_tabs) in heredocs.drain(..) {
        while at < command.len() {
            let end = command[at..].find('\n').map_or(command.len(), |n| at + n);
            let line = &command[at..end];
            let line = if strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line
            };
            at = end + 1;
            if line == delimiter {
                break;
            }
        }
    }
    at.min(command.len())
}

/// Commands no argument can make write or run anything, so they may take words plan mode
/// can't see: arguments `xargs` reads from its input, a glob that could name a file
/// called `-o` or `--pre=sh`, or a variable such as `$PATH`.
const ANY_ARGUMENTS: &[&str] = &[
    "cat", "head", "tail", "wc", "ls", "grep", "egrep", "fgrep", "stat", "du", "basename",
    "dirname", "realpath", "readlink", "echo", "nl", "cut", "tr", "cmp", "diff", "column", "jq",
];

/// The first segment of `command` plan mode refuses, if any, so the refusal can name it.
/// `extra` holds additional allowed prefixes from config.
/// The segment comes from the cleaned command, so a let-through redirect (`2>&1`,
/// `>/dev/null`) is missing from its text.
pub fn bash_refused(command: &str, extra: &[String]) -> Option<String> {
    let cleaned = without_harmless_redirects(command);
    segments(&cleaned)
        .into_iter()
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .find(|segment| {
            segment.contains("$(")
                || segment.contains('`')
                || segment.contains("<(")
                || !words::words(segment).is_some_and(|words| segment_allowed(&words, extra))
        })
        .map(str::to_string)
}

/// `command` without the redirects plan mode lets through: `2>&1`, `2>/dev/null` and
/// `>/dev/null`, each only where bash reads it as one, so the `>2` in `>2>&1/dev/null`,
/// which writes a file named `2`, stays and is refused.
fn without_harmless_redirects(command: &str) -> String {
    const LET_THROUGH: &[&str] = &[
        "2>&1",
        "2>/dev/null",
        "2> /dev/null",
        ">/dev/null",
        "> /dev/null",
    ];
    let bytes = command.as_bytes();
    let boundary =
        |byte: u8| byte.is_ascii_whitespace() || matches!(byte, b';' | b'|' | b'&' | b'(' | b')');
    let mut cleaned = String::with_capacity(command.len());
    let (mut kept, mut at) = (0, 0);
    while at < bytes.len() {
        let found = LET_THROUGH.iter().find(|redirect| {
            bytes[at..].starts_with(redirect.as_bytes())
                && (redirect.starts_with('>') || at == 0 || boundary(bytes[at - 1]))
                && bytes.get(at + redirect.len()).is_none_or(|&b| boundary(b))
        });
        match found {
            Some(redirect) => {
                cleaned.push_str(&command[kept..at]);
                at += redirect.len();
                kept = at;
            }
            None => at += 1,
        }
    }
    cleaned.push_str(&command[kept..]);
    cleaned
}

/// The command a segment runs, for the checks: `git -C <dir> log` is `git log`, and `xargs
/// wc -l` runs `wc` on what it reads (`xargs` alone runs `echo`). `git -c` stays in view,
/// since a config value can name a command to run. The flag says the command came after
/// `xargs`, so it gets arguments the checks never see. `None` when `xargs` would put what it
/// reads into the command's name (`-I`, `-J`, `-i`, `--replace`).
fn command_words(words: &[String]) -> Option<(Vec<&str>, bool)> {
    let mut words = words.iter().map(String::as_str).peekable();
    if words.peek() == Some(&"git") {
        words.next();
        while let Some(word) = words.peek().copied() {
            match word {
                "-C" | "--git-dir" | "--work-tree" => {
                    words.next();
                    words.next();
                }
                "--no-pager" | "--no-optional-locks" => {
                    words.next();
                }
                _ if word.starts_with("-C")
                    || word.starts_with("--git-dir=")
                    || word.starts_with("--work-tree=") =>
                {
                    words.next();
                }
                _ => break,
            }
        }
        return Some((std::iter::once("git").chain(words).collect(), false));
    }
    if words.peek() == Some(&"xargs") {
        words.next();
        let mut replace = None;
        while let Some(word) = words.peek().copied() {
            if word == "--" || !word.starts_with('-') {
                break;
            }
            words.next();
            if word.starts_with("--") {
                if writes::long_flag(word, "--replace") {
                    replace = Some(word.split_once('=').map_or("{}", |(_, value)| value));
                }
                // GNU's long options that need a value take the next word without `=`.
                let needs_value = [
                    "--arg-file",
                    "--delimiter",
                    "--max-args",
                    "--max-chars",
                    "--max-procs",
                    "--process-slot-var",
                ];
                if !word.contains('=') && needs_value.iter().any(|f| writes::long_flag(word, f)) {
                    words.next();
                }
                continue;
            }
            // Letters that take a value take the rest of the word or, when it is empty, the
            // next word; `-e`, `-i` and `-l` take only the rest.
            for (i, letter) in word.char_indices().skip(1) {
                let rest = &word[i + letter.len_utf8()..];
                match letter {
                    'I' | 'J' | 'a' | 'd' | 'E' | 'L' | 'n' | 'P' | 's' | 'S' | 'R' => {
                        let value = if rest.is_empty() {
                            words.next()
                        } else {
                            Some(rest)
                        };
                        if matches!(letter, 'I' | 'J') {
                            replace = Some(value.unwrap_or_default());
                        }
                        break;
                    }
                    'i' => {
                        replace = Some(if rest.is_empty() { "{}" } else { rest });
                        break;
                    }
                    'e' | 'l' => break,
                    _ => {}
                }
            }
        }
        let command = words.peek().copied().unwrap_or("echo");
        if replace.is_some_and(|r| r.is_empty() || command.contains(r)) {
            return None;
        }
        if words.peek().is_none() {
            return Some((vec!["echo"], true));
        }
        return Some((words.collect(), true));
    }
    Some((words.collect(), false))
}

fn segment_allowed(words: &[words::Word], extra: &[String]) -> bool {
    let texts: Vec<String> = words.iter().map(|w| w.text.clone()).collect();
    if writes::foreign_git_dir(&texts) {
        return false;
    }
    let Some((words_run, via_xargs)) = command_words(&texts) else {
        return false;
    };
    if words_run
        .first()
        .is_some_and(|w| w.contains(char::is_whitespace))
    {
        return false;
    }
    let starts = |prefix: &str| {
        let mut wanted = prefix.split_whitespace().peekable();
        wanted.peek().is_some()
            && wanted
                .enumerate()
                .all(|(i, w)| words_run.get(i) == Some(&w))
    };
    if writes::writes_behind_a_read_only_name(&words_run) {
        return false;
    }
    let configured = extra.iter().any(|prefix| starts(prefix));
    // A word past `--` is an operand, not a flag, except to `find`, whose expression comes
    // after its paths, and `cargo`, which hands what follows to another program.
    let operands_after_dashes = !matches!(words_run.first(), Some(&("find" | "cargo")));
    let unseen = words
        .iter()
        .take_while(|w| !(operands_after_dashes && w.text == "--"))
        .any(|w| w.unseen);
    if (via_xargs || unseen)
        && !configured
        && !words_run.first().is_some_and(|c| ANY_ARGUMENTS.contains(c))
    {
        return false;
    }
    READ_ONLY.iter().any(|prefix| starts(prefix)) || configured
}

pub const ON: &str = "<system-reminder>Plan mode is on. Research and plan only: write and edit are blocked, and bash runs only read-only commands. When your plan is ready, call exit_plan_mode with it and wait for the user's approval.</system-reminder>";

pub const OFF: &str =
    "<system-reminder>Plan mode is off. You may change files again.</system-reminder>";

#[cfg(test)]
mod tests;
