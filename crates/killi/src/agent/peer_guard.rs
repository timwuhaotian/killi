//! Refuses tree-moving git commands while another session works in this tree.
//!
//! Best effort, stated plainly: only a literal `git` command in the `bash` tool is
//! inspected. `bash -c "…"`, `xargs git`, scripts and aliases get through. The peer
//! reminder stays as the backstop.

/// The tree-moving git verb `command` runs, if any. Each segment between `|`, `;`, `&` and
/// newlines outside quotes is checked on its own; leading `VAR=x` assignments and `cd <dir>`
/// prefixes are skipped, and git's global options before the subcommand are accepted.
/// Each line is then read again on its own, so a quote or heredoc the split misreads can't
/// hide a later line; a line of a quoted message that starts with `git stash` is refused too.
pub fn moving_verb(command: &str) -> Option<&'static str> {
    let verb = |segment: &str| segment_verb(&words(segment));
    crate::plan::segments(command)
        .into_iter()
        .find_map(verb)
        .or_else(|| {
            command
                .lines()
                .flat_map(crate::plan::segments)
                .find_map(verb)
        })
}

/// A literal directory `command` points at (`cd <dir>`, `git -C <dir>`,
/// `--work-tree[=]<dir>`, `--git-dir[=]<dir>`), for resolving which tree it acts on.
/// The last one wins.
pub fn command_dir(command: &str) -> Option<String> {
    let mut found = None;
    for segment in crate::plan::segments(command) {
        if let Some(dir) = segment_dir(&words(segment)) {
            found = Some(dir);
        }
    }
    found
}

/// The words of a segment with quotes removed, or split at whitespace and parentheses when
/// bash would do more with it (`$(…)`, a redirect, a subshell), so a verb is still seen.
/// Braces are not expanded: this runs on every `bash` call, and its cost stays linear.
fn words(segment: &str) -> Vec<String> {
    crate::plan::words::unquote_words(segment).unwrap_or_else(|| {
        segment
            .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
            .filter(|word| !word.is_empty())
            .map(str::to_string)
            .collect()
    })
}

fn segment_verb(words: &[String]) -> Option<&'static str> {
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let mut words = words.as_slice();
    loop {
        while words.first().is_some_and(|w| is_env_assign(w)) {
            words = &words[1..];
        }
        if words.first() == Some(&"cd") {
            words = &words[1..];
            if !words.is_empty() {
                words = &words[1..];
            }
            continue;
        }
        break;
    }
    let first = words.first()?;
    if *first != "git" && !first.ends_with("/git") {
        return None;
    }
    let mut i = 1;
    while let Some(word) = words.get(i) {
        if *word == "-C" || *word == "--git-dir" || *word == "--work-tree" {
            i += 2;
        } else if word.starts_with("--git-dir=")
            || word.starts_with("--work-tree=")
            || word.starts_with("-C") && word.len() > 2
        {
            i += 1;
        } else if *word == "-c" {
            i += 2;
        } else if word.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    match words.get(i).copied() {
        Some("checkout") => Some("checkout"),
        Some("switch") => Some("switch"),
        Some("restore") => Some("restore"),
        Some("reset") => Some("reset"),
        Some("clean") => Some("clean"),
        Some("stash") => match words.get(i + 1).copied() {
            Some("list" | "show") => None,
            _ => Some("stash"),
        },
        _ => None,
    }
}

fn segment_dir(words: &[String]) -> Option<String> {
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let mut words = words.as_slice();
    let mut found = None;
    loop {
        while words.first().is_some_and(|w| is_env_assign(w)) {
            words = &words[1..];
        }
        if words.first() == Some(&"cd") && words.len() > 1 {
            found = Some(words[1].to_string());
            words = &words[2..];
            continue;
        }
        break;
    }
    let Some(first) = words.first() else {
        return found;
    };
    if *first != "git" && !first.ends_with("/git") {
        return found;
    }
    let mut i = 1;
    while let Some(word) = words.get(i) {
        if *word == "-C" || *word == "--work-tree" || *word == "--git-dir" {
            if let Some(dir) = words.get(i + 1) {
                found = Some(dir.to_string());
            }
            i += 2;
        } else if let Some(dir) = word
            .strip_prefix("--work-tree=")
            .or_else(|| word.strip_prefix("--git-dir="))
        {
            found = Some(dir.to_string());
            i += 1;
        } else if let Some(dir) = word.strip_prefix("-C") {
            if !dir.is_empty() {
                found = Some(dir.to_string());
            }
            i += 1;
        } else if *word == "-c" {
            i += 2;
        } else if word.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    found
}

/// A leading `VAR=x` assignment, not a flag: `FOO=1 git restore`.
fn is_env_assign(word: &str) -> bool {
    if word.starts_with('-') {
        return false;
    }
    let Some(eq) = word.find('=') else {
        return false;
    };
    let (name, _) = word.split_at(eq);
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.chars().next().is_some_and(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::{command_dir, moving_verb};

    #[test]
    fn tree_moving_verbs_are_found() {
        for command in [
            "git restore --staged --worktree -- f",
            "git checkout -- .",
            "git checkout -b x",
            "git switch main",
            "git stash",
            "git stash pop",
            "git reset --hard",
            "git reset HEAD~1",
            "git clean -fd",
            "cd sub && git restore f",
            "git -C . restore f",
            "git -c core.x=y checkout f",
            "FOO=1 git restore f",
            "true; git stash",
        ] {
            assert!(moving_verb(command).is_some(), "{command}");
        }
    }

    #[test]
    fn read_only_git_and_lookalikes_pass() {
        for command in [
            "git stash list",
            "git stash show -p",
            "git status",
            "git diff",
            "git log",
            "git commit -m x",
            "git add .",
            "git worktree add ../w",
            "echo \"git restore\"",
        ] {
            assert_eq!(moving_verb(command), None, "{command}");
        }
    }

    #[test]
    fn separators_and_verbs_inside_quotes_are_text() {
        for command in [
            "git commit -m \"wip; git stash\"",
            "git commit -m 'a | git checkout -- .'",
            "echo 'x && git reset --hard'",
        ] {
            assert_eq!(moving_verb(command), None, "{command}");
        }
        assert_eq!(moving_verb("git 'stash'"), Some("stash"));
        assert_eq!(moving_verb("git \"checkout\" -- ."), Some("checkout"));
        assert_eq!(
            moving_verb("git checkout $(git rev-parse HEAD~1)"),
            Some("checkout")
        );
        assert_eq!(
            command_dir("git -C 'my repo' commit -m 'x; cd /tmp' && git reset --hard"),
            Some("my repo".to_string())
        );
    }

    #[test]
    fn comments_heredocs_and_odd_lines_hide_no_verb() {
        assert_eq!(
            moving_verb("# Check what's changed\ngit stash"),
            Some("stash")
        );
        assert_eq!(
            moving_verb("git commit -F - <<EOF\nFix the parser's bug\nEOF\ngit checkout main"),
            Some("checkout")
        );
        assert_eq!(moving_verb("echo $((1<<2))\ngit stash"), Some("stash"));
        assert_eq!(moving_verb("echo 'it\ngit reset --hard"), Some("reset"));
        assert_eq!(moving_verb("ls # git stash"), None);
        assert_eq!(
            moving_verb("git commit -F - <<EOF\nkeep git stash out\nEOF"),
            None
        );
    }

    #[test]
    fn redirects_and_subshells_hide_no_verb() {
        assert_eq!(moving_verb("git stash 2>&1 | tail -3"), Some("stash"));
        assert_eq!(moving_verb("git stash >/tmp/out"), Some("stash"));
        assert_eq!(moving_verb("(git stash)"), Some("stash"));
        assert_eq!(
            moving_verb("(cd sub && git checkout -- .)"),
            Some("checkout")
        );
        assert_eq!(moving_verb("git stash list 2>/dev/null"), None);
    }

    #[test]
    fn long_commands_stay_cheap() {
        let inputs = [
            "cat {1..1023}{1..1};".repeat(10_000),
            "git status {a,b}{c,d}{e,f};".repeat(8_000),
            "cd x && ".repeat(25_000),
        ];
        let started = std::time::Instant::now();
        for input in &inputs {
            assert_eq!(moving_verb(input), None);
            command_dir(input);
        }
        // A few ms each in a release build; brace expansion here took seconds.
        let limit = if cfg!(debug_assertions) { 3000 } else { 300 };
        assert!(
            started.elapsed() < std::time::Duration::from_millis(limit),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn the_literal_directory_is_reported() {
        assert_eq!(
            command_dir("cd sub && git restore f"),
            Some("sub".to_string())
        );
        assert_eq!(command_dir("git -C . restore f"), Some(".".to_string()));
        assert_eq!(command_dir("git status"), None);
    }
}
