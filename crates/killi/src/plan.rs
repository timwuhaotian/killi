//! Plan mode: the tool list stays the same, and calls are checked when they run. Writes are
//! refused, and bash runs only commands known to be read-only.

/// Commands whose every form only reads. Matched as the start of each pipeline segment.
const READ_ONLY: &[&str] = &[
    "ls", "cat", "head", "tail", "wc", "rg", "grep", "egrep", "fgrep", "find", "fd", "tree", "file", "stat", "du",
    "df", "pwd", "which", "type", "whoami", "uname", "date", "echo", "printf", "basename", "dirname", "realpath",
    "readlink", "sort", "uniq", "cut", "tr", "diff", "cmp", "jq", "nl", "column", "less", "more", "sed -n",
    "git log", "git diff", "git status", "git show", "git branch", "git blame", "git rev-parse", "git ls-files",
    "git grep", "git remote -v", "git tag", "git describe", "git shortlog", "git config --get",
    "cargo check", "cargo clippy", "cargo tree", "cargo metadata", "cargo --version", "rustc --version",
    "npm ls", "npm view", "npm --version", "node --version", "pnpm ls", "tsc --noEmit", "go vet", "go list",
    "python3 --version", "swift --version", "xcodebuild -list",
    "killi sessions", "killi ask",
];

/// Whether `command` may run in plan mode, given extra allowed prefixes from config.
pub fn bash_allowed(command: &str, extra: &[String]) -> bool {
    let cleaned = command.replace("2>&1", "").replace(">/dev/null", "").replace("> /dev/null", "");
    if cleaned.contains('>') || cleaned.contains("$(") || cleaned.contains('`') || cleaned.contains("<(") {
        return false;
    }
    cleaned
        .split(['|', ';', '&', '\n'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .all(|segment| segment_allowed(segment, extra))
}

fn segment_allowed(segment: &str, extra: &[String]) -> bool {
    let words: Vec<&str> = segment.split_whitespace().collect();
    let normalized = words.join(" ");
    let starts = |prefix: &str| normalized == prefix || normalized.starts_with(&format!("{prefix} "));
    if words.first() == Some(&"find") && words.iter().any(|w| matches!(*w, "-delete" | "-exec" | "-execdir" | "-ok" | "-fprint")) {
        return false;
    }
    if words.first() == Some(&"sed") && words.iter().any(|w| w.starts_with("-i")) {
        return false;
    }
    READ_ONLY.iter().any(|prefix| starts(prefix)) || extra.iter().any(|prefix| starts(prefix))
}

pub const ON: &str = "<system-reminder>Plan mode is on. Research and plan only: write and edit are blocked, and bash runs only read-only commands. When your plan is ready, call exit_plan_mode with it and wait for the user's approval.</system-reminder>";

pub const OFF: &str = "<system-reminder>Plan mode is off. You may change files again.</system-reminder>";

#[cfg(test)]
mod tests {
    use super::bash_allowed;

    #[test]
    fn read_only_commands_pass() {
        for command in [
            "ls -la",
            "rg slugify src | head -20",
            "git log --oneline -5 && git status",
            "cargo check 2>&1 | tail -30",
            "find . -name '*.rs'",
            "sed -n 1,40p src/lib.rs",
            "killi sessions",
            "killi ask 4321 \"which files are you changing?\"",
        ] {
            assert!(bash_allowed(command, &[]), "{command}");
        }
    }

    #[test]
    fn writes_are_refused() {
        for command in [
            "rm -rf target",
            "echo hi > notes.txt",
            "cat a | tee b",
            "find . -name '*.tmp' -delete",
            "sed -i 's/a/b/' x",
            "git checkout -- .",
            "ls $(touch x)",
            "cargo test",
            "npm install",
            "killi tell 4321 'I pushed the base'",
        ] {
            assert!(!bash_allowed(command, &[]), "{command}");
        }
    }

    #[test]
    fn config_can_allow_more() {
        assert!(bash_allowed("make -n all", &["make -n".to_string()]));
    }
}
