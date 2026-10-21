//! The system prompt, built once per session and never re-rendered.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::home;
use crate::skills::Skills;

const BASE: &str = include_str!("prompt/system.md");

pub struct SystemPrompt {
    pub text: String,
    pub instruction_files: Vec<PathBuf>,
    /// Estimated at four characters per token.
    pub instruction_tokens: usize,
}

pub fn build(cwd: &Path, skills: &Skills) -> SystemPrompt {
    let mut text = BASE.replace("{cwd}", &cwd.display().to_string()).replace("{date}", &today());
    let files = instruction_files(cwd);
    let mut instructions = String::new();
    for (path, content) in &files {
        instructions.push_str(&format!("\n<file path=\"{}\">\n{}\n</file>\n", path.display(), content.trim()));
    }
    if !files.is_empty() {
        text.push_str(
            "\n# Instructions\nThe files below hold the user's and the project's standing instructions. Follow them; the user's messages in this conversation take precedence.\n",
        );
        text.push_str(&instructions);
    }
    text.push_str(&skills.prompt_section());
    SystemPrompt {
        text,
        instruction_files: files.into_iter().map(|(path, _)| path).collect(),
        instruction_tokens: instructions.len().div_ceil(4),
    }
}

/// `~/.agents/AGENTS.md`, then for each directory from the git root down to `cwd` its
/// `AGENTS.md`, or `CLAUDE.md` (with `@file` lines expanded) when there is no `AGENTS.md`.
fn instruction_files(cwd: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    if let Some(global) = first_existing(&home().join(".agents"), &["AGENTS.md", "agents.md"]) {
        push_nonempty(&mut found, global.clone(), fs::read_to_string(&global).unwrap_or_default());
    }
    let root = cwd.ancestors().find(|dir| dir.join(".git").exists()).unwrap_or(cwd);
    let mut chain: Vec<&Path> = cwd.ancestors().take_while(|dir| dir.starts_with(root)).collect();
    chain.reverse();
    for dir in chain {
        if let Some(agents) = first_existing(dir, &["AGENTS.md", "agents.md"]) {
            push_nonempty(&mut found, agents.clone(), fs::read_to_string(&agents).unwrap_or_default());
        } else if let Some(claude) = first_existing(dir, &["CLAUDE.md"]) {
            push_nonempty(&mut found, claude.clone(), expand_imports(dir, &fs::read_to_string(&claude).unwrap_or_default()));
        }
    }
    found
}

fn first_existing(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().map(|name| dir.join(name)).find(|path| path.is_file())
}

fn push_nonempty(found: &mut Vec<(PathBuf, String)>, path: PathBuf, content: String) {
    let canonical = fs::canonicalize(&path).unwrap_or(path);
    if !content.trim().is_empty() && !found.iter().any(|(seen, _)| *seen == canonical) {
        found.push((canonical, content));
    }
}

/// Replaces lines that are exactly `@path` with that file's contents, one level deep.
fn expand_imports(dir: &Path, text: &str) -> String {
    text.lines()
        .map(|line| match line.trim().strip_prefix('@') {
            Some(target) if !target.is_empty() && !target.contains(char::is_whitespace) => {
                let path = match target.strip_prefix("~/") {
                    Some(rest) => home().join(rest),
                    None => dir.join(target),
                };
                fs::read_to_string(path).unwrap_or_else(|_| line.to_string())
            }
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn today() -> String {
    // SAFETY: `time` and `localtime_r` only write to the locals passed in.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_md_imports_are_expanded() {
        let dir = std::env::temp_dir().join(format!("killi-prompt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("RULES.md"), "be terse").unwrap();
        assert_eq!(expand_imports(&dir, "intro\n@RULES.md\n@missing.md"), "intro\nbe terse\n@missing.md");
        fs::remove_dir_all(dir).unwrap();
    }
}
