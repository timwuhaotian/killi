//! Whether a `sed` command line writes or runs something: an in-place flag, a script file
//! plan mode can't read, or a script with a `w`, `W` or `e` command or an `s///w` or `s///e`
//! flag anywhere in it, `p;w out` included.

/// `words` is a `sed` command line, quotes already removed.
pub(super) fn sed_writes(words: &[&str]) -> bool {
    let mut scripts = Vec::new();
    let mut operands = Vec::new();
    let mut options_done = false;
    let mut at = 1;
    while let Some(&word) = words.get(at) {
        at += 1;
        if options_done || word == "-" || !word.starts_with('-') {
            operands.push(word);
            continue;
        }
        if word == "--" {
            options_done = true;
            continue;
        }
        if let Some(long) = word.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let abbreviates = |full: &str| !name.is_empty() && full.starts_with(name);
            if abbreviates("in-place") || abbreviates("file") {
                return true;
            }
            if abbreviates("expression") || abbreviates("line-length") {
                let value = value.or_else(|| {
                    at += 1;
                    words.get(at - 1).copied()
                });
                if abbreviates("expression") {
                    scripts.extend(value);
                }
            }
            continue;
        }
        for (i, letter) in word.char_indices().skip(1) {
            match letter {
                // GNU `-i[SUFFIX]`, BSD `-i SUFFIX` and `-I SUFFIX`; `-f` reads a script file.
                'i' | 'I' | 'f' => return true,
                // GNU `-l N` takes a value and BSD `-l` doesn't: `N` is left as an operand,
                // and a number read as the script refuses.
                'e' => {
                    let rest = &word[i + 1..];
                    if rest.is_empty() {
                        at += 1;
                        scripts.extend(words.get(at - 1).copied());
                    } else {
                        scripts.push(rest);
                    }
                    break;
                }
                _ => {}
            }
        }
    }
    if scripts.is_empty() {
        scripts.extend(operands.first());
    }
    scripts.iter().any(|script| {
        let chars: Vec<char> = script.chars().collect();
        // A `/` inside `[…]` ends a regex for some seds and not others: read it both ways.
        script_writes(&chars, true) || script_writes(&chars, false)
    })
}

/// Walks a sed script command by command. True when a command writes or runs something, or
/// when the script doesn't parse, since what sed makes of it then is unknown.
fn script_writes(s: &[char], brackets: bool) -> bool {
    let mut at = 0;
    loop {
        while at < s.len() && (s[at].is_whitespace() || matches!(s[at], ';' | '{' | '}')) {
            at += 1;
        }
        if at >= s.len() {
            return false;
        }
        if s[at] == '#' {
            at = line_end(s, at);
            continue;
        }
        let Some(next) = addresses(s, at, brackets) else {
            return true;
        };
        at = next;
        let Some(&command) = s.get(at) else {
            return true;
        };
        at += 1;
        at = match command {
            'w' | 'W' | 'e' => return true,
            's' => match substitute(s, at, brackets) {
                Some(next) => next,
                None => return true,
            },
            'y' => {
                let Some(&delim) = s.get(at) else { return true };
                match regex_end(s, at + 1, delim, false)
                    .and_then(|next| regex_end(s, next, delim, false))
                {
                    Some(next) => next,
                    None => return true,
                }
            }
            'a' | 'i' | 'c' | 'r' | 'R' => text_end(s, at),
            ':' | 'b' | 't' | 'T' | 'v' => label_end(s, at),
            'q' | 'Q' | 'l' | 'L' => {
                while at < s.len() && (s[at] == ' ' || s[at].is_ascii_digit()) {
                    at += 1;
                }
                at
            }
            '=' | 'd' | 'D' | 'g' | 'G' | 'h' | 'H' | 'n' | 'N' | 'p' | 'P' | 'x' | 'z' | 'F'
            | '{' => at,
            _ => return true,
        };
    }
}

/// Past up to two addresses and any `!`: a line number, `first~step`, `$`, `/re/` or `\cREc`
/// with its `I`/`M` flags, and a second address's `+N` or `~N`. `None` when one doesn't parse.
fn addresses(s: &[char], mut at: usize, brackets: bool) -> Option<usize> {
    for _ in 0..2 {
        match s.get(at) {
            Some(c) if c.is_ascii_digit() || matches!(c, '+' | '~') => {
                at += 1;
                while at < s.len() && (s[at].is_ascii_digit() || s[at] == '~') {
                    at += 1;
                }
            }
            Some('$') => at += 1,
            Some('/') => at = regex_end(s, at + 1, '/', brackets)?,
            Some('\\') => at = regex_end(s, at + 2, *s.get(at + 1)?, brackets)?,
            _ => break,
        }
        while at < s.len() && matches!(s[at], 'I' | 'M') {
            at += 1;
        }
        while at < s.len() && s[at] == ' ' {
            at += 1;
        }
        if s.get(at) != Some(&',') {
            break;
        }
        at += 1;
        while at < s.len() && s[at] == ' ' {
            at += 1;
        }
    }
    while at < s.len() && matches!(s[at], ' ' | '!') {
        at += 1;
    }
    Some(at)
}

/// Past the `delim` that ends a regex or replacement starting at `at`. With `brackets`, a
/// `[…]` bracket expression doesn't end on `delim`.
fn regex_end(s: &[char], mut at: usize, delim: char, brackets: bool) -> Option<usize> {
    if delim == '\n' || delim == '\\' {
        return None;
    }
    while let Some(&c) = s.get(at) {
        match c {
            '\\' => at += 2,
            '\n' => return None,
            _ if c == delim => return Some(at + 1),
            '[' if brackets => {
                at += 1;
                if s.get(at) == Some(&'^') {
                    at += 1;
                }
                if s.get(at) == Some(&']') {
                    at += 1;
                }
                while *s.get(at)? != ']' {
                    if s[at] == '['
                        && let Some(&class @ (':' | '.' | '=')) = s.get(at + 1)
                    {
                        at += 2;
                        while !(s.get(at)? == &class && s.get(at + 1) == Some(&']')) {
                            at += 1;
                        }
                        at += 1;
                    }
                    at += 1;
                }
                at += 1;
            }
            _ => at += 1,
        }
    }
    None
}

/// Past an `s` command's regex, replacement and flags, starting at its delimiter. `None`
/// when it doesn't parse or a flag writes (`w`) or runs the pattern space (`e`).
fn substitute(s: &[char], at: usize, brackets: bool) -> Option<usize> {
    let delim = *s.get(at)?;
    let mut at = regex_end(s, at + 1, delim, brackets)?;
    at = regex_end(s, at, delim, false)?;
    while let Some(&c) = s.get(at) {
        match c {
            ';' | '\n' | '}' | '#' => break,
            'g' | 'p' | 'i' | 'I' | 'm' | 'M' | ' ' | '\t' => {}
            _ if c.is_ascii_digit() => {}
            _ => return None,
        }
        at += 1;
    }
    Some(at)
}

/// `a`, `i`, `c`, `r` and `R` take the rest of the line, and a text line ending in `\` goes on.
fn text_end(s: &[char], mut at: usize) -> usize {
    while at < s.len() && s[at] != '\n' {
        at += if s[at] == '\\' { 2 } else { 1 };
    }
    at
}

/// A label or version runs to the next `;` or newline.
fn label_end(s: &[char], mut at: usize) -> usize {
    while at < s.len() && !matches!(s[at], ';' | '\n') {
        at += 1;
    }
    at
}

/// Past the end of a `#` comment line.
fn line_end(s: &[char], at: usize) -> usize {
    (at..s.len()).find(|&i| s[i] == '\n').unwrap_or(s.len())
}
