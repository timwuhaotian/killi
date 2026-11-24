//! The words bash hands a command: quotes removed, escapes resolved, `$HOME` filled in and
//! braces expanded, so plan mode's checks see `'-delete'`, `-de""lete` and `-{delete,x}` as the
//! `-delete` that `find` receives.

use super::braces;

/// Variables bash takes from the environment unchanged, so this process's value is the
/// shell's. Others may be bash's own (`$BASH_COMMAND` is the command's text) or set by the
/// command. `$PWD` follows `cd`, so it is read only inside double quotes, where a directory
/// name with a space in it can't split.
const ENV_NAMES: &[&str] = &["HOME", "USER", "LOGNAME", "TMPDIR", "PWD"];

/// One word as the command receives it.
pub(crate) struct Word {
    pub text: String,
    /// Bash may hand the command something else here: the word starts with an unquoted glob,
    /// or with `-` and holds one (`*`, `-*.rs`), so a file whose name reads as a flag could
    /// take its place; or it holds a variable whose value plan mode doesn't know (`$PATH`),
    /// which `text` keeps as written.
    pub unseen: bool,
}

/// A character of a word, and whether quoting or an escape made it literal.
pub(super) type Char = (char, bool);

/// A word as read, before its braces are expanded.
#[derive(Default)]
pub(super) struct Raw {
    pub chars: Vec<Char>,
    /// Holds a variable whose value plan mode doesn't know.
    pub unknown: bool,
}

/// The words of one segment, as [`words`] gives them.
pub(crate) fn unquote_words(segment: &str) -> Option<Vec<String>> {
    Some(words(segment)?.into_iter().map(|w| w.text).collect())
}

/// The words of one segment (no `|`, `;`, `&` or newline outside quotes), or `None` when
/// bash would read it differently from a plain word list: an unbalanced quote, an output
/// redirect (`>` outside quotes), a heredoc (`<<`), a backquote, `$(…)`, `${…}` beyond a bare
/// name, or `$_`, `$@` and the like. An input redirect's word (`<f`, `<<<'text'`) is stdin,
/// not an argument, and is left out; an unquoted `#` starting a word comments out the rest
/// of its line.
pub(crate) fn words(segment: &str) -> Option<Vec<Word>> {
    let chars: Vec<char> = segment.chars().collect();
    let mut words: Vec<Raw> = Vec::new();
    let mut word = Raw::default();
    let mut in_word = false;
    // The word being read is a redirect's, not the command's.
    let mut redirected = false;
    let finish =
        |words: &mut Vec<Raw>, word: &mut Raw, in_word: &mut bool, redirected: &mut bool| {
            if std::mem::take(in_word) {
                let word = std::mem::take(word);
                if !std::mem::take(redirected) {
                    words.push(word);
                }
            }
        };
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        match c {
            ' ' | '\t' | '\n' => {
                finish(&mut words, &mut word, &mut in_word, &mut redirected);
                at += 1;
                continue;
            }
            '#' if !in_word => {
                at = (at..chars.len())
                    .find(|&i| chars[i] == '\n')
                    .unwrap_or(chars.len());
                continue;
            }
            '\\' => {
                match chars.get(at + 1) {
                    Some('\n') => {
                        at += 2;
                        continue;
                    }
                    Some(&next) => word.chars.push((next, true)),
                    None => word.chars.push(('\\', true)),
                }
                at += 2;
            }
            '\'' => {
                let close = (at + 1..chars.len()).find(|&i| chars[i] == '\'')?;
                word.chars
                    .extend(chars[at + 1..close].iter().map(|&c| (c, true)));
                at = close + 1;
            }
            '"' => at = double_quoted(&chars, at + 1, &mut word)?,
            '$' if chars.get(at + 1) == Some(&'\'') => {
                at = ansi_quoted(&chars, at + 2, &mut word.chars)?
            }
            '$' if chars.get(at + 1) == Some(&'"') => {
                at = double_quoted(&chars, at + 2, &mut word)?
            }
            '$' => at = dollar(&chars, at, &mut word, false)?,
            '`' | '>' => return None,
            '<' => {
                match chars.get(at + 1) {
                    Some('(' | '>') => return None,
                    Some('<') if chars.get(at + 2) != Some(&'<') => return None,
                    _ => {}
                }
                let fd = !word.chars.is_empty()
                    && word
                        .chars
                        .iter()
                        .all(|&(c, quoted)| !quoted && c.is_ascii_digit());
                if fd {
                    word = Raw::default();
                    in_word = false;
                }
                finish(&mut words, &mut word, &mut in_word, &mut redirected);
                at += 1;
                while matches!(chars.get(at), Some('<' | '&')) {
                    at += 1;
                }
                redirected = true;
                continue;
            }
            _ => {
                word.chars.push((c, false));
                at += 1;
            }
        }
        in_word = true;
    }
    finish(&mut words, &mut word, &mut in_word, &mut redirected);
    Some(
        braces::expand(words)?
            .into_iter()
            .map(|word| Word {
                unseen: word.unknown || glob_flag(&word.chars),
                text: word.chars.into_iter().map(|(c, _)| c).collect(),
            })
            .collect(),
    )
}

/// `"…"` from `at`, just past the opening quote: a backslash escapes only `$`, `` ` ``, `"`,
/// `\` and a newline. Returns the index past the closing quote.
fn double_quoted(chars: &[char], mut at: usize, word: &mut Raw) -> Option<usize> {
    loop {
        match *chars.get(at)? {
            '"' => return Some(at + 1),
            '\\' => match chars.get(at + 1) {
                Some('\n') => at += 2,
                Some(&next @ ('$' | '`' | '"' | '\\')) => {
                    word.chars.push((next, true));
                    at += 2;
                }
                _ => {
                    word.chars.push(('\\', true));
                    at += 1;
                }
            },
            '`' => return None,
            '$' if chars.get(at + 1) == Some(&'"') => {
                word.chars.push(('$', true));
                at += 1;
            }
            '$' => at = dollar(chars, at, word, true)?,
            c => {
                word.chars.push((c, true));
                at += 1;
            }
        }
    }
}

/// `$'…'` from `at`, just past the opening quote. The escapes a read-only command plausibly
/// needs are decoded; numeric and control escapes (`\x2d`, `\055`, `\u…`, `\c…`), which could
/// spell anything, refuse the segment.
fn ansi_quoted(chars: &[char], mut at: usize, word: &mut Vec<Char>) -> Option<usize> {
    loop {
        match *chars.get(at)? {
            '\'' => return Some(at + 1),
            '\\' => {
                let decoded = match *chars.get(at + 1)? {
                    c @ ('\\' | '\'' | '"' | '?') => c,
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    'a' => '\x07',
                    'b' => '\x08',
                    'e' | 'E' => '\x1b',
                    'f' => '\x0c',
                    'v' => '\x0b',
                    _ => return None,
                };
                word.push((decoded, true));
                at += 2;
            }
            c => {
                word.push((c, true));
                at += 1;
            }
        }
    }
}

/// A `$` at `at`, outside single quotes. `$NAME` or `${NAME}` for a name in [`ENV_NAMES`]
/// becomes its value from the environment (empty when unset); `$?`, `$$`, `$#`, `$!` and
/// `${PIPESTATUS[N]}` are numbers. Any other variable stays as written and marks the word
/// unknown, as does one in `ENV_NAMES` whose value would split or glob. Anything that
/// computes or runs (`$(…)`, `$((…))`, `${X:-…}`) or that bash fills from the command itself
/// (`$_`, `$@`, `$-`, `$0`…) refuses the segment. A `$` before anything else is itself.
fn dollar(chars: &[char], at: usize, word: &mut Raw, quoted: bool) -> Option<usize> {
    let name_char = |c: &char| c.is_ascii_alphanumeric() || *c == '_';
    let (name, end) = match chars.get(at + 1) {
        Some('{') => {
            let close = (at + 2..chars.len()).find(|&i| chars[i] == '}')?;
            (&chars[at + 2..close], close + 1)
        }
        Some(c) if c.is_ascii_alphabetic() || *c == '_' => {
            let len = chars[at + 1..].iter().take_while(|c| name_char(c)).count();
            (&chars[at + 1..at + 1 + len], at + 1 + len)
        }
        Some('?' | '$' | '#' | '!') => {
            word.chars.push(('0', true));
            return Some(at + 2);
        }
        Some(c) if c.is_ascii_digit() || matches!(c, '(' | '[' | '@' | '*' | '-') => return None,
        _ => {
            word.chars.push(('$', quoted));
            return Some(at + 1);
        }
    };
    let name_text: String = name.iter().collect();
    if let Some(index) = name_text
        .strip_prefix("PIPESTATUS[")
        .and_then(|rest| rest.strip_suffix(']'))
        && !index.is_empty()
        && index.chars().all(|c| c.is_ascii_digit())
    {
        word.chars.push(('0', true));
        return Some(end);
    }
    let valid = name
        .first()
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == '_')
        && name.iter().all(name_char)
        && name != ['_'];
    if !valid {
        return None;
    }
    let value = ENV_NAMES
        .contains(&name_text.as_str())
        .then(|| std::env::var(&name_text).unwrap_or_default())
        .filter(|value| {
            quoted
                || (name_text != "PWD"
                    && !value
                        .chars()
                        .any(|c| c.is_whitespace() || matches!(c, '*' | '?' | '[' | '{')))
        });
    match value {
        Some(value) => word.chars.extend(value.chars().map(|c| (c, true))),
        None => {
            word.chars.extend(chars[at..end].iter().map(|&c| (c, true)));
            word.unknown = true;
        }
    }
    Some(end)
}

/// A word bash may replace with file names, one of which could read as a flag: it holds an
/// unquoted `*`, `?` or `[`, and the text before it is empty or starts with `-`.
fn glob_flag(word: &[Char]) -> bool {
    let Some(glob) = word
        .iter()
        .position(|&(c, quoted)| !quoted && matches!(c, '*' | '?' | '['))
    else {
        return false;
    };
    glob == 0 || word[0].0 == '-'
}
