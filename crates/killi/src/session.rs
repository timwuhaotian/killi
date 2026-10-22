//! The append-only session log: every message exactly as it was sent, so a resumed session
//! sends the same bytes again and its cached prefix still hits.

use std::collections::HashMap;
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
    /// How each tool call ended, by call id, for logs that kept it.
    pub tools: HashMap<String, ToolEnd>,
    /// Input tokens of the last request: how full the context was when the log stopped.
    pub context: u64,
    /// The model and effort the session last ran on.
    pub choice: Choice,
}

/// How a tool call ended, as the log kept it beside the result.
#[derive(Debug, Default, PartialEq)]
pub struct ToolEnd {
    pub is_error: bool,
    pub brief: String,
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

pub(crate) fn project_dir(cwd: &Path) -> PathBuf {
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

    /// Deletes the log and its folder, for a session in which nothing happened. A folder that
    /// holds saved files is left alone.
    pub fn discard(&self) {
        let _ = fs::remove_file(&self.log_path);
        let _ = fs::remove_dir(&self.dir);
    }

    /// Appends an entry, stamped with the time in milliseconds (`ts`), so a log shows when
    /// things happened. The stamp sits beside the entry's data and is never sent to a model.
    pub fn record(&mut self, entry: &Value) -> Result<()> {
        let mut entry = entry.clone();
        if let Some(fields) = entry.as_object_mut() {
            let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
            fields.insert("ts".into(), Value::from(u64::try_from(ts).unwrap_or(u64::MAX)));
        }
        writeln!(self.file, "{entry}").with_context(|| format!("could not write {}", self.log_path.display()))
    }
}

/// Replays a log: messages in order, restarting from the summary at each compaction.
pub fn load(path: &Path) -> Result<Loaded> {
    let text = fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut loaded = Loaded { messages: Vec::new(), tools: HashMap::new(), context: 0, choice: Choice::default() };
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        match entry["type"].as_str() {
            Some("session" | "model") => {
                loaded.choice = Choice {
                    model: entry["model"].as_str().map(String::from),
                    effort: entry["effort"].as_str().map(String::from),
                };
            }
            Some("message") => {
                if let (Some(id), Some(tool)) = (entry["message"]["tool_call_id"].as_str(), entry.get("tool")) {
                    let end = ToolEnd {
                        is_error: tool["is_error"].as_bool().unwrap_or(false),
                        brief: tool["brief"].as_str().unwrap_or_default().to_string(),
                    };
                    loaded.tools.insert(id.to_string(), end);
                }
                loaded.messages.push(entry["message"].clone());
            }
            Some("usage") => loaded.context = entry["prompt"].as_u64().unwrap_or(0),
            Some("compaction") => {
                loaded.messages = entry["messages"].as_array().cloned().unwrap_or_default();
                loaded.context = 0;
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

/// One thing a resumed session shows again, in the order it happened.
#[derive(Debug, PartialEq)]
pub enum Replayed {
    Note(String),
    Prompt(String),
    Text(String),
    Tool { summary: String, output: String, is_error: bool, brief: String },
}

/// The conversation as the terminal showed it, for a resumed session: prompts, the model's
/// text and each tool call with its result. Only the last `keep` prompts are shown, with a
/// note for the earlier ones. Logs from before tool results were kept with how they ended
/// get a best guess from the result's text.
pub fn replay(loaded: &Loaded, cwd: &Path, keep: usize) -> Vec<Replayed> {
    let results: HashMap<&str, &str> = loaded
        .messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| Some((m["tool_call_id"].as_str()?, m["content"].as_str()?)))
        .collect();
    let mut shown = Vec::new();
    for message in &loaded.messages {
        match message["role"].as_str() {
            Some("user") => {
                let Some(text) = message["content"].as_str() else { continue };
                if text.starts_with(COMPACTED_MARK) {
                    shown.push(Replayed::Note("The conversation before this was compacted into a summary.".into()));
                    shown.extend(user_prompts(std::slice::from_ref(message)).into_iter().map(Replayed::Prompt));
                } else if let Some(prompt) = strip_reminders(text) {
                    shown.push(Replayed::Prompt(prompt.to_string()));
                }
            }
            Some("assistant") => {
                if let Some(text) = message["content"].as_str().filter(|t| !t.trim().is_empty()) {
                    shown.push(Replayed::Text(text.to_string()));
                }
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    let name = call["function"]["name"].as_str().unwrap_or_default();
                    let args = serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap_or_default()).unwrap_or_default();
                    let id = call["id"].as_str().unwrap_or_default();
                    let output = results.get(id).copied().unwrap_or("(no result)").to_string();
                    let (is_error, brief) = match loaded.tools.get(id) {
                        Some(end) => (end.is_error, end.brief.clone()),
                        None => guess_end(&output),
                    };
                    shown.push(Replayed::Tool { summary: crate::tools::summary(name, &args, cwd), output, is_error, brief });
                }
            }
            _ => {}
        }
    }
    let prompts = shown.iter().filter(|r| matches!(r, Replayed::Prompt(_))).count();
    if prompts > keep {
        let first = shown.iter().enumerate().filter(|(_, r)| matches!(r, Replayed::Prompt(_))).nth(prompts - keep).map_or(0, |(i, _)| i);
        shown.drain(..first);
        let earlier = prompts - keep;
        shown.insert(0, Replayed::Note(format!("{earlier} earlier prompt{} not shown.", if earlier == 1 { "" } else { "s" })));
    }
    shown
}

/// How a result ended, for logs that didn't keep it: a command that failed says so at the end.
fn guess_end(output: &str) -> (bool, String) {
    let failed = output.lines().last().is_some_and(|l| l.starts_with("[Command ")) || output.starts_with("Interrupted by the user");
    let lines = output.lines().count();
    match (failed, lines) {
        _ if output.starts_with("(no output") => (false, "no output".into()),
        (true, _) => (true, output.lines().last().unwrap_or_default().trim_matches(['[', ']']).to_string()),
        (false, 0 | 1) => (false, String::new()),
        (false, n) => (false, format!("{n} lines")),
    }
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

/// Every session's log on this machine, newest first, with the folder beside it that holds
/// its saved outputs and subagent logs.
pub fn list_all() -> Vec<(PathBuf, SystemTime)> {
    list_all_in(&killi_home().join("sessions"))
}

fn list_all_in(root: &Path) -> Vec<(PathBuf, SystemTime)> {
    let Ok(projects) = fs::read_dir(root) else { return Vec::new() };
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

/// What `prune` removed: how many sessions, and how many bytes they took.
#[derive(Debug, Default, PartialEq)]
pub struct Pruned {
    pub sessions: usize,
    pub bytes: u64,
}

/// Deletes every session but the `keep` newest and `current`, each with its side folder, and
/// the project folders left empty.
pub fn prune(keep: usize, current: &Path) -> Result<Pruned> {
    prune_in(&killi_home().join("sessions"), keep, current)
}

fn prune_in(root: &Path, keep: usize, current: &Path) -> Result<Pruned> {
    let mut pruned = Pruned::default();
    let old: Vec<PathBuf> = list_all_in(root).into_iter().map(|(path, _)| path).filter(|path| path != current).skip(keep).collect();
    for log in old {
        let dir = log.with_extension("");
        if dir.is_dir() {
            pruned.bytes += dir_size(&dir);
            fs::remove_dir_all(&dir).with_context(|| format!("could not remove {}", dir.display()))?;
        }
        pruned.bytes += fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
        fs::remove_file(&log).with_context(|| format!("could not remove {}", log.display()))?;
        pruned.sessions += 1;
        if let Some(project) = log.parent()
            && fs::read_dir(project).is_ok_and(|mut entries| entries.next().is_none())
        {
            let _ = fs::remove_dir(project);
        }
    }
    Ok(pruned)
}

fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() { dir_size(&path) } else { fs::metadata(&path).map(|m| m.len()).unwrap_or(0) }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    #[test]
    fn prune_keeps_the_newest_and_the_current_session_and_removes_the_rest_with_their_folders() {
        let root = std::env::temp_dir().join(format!("killi-prune-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (a, b) = (root.join("-p-one"), root.join("-p-two"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let logs = [a.join("1-1.jsonl"), a.join("2-2.jsonl"), b.join("3-3.jsonl"), b.join("4-4.jsonl")];
        for (i, log) in logs.iter().enumerate() {
            fs::write(log, "x".repeat(100)).unwrap();
            let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i as u64 * 60);
            File::options().write(true).open(log).unwrap().set_modified(when).unwrap();
        }
        fs::create_dir_all(a.join("1-1")).unwrap();
        fs::write(a.join("1-1/agent-1.jsonl"), "y".repeat(50)).unwrap();
        let newest: Vec<PathBuf> = list_all_in(&root).into_iter().map(|(p, _)| p).collect();
        assert_eq!(newest, [logs[3].clone(), logs[2].clone(), logs[1].clone(), logs[0].clone()]);

        let pruned = prune_in(&root, 1, &logs[1]).unwrap();
        assert_eq!(pruned, Pruned { sessions: 2, bytes: 250 });
        assert!(logs[3].exists() && logs[1].exists(), "the newest and the current stay");
        assert!(!logs[0].exists() && !logs[2].exists() && !a.join("1-1").exists());
        assert!(a.exists() && b.exists());
        assert_eq!(prune_in(&root, 0, &logs[1]).unwrap().sessions, 1);
        assert!(!b.exists(), "a project folder left empty goes too");
        fs::remove_dir_all(root).unwrap();
    }

    use super::*;
    use serde_json::json;

    fn loaded(messages: Vec<Value>) -> Loaded {
        Loaded { messages, tools: HashMap::new(), context: 0, choice: Choice::default() }
    }

    #[test]
    fn a_resumed_session_shows_prompts_text_and_tool_calls_in_order() {
        let mut log = loaded(vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "<system-reminder>note</system-reminder>\n\nfix it"}),
            json!({"role": "assistant", "content": "Looking.", "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"cd /w && ls\"}"}},
                {"id": "c2", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"/w/a.rs\"}"}},
            ]}),
            json!({"role": "tool", "tool_call_id": "c1", "content": "a\nb\n[Command exited with code 2]"}),
            json!({"role": "tool", "tool_call_id": "c2", "content": "1\tfn main() {}"}),
            json!({"role": "user", "content": "<system-reminder>only a reminder</system-reminder>"}),
            json!({"role": "assistant", "content": "Done."}),
        ]);
        log.tools.insert("c2".into(), ToolEnd { is_error: false, brief: "1 line".into() });
        let shown = replay(&log, Path::new("/w"), 10);
        assert_eq!(
            shown,
            [
                Replayed::Prompt("fix it".into()),
                Replayed::Text("Looking.".into()),
                Replayed::Tool { summary: "bash ls".into(), output: "a\nb\n[Command exited with code 2]".into(), is_error: true, brief: "Command exited with code 2".into() },
                Replayed::Tool { summary: "read a.rs".into(), output: "1\tfn main() {}".into(), is_error: false, brief: "1 line".into() },
                Replayed::Text("Done.".into()),
            ]
        );
    }

    #[test]
    fn a_replay_keeps_the_last_prompts_and_counts_the_rest() {
        let mut messages = vec![json!({"role": "system", "content": "sys"})];
        for n in 1..=4 {
            messages.push(json!({"role": "user", "content": format!("prompt {n}")}));
            messages.push(json!({"role": "assistant", "content": format!("answer {n}")}));
        }
        let shown = replay(&loaded(messages), Path::new("/w"), 2);
        assert_eq!(shown[0], Replayed::Note("2 earlier prompts not shown.".into()));
        assert_eq!(shown[1], Replayed::Prompt("prompt 3".into()));
        assert_eq!(shown.len(), 5);
    }

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
    fn discarding_removes_the_log_and_only_an_empty_folder() {
        let root = std::env::temp_dir().join(format!("killi-discard-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let empty = Session::open_new(root.join("1-1.jsonl")).unwrap();
        empty.discard();
        assert!(!root.join("1-1.jsonl").exists());
        let kept = Session::open_new(root.join("2-2.jsonl")).unwrap();
        fs::create_dir_all(&kept.dir).unwrap();
        fs::write(kept.dir.join("result-1.txt"), "saved output").unwrap();
        kept.discard();
        assert!(!root.join("2-2.jsonl").exists());
        assert!(kept.dir.join("result-1.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn load_keeps_how_tools_ended_and_how_full_the_context_was() {
        let dir = std::env::temp_dir().join(format!("killi-ends-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1-1.jsonl");
        let lines = [
            json!({"type": "session", "model": "m/x", "ts": 1}),
            json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
            json!({"type": "usage", "prompt": 900, "cached": 0, "completion": 5}),
            json!({"type": "message", "message": {"role": "tool", "tool_call_id": "c1", "content": "boom"}, "tool": {"is_error": true, "brief": "exit 1", "ms": 40}}),
            json!({"type": "usage", "prompt": 1200, "cached": 900, "completion": 7}),
        ];
        fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.context, 1200);
        assert_eq!(loaded.tools["c1"], ToolEnd { is_error: true, brief: "exit 1".into() });
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_silent_result_from_an_old_log_reads_as_no_output() {
        assert_eq!(guess_end("(no output)"), (false, "no output".into()));
        assert_eq!(guess_end("a\nb\n[Command exited with code 2]"), (true, "Command exited with code 2".into()));
        assert_eq!(guess_end("one line"), (false, String::new()));
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
