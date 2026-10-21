//! The append-only session log: every message exactly as it was sent, so a resumed session
//! sends the same bytes again and its cached prefix still hits.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::config::killi_home;

pub struct Session {
    pub id: String,
    pub log_path: PathBuf,
    /// Holds files saved for this session: full outputs of truncated commands, subagent logs.
    pub dir: PathBuf,
    file: File,
}

/// What a log holds, replayed.
pub struct Loaded {
    pub messages: Vec<Value>,
    pub model: Option<String>,
}

pub struct Listed {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub first_prompt: String,
    pub prompts: usize,
}

fn project_dir(cwd: &Path) -> PathBuf {
    killi_home().join("sessions").join(cwd.to_string_lossy().replace('/', "-"))
}

impl Session {
    pub fn create(cwd: &Path) -> Result<Self> {
        let root = project_dir(cwd);
        fs::create_dir_all(&root).with_context(|| format!("could not create {}", root.display()))?;
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        Self::open_new(root.join(format!("{secs}-{}.jsonl", std::process::id())))
    }

    /// A subagent's log, kept in its parent's session folder.
    pub fn child(&self, n: usize) -> Result<Self> {
        fs::create_dir_all(&self.dir)?;
        Self::open_new(self.dir.join(format!("agent-{n}.jsonl")))
    }

    fn open_new(log_path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("could not create {}", log_path.display()))?;
        Ok(Self::from_parts(log_path, file))
    }

    /// Reopens an existing log to append to it.
    pub fn open(log_path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .append(true)
            .open(log_path)
            .with_context(|| format!("could not open {}", log_path.display()))?;
        Ok(Self::from_parts(log_path.to_path_buf(), file))
    }

    fn from_parts(log_path: PathBuf, file: File) -> Self {
        let id = log_path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        Self { dir: log_path.with_extension(""), id, log_path, file }
    }

    pub fn record(&mut self, entry: &Value) -> Result<()> {
        writeln!(self.file, "{entry}").with_context(|| format!("could not write {}", self.log_path.display()))
    }
}

/// Replays a log: messages in order, restarting from the summary at each compaction.
pub fn load(path: &Path) -> Result<Loaded> {
    let text = fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut loaded = Loaded { messages: Vec::new(), model: None };
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        match entry["type"].as_str() {
            Some("session") => loaded.model = entry["model"].as_str().map(String::from),
            Some("message") => loaded.messages.push(entry["message"].clone()),
            Some("compaction") => {
                loaded.messages = entry["messages"].as_array().cloned().unwrap_or_default();
            }
            _ => {}
        }
    }
    if loaded.messages.first().is_none_or(|m| m["role"] != "system") {
        bail!("{} has no system prompt; it isn't a killi session", path.display());
    }
    Ok(loaded)
}

/// This folder's sessions, newest first.
pub fn list(cwd: &Path) -> Vec<Listed> {
    let Ok(entries) = fs::read_dir(project_dir(cwd)) else { return Vec::new() };
    let mut sessions: Vec<Listed> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .filter_map(|path| {
            let modified = fs::metadata(&path).and_then(|m| m.modified()).ok()?;
            let text = fs::read_to_string(&path).ok()?;
            let prompts: Vec<String> = text
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|e| e["type"] == "message" && e["message"]["role"] == "user")
                .filter_map(|e| e["message"]["content"].as_str().map(String::from))
                .filter(|text| !text.starts_with("<system-reminder>"))
                .collect();
            let first_prompt = prompts.first()?.lines().next().unwrap_or_default().to_string();
            Some(Listed { path, modified, first_prompt, prompts: prompts.len() })
        })
        .collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn load_restarts_at_a_compaction() {
        let dir = std::env::temp_dir().join(format!("killi-session-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1-1.jsonl");
        let lines = [
            json!({"type": "session", "model": "m/x"}),
            json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
            json!({"type": "message", "message": {"role": "user", "content": "first"}}),
            json!({"type": "compaction", "messages": [{"role": "system", "content": "sys"}, {"role": "user", "content": "summary"}]}),
            json!({"type": "message", "message": {"role": "user", "content": "next"}}),
        ];
        fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.model.as_deref(), Some("m/x"));
        let contents: Vec<_> = loaded.messages.iter().map(|m| m["content"].as_str().unwrap()).collect();
        assert_eq!(contents, ["sys", "summary", "next"]);
        fs::remove_dir_all(dir).unwrap();
    }
}
