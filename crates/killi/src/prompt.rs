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
    fn nested_agents_files_are_found_once() {
        let root = std::env::temp_dir().join(format!("killi-nested-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("web/src")).unwrap();
        fs::write(root.join("web/AGENTS.md"), "use pnpm").unwrap();
        let mut instructions = Instructions::new(&root, &[]);
        instructions.touched(&root.join("web/src/app.ts"));
        instructions.touched(&root.join("web/src/other.ts"));
        let found = instructions.take_found();
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("web/AGENTS.md"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claude_md_imports_are_expanded() {
        let dir = std::env::temp_dir().join(format!("killi-prompt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("RULES.md"), "be terse").unwrap();
        assert_eq!(expand_imports(&dir, "intro\n@RULES.md\n@missing.md"), "intro\nbe terse\n@missing.md");
        fs::remove_dir_all(dir).unwrap();
    }
}

/// Instruction files beyond the system prompt: nested `AGENTS.md` files found as the agent
/// works in subfolders, and loaded ones that change on disk. Both reach the model as appended
/// reminders, so the system prompt and its cache stay as they are.
#[derive(Clone, Default)]
pub struct Instructions {
    root: PathBuf,
    known: std::collections::HashSet<PathBuf>,
    found: Vec<String>,
    loaded: Vec<(PathBuf, Option<std::time::SystemTime>)>,
}

impl Instructions {
    pub fn new(cwd: &Path, loaded: &[PathBuf]) -> Self {
        let root = cwd.ancestors().find(|dir| dir.join(".git").exists()).unwrap_or(cwd).to_path_buf();
        Self {
            root,
            known: loaded.iter().cloned().collect(),
            found: Vec::new(),
            loaded: loaded.iter().map(|p| (p.clone(), modified(p))).collect(),
        }
    }

    /// Notes any AGENTS.md between the project root and `path` that the model hasn't seen.
    pub fn touched(&mut self, path: &Path) {
        let Ok(relative) = path.strip_prefix(&self.root) else { return };
        let mut dir = self.root.clone();
        for part in relative.parent().into_iter().flat_map(Path::components) {
            dir.push(part);
            let Some(file) = first_existing(&dir, &["AGENTS.md", "agents.md", "CLAUDE.md"]) else { continue };
            let file = fs::canonicalize(&file).unwrap_or(file);
            if self.known.insert(file.clone()) && path != file {
                self.found.push(file.display().to_string());
            }
        }
    }

    pub fn take_found(&mut self) -> Vec<String> {
        std::mem::take(&mut self.found)
    }

    /// Loaded instruction files that changed since the last check.
    pub fn changed(&mut self) -> Vec<String> {
        let mut changed = Vec::new();
        for (path, seen) in &mut self.loaded {
            let now = modified(path);
            if now != *seen {
                *seen = now;
                changed.push(path.display().to_string());
            }
        }
        changed
    }
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}
