//! The sessions a folder and this machine hold, newest first, read without a full load.

use super::*;

use std::io::{BufRead, BufReader};

pub struct Listed {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub first_prompt: String,
    pub prompts: usize,
}

/// This folder's sessions, newest first. The list streams each log and parses only the
/// lines that may hold a user message, so tool results and usage lines are never touched.
pub fn list(cwd: &Path) -> Vec<Listed> {
    let Ok(entries) = fs::read_dir(project_dir(cwd)) else {
        return Vec::new();
    };
    let mut sessions: Vec<Listed> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .filter_map(|path| {
            let modified = fs::metadata(&path).and_then(|m| m.modified()).ok()?;
            let (first_prompt, prompts) = scan_prompts(&path)?;
            Some(Listed {
                path,
                modified,
                first_prompt,
                prompts,
            })
        })
        .collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
    sessions
}

/// Whether `line` holds `needle` as raw bytes, without decoding it.
fn contains(line: &[u8], needle: &[u8]) -> bool {
    line.windows(needle.len()).any(|w| w == needle)
}

/// A log's typed prompts without parsing it whole: only lines that may hold a user
/// message or a compaction are parsed as entries, and the rest are skipped as bytes —
/// tool results and usage lines are never touched. A compaction's summary replaces the
/// prompts before it, the way `load` replays them, and a log whose first replayed
/// message isn't the system prompt stays out, the way `load` refuses it. `None` when
/// the log holds no typed prompt, or can't be read.
pub(super) fn scan_prompts(path: &Path) -> Option<(String, usize)> {
    let file = File::open(path).ok()?;
    let mut prompts: Vec<String> = Vec::new();
    let mut anchored: Option<bool> = None;
    for line in BufReader::new(file).split(b'\n') {
        let line = line.ok()?;
        if anchored.is_some()
            && !contains(&line, b"\"user\"")
            && !contains(&line, b"\"compaction\"")
        {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        let Ok(entry) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        match entry["type"].as_str() {
            Some("message") => {
                if anchored.is_none() {
                    anchored = Some(entry["message"]["role"] == "system");
                }
                if entry["message"]["role"] == "user"
                    && let Some(content) = entry["message"]["content"].as_str()
                {
                    prompts.extend(text_prompts(content));
                }
            }
            Some("compaction") => {
                anchored = Some(
                    entry["messages"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .next()
                        .is_some_and(|m| m["role"] == "system"),
                );
                prompts.clear();
                prompts.extend(
                    entry["messages"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|m| m["role"] == "user")
                        .filter_map(|m| m["content"].as_str())
                        .flat_map(text_prompts),
                );
            }
            _ => {}
        }
    }
    if anchored != Some(true) {
        return None;
    }
    let first = prompts.first()?;
    Some((
        first.lines().next().unwrap_or_default().to_string(),
        prompts.len(),
    ))
}

/// Every session's log on this machine, newest first, with the folder beside it that holds
/// its saved outputs and subagent logs.
pub fn list_all() -> Vec<(PathBuf, SystemTime)> {
    list_all_in(&killi_home().join("sessions"))
}

pub(super) fn list_all_in(root: &Path) -> Vec<(PathBuf, SystemTime)> {
    let Ok(projects) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut logs: Vec<(PathBuf, SystemTime)> = projects
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .flat_map(|project| fs::read_dir(project.path()).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .filter_map(|path| {
            let modified = fs::metadata(&path).and_then(|m| m.modified()).ok()?;
            Some((path, modified))
        })
        .collect();
    logs.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    logs
}
