//! Plan mode: the tool list stays the same, and calls are checked when they run. Writes are
//! refused, and bash runs only commands known to be read-only.

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
    "git remote -v",
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
/// in `grep -E 'a|b'`. `$'…'` is like `'…'` with backslash escapes; `'…'` has none.
fn segments(command: &str) -> Vec<&str> {
    #[derive(PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
        Ansi,
    }
    let bytes = command.as_bytes();
    let mut parts = Vec::new();
    let (mut start, mut at, mut quote) = (0, 0, Quote::None);
    while at < bytes.len() {
        let byte = bytes[at];
        match quote {
            Quote::None => match byte {
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
                b'|' | b';' | b'&' | b'\n' => {
                    parts.push(&command[start..at]);
                    start = at + 1;
                }
                _ => {}
            },
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
    parts.push(&command[start..]);
    parts
}

/// The first segment of `command` plan mode refuses, if any, so the refusal can name it.
/// `extra` holds additional allowed prefixes from config.
/// The segment comes from the cleaned command, so a let-through redirect (`2>&1`,
/// `>/dev/null`) is missing from its text.
pub fn bash_refused(command: &str, extra: &[String]) -> Option<String> {
    let cleaned = command
        .replace("2>&1", "")
        .replace(">/dev/null", "")
        .replace("> /dev/null", "");
    segments(&cleaned)
        .into_iter()
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .find(|segment| {
            segment.contains('>')
                || segment.contains("$(")
                || segment.contains('`')
                || segment.contains("<(")
                || !segment_allowed(segment, extra)
        })
        .map(str::to_string)
}

/// The command a segment runs, for the checks: `git -C <dir> log` is `git log`, and `xargs
/// wc -l` runs `wc` on what it reads (`xargs` alone runs `echo`). `git -c` stays in view,
/// since a config value can name a command to run.
fn command_words(segment: &str) -> Vec<&str> {
    let mut words = segment.split_whitespace().peekable();
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
        return std::iter::once("git").chain(words).collect();
    }
    if words.peek() == Some(&"xargs") {
        words.next();
        while let Some(word) = words.peek().copied() {
            if word == "--" || !word.starts_with('-') {
                break;
            }
            // The flags that take their value as the next word; `-e` takes it only attached.
            let takes_value = matches!(
                word,
                "-I" | "-J" | "-n" | "-N" | "-d" | "-a" | "-P" | "-s" | "-S" | "-L" | "-E" | "-R"
            );
            words.next();
            if takes_value {
                words.next();
            }
        }
        if words.peek().is_none() {
            return vec!["echo"];
        }
    }
    words.collect()
}

fn segment_allowed(segment: &str, extra: &[String]) -> bool {
    let words = command_words(segment);
    let normalized = words.join(" ");
    let starts =
        |prefix: &str| normalized == prefix || normalized.starts_with(&format!("{prefix} "));
    if words.first() == Some(&"find")
        && words
            .iter()
            .any(|w| matches!(*w, "-delete" | "-exec" | "-execdir" | "-ok" | "-fprint"))
    {
        return false;
    }
    if words.first() == Some(&"sed") && words.iter().any(|w| w.starts_with("-i")) {
        return false;
    }
    if writes_behind_a_read_only_name(&words) {
        return false;
    }
    READ_ONLY.iter().any(|prefix| starts(prefix)) || extra.iter().any(|prefix| starts(prefix))
}

/// Flags and subcommands that write behind a read-only command name: `sort -o`, `git tag v1,
/// `sed -n ... w out`. The whole segment is refused when one shows up.
fn writes_behind_a_read_only_name(words: &[&str]) -> bool {
    let has = |flags: &[&str]| {
        words.iter().skip(1).any(|w| {
            flags
                .iter()
                .any(|f| *w == *f || w.starts_with(&format!("{f}=")))
        })
    };
    match words.first() {
        Some(&"fd") => has(&["-x", "-X", "--exec", "--exec-batch"]),
        Some(&"rg") => has(&["--pre"]),
        Some(&"sort") => words
            .iter()
            .skip(1)
            .any(|w| *w == "-o" || w.starts_with("--output") || short_has(w, 'o')),
        Some(&"tree") => has(&["-o", "--output"]),
        Some(&"uniq") => uniq_has_output(words),
        Some(&"sed") => sed_writes(words),
        Some(&"git") => git_writes(words),
        Some(&"cargo") => {
            matches!(words.get(..2), Some(["cargo", "clippy" | "check"])) && has(&["--fix"])
        }
        _ => false,
    }
}

/// A combined short flag like `-ro`: `-`, not `--`, with `flag` among the letters.
fn short_has(word: &str, flag: char) -> bool {
    word.starts_with('-') && !word.starts_with("--") && word[1..].contains(flag)
}

/// `uniq` with an input and an output file writes the second one. Flags that take a value
/// (`-f`, `-s`, `-w`) don't count their value as a file.
fn uniq_has_output(words: &[&str]) -> bool {
    let mut operands = 0;
    let mut skip_next = false;
    for word in words.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if *word == "--" {
            continue;
        }
        if word.starts_with('-') && word.len() > 1 {
            skip_next = matches!(
                *word,
                "-f" | "-s" | "-w" | "--skip-fields" | "--skip-chars" | "--check-chars"
            );
            continue;
        }
        operands += 1;
    }
    operands >= 2
}

/// A `w` command or `s///w` flag with a filename: `sed -n` still writes it.
fn sed_writes(words: &[&str]) -> bool {
    for word in words.iter().skip(1) {
        let mut script = *word;
        if script == "-e" {
            continue;
        }
        if let Some(rest) = script
            .strip_prefix("--expression=")
            .or_else(|| script.strip_prefix("-e"))
        {
            script = rest;
        }
        if script.starts_with('-') {
            continue;
        }
        let script = script.trim_matches(|c| c == '\'' || c == '"' || c == ';');
        if script == "w"
            || script.trim_start_matches(|c: char| c.is_ascii_digit() || ",.$".contains(c)) == "w"
        {
            return true;
        }
        if sed_substitute_writes(script) {
            return true;
        }
        if let Some(pos) = script.rfind("/w") {
            let after = &script[pos + 2..];
            if after.is_empty() || !after.starts_with('/') {
                return true;
            }
        }
    }
    false
}

/// An `s<delim>…<delim>…<delim>w` substitution writes its file, whatever delimiter it uses:
/// `s|a|b|w out` writes just as `s/a/b/w out` does.
fn sed_substitute_writes(script: &str) -> bool {
    let mut chars = script.char_indices();
    if chars.next().map(|(_, c)| c) != Some('s') {
        return false;
    }
    let Some((_, delim)) = chars.next() else {
        return false;
    };
    if delim.is_alphanumeric() || delim.is_whitespace() || delim == '\\' {
        return false;
    }
    let rest = &script[1 + delim.len_utf8()..];
    let mut seen = 0;
    let mut escaped = false;
    for (at, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        escaped = c == '\\';
        if c == delim {
            seen += 1;
            if seen == 2 {
                let flags = &rest[at + delim.len_utf8()..];
                return flags
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .contains('w');
            }
        }
    }
    false
}

/// `git branch` creating, deleting or renaming a branch, `git tag` creating or deleting one,
/// and `git diff --output=`: all write. Bare `git branch`, `git tag` and listings stay allowed.
fn git_writes(words: &[&str]) -> bool {
    if words.len() < 2 || words[0] != "git" {
        return false;
    }
    match words[1] {
        "branch" if words.len() > 2 => {
            let takes_value = [
                "--contains",
                "--no-contains",
                "--merged",
                "--no-merged",
                "--points-at",
                "--sort",
                "--format",
                "--set-upstream-to",
            ];
            let writes = [
                "--force",
                "--delete",
                "--move",
                "--copy",
                "--set-upstream-to",
                "--unset-upstream",
                "--edit-description",
            ];
            let listed = words
                .iter()
                .skip(2)
                .any(|w| *w == "-l" || *w == "--list" || short_has(w, 'l'));
            let mut skip_next = false;
            for word in words.iter().skip(2) {
                if skip_next {
                    skip_next = false;
                    continue;
                }
                let flag = word.split('=').next().unwrap_or(word);
                if writes.contains(&flag) {
                    return true;
                }
                if takes_value.contains(&flag) {
                    skip_next = !word.contains('=');
                    continue;
                }
                if word.starts_with('-')
                    && !word.starts_with("--")
                    && "dDmMcCfu".chars().any(|f| short_has(word, f))
                {
                    return true;
                }
                if !word.starts_with('-') && !listed {
                    return true;
                }
            }
            false
        }
        "tag" if words.len() > 2 => {
            // List filters imply `--list`, so `--contains HEAD` lists just as `-l` does.
            let listed = words.iter().skip(2).any(|w| {
                matches!(*w, "-l" | "--list")
                    || short_has(w, 'l')
                    || [
                        "--contains",
                        "--no-contains",
                        "--merged",
                        "--no-merged",
                        "--points-at",
                    ]
                    .contains(&w.split('=').next().unwrap_or(w))
            });
            words.iter().skip(2).any(|w| {
                let flag = w.split('=').next().unwrap_or(w);
                if matches!(
                    flag,
                    "-l" | "--list"
                        | "-n"
                        | "--sort"
                        | "--format"
                        | "--contains"
                        | "--no-contains"
                        | "--merged"
                        | "--no-merged"
                        | "--points-at"
                        | "--column"
                        | "--no-column"
                ) || (flag == "-n"
                    || (flag.starts_with("-n") && flag[2..].chars().all(|c| c.is_ascii_digit())))
                {
                    return false;
                }
                if listed && !w.starts_with('-') {
                    return false;
                }
                if listed && "dasufme".chars().any(|f| short_has(w, f)) {
                    return true;
                }
                !listed
            })
        }
        "diff" => words
            .iter()
            .any(|w| *w == "--output" || w.starts_with("--output=")),
        _ => false,
    }
}

pub const ON: &str = "<system-reminder>Plan mode is on. Research and plan only: write and edit are blocked, and bash runs only read-only commands. When your plan is ready, call exit_plan_mode with it and wait for the user's approval.</system-reminder>";

pub const OFF: &str =
    "<system-reminder>Plan mode is off. You may change files again.</system-reminder>";

#[cfg(test)]
mod tests;
