//! The append-only session log: every message exactly as it was sent, so a resumed session
//! sends the same bytes again and its cached prefix still hits.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
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
    /// The model and effort the session last ran on.
    pub choice: Choice,
}

/// A model alias and thinking effort, as `Thinking::label` names it.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub model: Option<String>,
    pub effort: Option<String>,
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
    let mut loaded = Loaded { messages: Vec::new(), choice: Choice::default() };
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        match entry["type"].as_str() {
            Some("session" | "model") => {
                loaded.choice = Choice {
                    model: entry["model"].as_str().map(String::from),
                    effort: entry["effort"].as_str().map(String::from),
                };
            }
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

fn choice_path() -> PathBuf {
    killi_home().join("last-model.json")
}

/// The model and effort last picked with /model or /effort, which new sessions start on.
/// Nothing when none was picked or the file can't be read.
pub fn last_choice() -> Choice {
    read_choice(&choice_path())
}

/// Remembers a pick from /model or /effort for the sessions started after it.
pub fn save_choice(choice: &Choice) -> Result<()> {
    write_choice(&choice_path(), choice)
}

fn read_choice(path: &Path) -> Choice {
    fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn write_choice(path: &Path, choice: &Choice) -> Result<()> {
    fs::write(path, serde_json::to_string(choice)?).with_context(|| format!("could not write {}", path.display()))
}

pub const COMPACTED_MARK: &str = "<system-reminder>This conversation was compacted";

/// What the user typed, oldest first: reminders killi added are left out, and a compaction's
/// summary gives back the requests it kept.
pub fn user_prompts(messages: &[Value]) -> Vec<String> {
    let mut prompts = Vec::new();
    for text in messages.iter().filter(|m| m["role"] == "user").filter_map(|m| m["content"].as_str()) {
        if text.starts_with(COMPACTED_MARK) {
            let mut rest = text;
            while let Some(start) = rest.find("<request>\n") {
                let body = &rest[start + 10..];
                let end = body.find("\n</request>").unwrap_or(body.len());
                prompts.push(body[..end].to_string());
                rest = &body[end..];
            }
        } else if let Some(prompt) = strip_reminders(text) {
            prompts.push(prompt.to_string());
        }
    }
    prompts
}

/// A user message without the reminders in front of it, or `None` if it is only reminders.
fn strip_reminders(text: &str) -> Option<&str> {
    let mut rest = text;
    while let Some(after) = rest.strip_prefix("<system-reminder>") {
        let end = after.find("</system-reminder>")?;
        rest = after[end + 18..].trim_start();
    }
    (!rest.is_empty()).then_some(rest)
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
            let prompts = user_prompts(&load(&path).ok()?.messages);
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
    fn prompts_come_back_without_reminders() {
        assert_eq!(strip_reminders("<system-reminder>a</system-reminder>\n\nfix it"), Some("fix it"));
        assert_eq!(strip_reminders("<system-reminder>Plan mode is on.</system-reminder>"), None);
        let compacted = format!("{COMPACTED_MARK} to save space.</system-reminder>\n\n<user-requests>\n<request>\none\n</request>\n<request>\ntwo\n</request>\n</user-requests>");
        let messages = vec![json!({"role": "user", "content": compacted}), json!({"role": "user", "content": "three"})];
        assert_eq!(user_prompts(&messages), ["one", "two", "three"]);
    }

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
        assert_eq!(loaded.choice, Choice { model: Some("m/x".into()), effort: None });
        let contents: Vec<_> = loaded.messages.iter().map(|m| m["content"].as_str().unwrap()).collect();
        assert_eq!(contents, ["sys", "summary", "next"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_switch_in_the_log_is_what_a_resume_runs_on() {
        let dir = std::env::temp_dir().join(format!("killi-switch-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1-1.jsonl");
        let lines = [
            json!({"type": "session", "model": "m/x", "effort": "high"}),
            json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
            json!({"type": "model", "model": "m/y", "effort": "medium"}),
            json!({"type": "model", "model": "m/y", "effort": "low"}),
        ];
        fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        assert_eq!(load(&path).unwrap().choice, Choice { model: Some("m/y".into()), effort: Some("low".into()) });
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_saved_choice_reads_back_and_a_missing_one_is_empty() {
        let path = std::env::temp_dir().join(format!("killi-choice-{}.json", std::process::id()));
        assert_eq!(read_choice(&path), Choice::default());
        let choice = Choice { model: Some("m/y".into()), effort: Some("low".into()) };
        write_choice(&path, &choice).unwrap();
        assert_eq!(read_choice(&path), choice);
        fs::write(&path, "not json").unwrap();
        assert_eq!(read_choice(&path), Choice::default());
        fs::remove_file(path).unwrap();
    }
}
