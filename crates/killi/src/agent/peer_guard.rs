//! Refuses tree-moving git commands while another session works in this tree.
//!
//! Best effort, stated plainly: only a literal `git` command in the `bash` tool is
//! inspected. `bash -c "…"`, `xargs git`, scripts and aliases get through. The peer
//! reminder stays as the backstop.

/// The tree-moving git verb `command` runs, if any. Each `|`, `;`, `&` and newline
/// segment is checked on its own; leading `VAR=x` assignments and `cd <dir>` prefixes
/// are skipped, and git's global options before the subcommand are accepted.
pub fn moving_verb(command: &str) -> Option<&'static str> {
    command.split(['|', ';', '&', '\n']).find_map(segment_verb)
}

/// A literal directory `command` points at (`cd <dir>`, `git -C <dir>`,
/// `--work-tree[=]<dir>`, `--git-dir[=]<dir>`), for resolving which tree it acts on.
/// The last one wins.
pub fn command_dir(command: &str) -> Option<String> {
    let mut found = None;
    for segment in command.split(['|', ';', '&', '\n']) {
        if let Some(dir) = segment_dir(segment) {
            found = Some(dir);
        }
    }
    found
}

fn segment_verb(segment: &str) -> Option<&'static str> {
    let words: Vec<&str> = segment.split_whitespace().collect();
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

fn segment_dir(segment: &str) -> Option<String> {
    let words: Vec<&str> = segment.split_whitespace().collect();
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
    fn the_literal_directory_is_reported() {
        assert_eq!(
            command_dir("cd sub && git restore f"),
            Some("sub".to_string())
        );
        assert_eq!(command_dir("git -C . restore f"), Some(".".to_string()));
        assert_eq!(command_dir("git status"), None);
    }
}
