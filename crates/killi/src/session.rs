//! The append-only session log: every message exactly as it was sent, so a resumed session
//! sends the same bytes again and its cached prefix still hits. One live session holds the
//! log's lock, so no two sessions ever write the same file.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
        create_private_dir(&root)?;
        Self::create_in(&root)
    }

    /// Names a log for this second and this process, taking the next name when the same second
    /// already saw one: `/new` twice in a row, or a pid handed out again.
    fn create_in(root: &Path) -> Result<Self> {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let stem = format!("{secs}-{}", std::process::id());
        for n in 1..=100 {
            let name = if n == 1 { stem.clone() } else { format!("{stem}-{n}") };
            match Self::open_new(root.join(format!("{name}.jsonl"))) {
                Ok(session) => return Ok(session),
                Err(err) if n < 100 && name_taken(&err) => continue,
                Err(err) => return Err(err),
            }
        }
        bail!("every log name for second {secs} is already taken")
    }

    /// A subagent's log, kept in its parent's session folder: the next free `agent-N.jsonl`
    /// from `n` on, so a resumed session whose children count restarted at 0 doesn't fail
    /// against the logs its earlier subagents left. Returns the log and the N it used.
    pub fn child(&self, n: usize) -> Result<(Self, usize)> {
        create_private_dir(&self.dir)?;
        for used in n..n + 100 {
            match Self::open_new(self.dir.join(format!("agent-{used}.jsonl"))) {
                Ok(session) => return Ok((session, used)),
                Err(err) if used + 1 < n + 100 && name_taken(&err) => continue,
                Err(err) => return Err(err),
            }
        }
        bail!("every subagent log name from agent-{n} is already taken")
    }

    fn open_new(log_path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)
            .with_context(|| format!("could not create {}", log_path.display()))?;
        Self::holding(log_path, file)
    }

    /// Reopens an existing log to append to it. A file that doesn't end with a newline — its
    /// last line torn by a crash — gets one first, so the next record isn't glued onto it.
    pub fn open(log_path: &Path) -> Result<Self> {
        let mut file = OpenOptions::new()
            .append(true)
            .read(true)
            .open(log_path)
            .with_context(|| format!("could not open {}", log_path.display()))?;
        lock(&file, log_path)?;
        if file.metadata()?.len() > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0u8];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n").with_context(|| format!("could not write {}", log_path.display()))?;
            }
        }
        Ok(Self::from_parts(log_path.to_path_buf(), file))
    }

    /// Takes the log's exclusive lock and holds it until the `Session` is dropped or the
    /// process ends: two sessions writing one log interleave their records, and the log can
    /// never be resumed again.
    fn holding(log_path: PathBuf, file: File) -> Result<Self> {
        lock(&file, &log_path)?;
        Ok(Self::from_parts(log_path, file))
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
    /// The line is built first and written once: formatting straight into the file costs a
    /// `write` per JSON fragment.
    pub fn record(&mut self, entry: &Value) -> Result<()> {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
        let line = stamped(entry, u64::try_from(ts).unwrap_or(u64::MAX));
        self.file.write_all(line.as_bytes()).with_context(|| format!("could not write {}", self.log_path.display()))
    }
}

/// Whether `create`'s name was already taken, so it can try the next one.
fn name_taken(err: &anyhow::Error) -> bool {
    err.root_cause().downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists)
}

/// Takes the log's exclusive lock for `file`, held until the `Session` is dropped.
fn lock(file: &File, log_path: &Path) -> Result<()> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => bail!(
            "session is in use: {} is held by a session that is still running; continue it there, or start /new",
            log_path.display()
        ),
        Err(err) => Err(err).with_context(|| format!("could not lock {}", log_path.display())),
    }
}

/// A folder for session logs, private to this user: logs keep pasted secrets and tool output.
fn create_private_dir(root: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)
        .with_context(|| format!("could not create {}", root.display()))
}

/// Whether a session that is still running holds this log: each session locks its log for its
/// life, so a lock that can't be taken belongs to one.
pub fn is_locked(path: &Path) -> bool {
    File::open(path).is_ok_and(|file| matches!(file.try_lock(), Err(TryLockError::WouldBlock)))
}

/// `entry` as one log line ending in a newline, with `ts` added to an object.
fn stamped(entry: &Value, ts: u64) -> String {
    let mut line = entry.to_string();
    if entry.as_object().is_some() {
        line.pop();
        if line.len() > 1 {
            line.push(',');
        }
        line.push_str(&format!("\"ts\":{ts}}}"));
    }
    line.push('\n');
    line
}

/// What `load_reported` found besides the conversation.
#[allow(dead_code)]
pub struct LoadReport {
    pub loaded: Loaded,
    /// Non-empty log lines skipped: undecodable UTF-8 or torn JSON.
    pub skipped: usize,
    /// Placeholder tool results inserted for mid-history calls the log never answered.
    pub repaired: usize,
}

/// Replays a log: messages in order, restarting from the summary at each compaction.
/// Resume loads once via `load_reported`; only the tests call this.
#[allow(dead_code)]
pub fn load(path: &Path) -> Result<Loaded> {
    Ok(load_reported(path)?.loaded)
}

/// `load`, also reporting how many lines were skipped and how many dangling tool calls were
/// repaired. Callers that keep showing `Loaded` are unaffected.
pub fn load_reported(path: &Path) -> Result<LoadReport> {
    // Decoded per line, so one bad-UTF-8 line is skipped instead of losing the whole log.
    // Splitting bytes on `\n` is safe: it never appears inside a multibyte UTF-8 sequence.
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut skipped = 0;
    let mut loaded = Loaded { messages: Vec::new(), tools: HashMap::new(), context: 0, choice: Choice::default() };
    for chunk in bytes.split(|b| *b == b'\n') {
        if chunk.is_empty() {
            continue;
        }
        let Ok(line) = std::str::from_utf8(chunk) else {
            skipped += 1;
            continue;
        };
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            skipped += 1;
            continue;
        };
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
    let repaired = repair_dangling_calls(&mut loaded.messages);
    if loaded.messages.first().is_none_or(|m| m["role"] != "system") {
        bail!("{} has no system prompt; it isn't a killi session", path.display());
    }
    Ok(LoadReport { loaded, skipped, repaired })
}

/// A placeholder tool result, so a resumed session sends a history the provider accepts.
const DANGLING_TOOL_RESULT: &str = "(no result: the session stopped before this tool call returned)";

/// Gives every mid-history assistant tool call the log never answered a placeholder result
/// right after its calls, and returns how many were inserted. A call at the very end is left
/// alone: resuming runs it. This is a load-time repair only; a resume that wants it kept in
/// the log records the repaired messages as a compaction-kind entry, the way the media
/// recovery in `agent.rs` records its rewrite.
fn repair_dangling_calls(messages: &mut Vec<Value>) -> usize {
    let mut answered: HashSet<&str> = messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| m["tool_call_id"].as_str())
        .collect();
    let mut pending: Vec<(usize, Vec<Value>)> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if message["role"] != "assistant" || index + 1 >= messages.len() {
            continue;
        }
        let mut placeholders = Vec::new();
        for call in message["tool_calls"].as_array().into_iter().flatten() {
            if let Some(id) = call["id"].as_str()
                && !answered.contains(id)
            {
                answered.insert(id);
                placeholders.push(json!({"role": "tool", "tool_call_id": id, "content": DANGLING_TOOL_RESULT}));
            }
        }
        if !placeholders.is_empty() {
            pending.push((index, placeholders));
        }
    }
    let mut repaired = 0;
    for (index, placeholders) in pending.into_iter().rev() {
        repaired += placeholders.len();
        messages.splice(index + 1..index + 1, placeholders);
    }
    repaired
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
    crate::fs_atomic::write(path, serde_json::to_string(choice)?.as_bytes(), None)
        .with_context(|| format!("could not write {}", path.display()))
}

pub const COMPACTED_MARK: &str = "<system-reminder>This conversation was compacted";

/// What the user typed, oldest first: reminders killi added are left out, and a compaction's
/// summary gives back the requests it kept.
pub fn user_prompts(messages: &[Value]) -> Vec<String> {
    messages.iter().filter(|m| m["role"] == "user").filter_map(|m| m["content"].as_str()).flat_map(text_prompts).collect()
}

/// The prompts in one user message: a compaction's summary gives back the requests it
/// kept, anything else is the typed text without its reminders.
fn text_prompts(text: &str) -> Vec<String> {
    if text.starts_with(COMPACTED_MARK) {
        let mut prompts = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("<request>\n") {
            let body = &rest[start + 10..];
            let end = body.find("\n</request>").unwrap_or(body.len());
            prompts.extend(typed(&body[..end]));
            rest = &body[end..];
        }
        prompts
    } else {
        typed(text).into_iter().collect()
    }
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
                } else if let Some(prompt) = typed(text) {
                    shown.push(Replayed::Prompt(prompt));
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

/// What the user typed for a message killi sent as `text`: without the reminders before it or
/// the files it named with `@` after it, which a compaction may have cut before their end, and
/// a skill run as the `/name request` that started it. `None` if it is only reminders.
fn typed(text: &str) -> Option<String> {
    let prompt = strip_reminders(text)?;
    let mut prompt = crate::skills::typed(prompt).unwrap_or_else(|| prompt.to_string());
    if let Some(at) = prompt.find("\n\n<system-reminder>") {
        prompt.truncate(at);
    }
    Some(prompt)
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

/// This folder's sessions, newest first. The list streams each log and parses only the
/// lines that may hold a user message, so tool results and usage lines are never touched.
pub fn list(cwd: &Path) -> Vec<Listed> {
    let Ok(entries) = fs::read_dir(project_dir(cwd)) else { return Vec::new() };
    let mut sessions: Vec<Listed> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .filter_map(|path| {
            let modified = fs::metadata(&path).and_then(|m| m.modified()).ok()?;
            let (first_prompt, prompts) = scan_prompts(&path)?;
            Some(Listed { path, modified, first_prompt, prompts })
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
fn scan_prompts(path: &Path) -> Option<(String, usize)> {
    let file = File::open(path).ok()?;
    let mut prompts: Vec<String> = Vec::new();
    let mut anchored: Option<bool> = None;
    for line in BufReader::new(file).split(b'\n') {
        let line = line.ok()?;
        if anchored.is_some() && !contains(&line, b"\"user\"") && !contains(&line, b"\"compaction\"") {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&line) else { continue };
        let Ok(entry) = serde_json::from_str::<Value>(text) else { continue };
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
                anchored = Some(entry["messages"].as_array().into_iter().flatten().next().is_some_and(|m| m["role"] == "system"));
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
    Some((first.lines().next().unwrap_or_default().to_string(), prompts.len()))
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
    let live = live_logs();
    let old: Vec<PathBuf> = list_all_in(root).into_iter().map(|(path, _)| path).filter(|path| deletable(path, current, &live)).skip(keep).collect();
    for log in old {
        // Checked again per log: a session may have resumed this one since the list above
        // was taken, while earlier (larger) side folders were being removed.
        if !deletable(&log, current, &live_logs()) {
            continue;
        }
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

/// Whether `prune` may delete this log: not the session running now, not one that is still
/// running — each live session locks its log and lists it in `~/.killi/live` — and not one a
/// live session lists.
fn deletable(path: &Path, current: &Path, live: &HashSet<PathBuf>) -> bool {
    path != current && !live.contains(path) && !is_locked(path)
}

/// The logs live sessions list as theirs in `~/.killi/live` (`live::Entry::log`).
fn live_logs() -> HashSet<PathBuf> {
    crate::live::list().into_iter().map(|entry| entry.log).collect()
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
    fn a_skill_run_comes_back_as_the_command_that_started_it() {
        let skill = "Use the pr-review skill for this.\n\n<skill name=\"pr-review\" folder=\"/s/pr-review\">\n---\nname: pr-review\n---\nReview it.\n</skill>";
        let files = "<system-reminder>The files the user named with @, as they are now:\n\n<file path=\"a.rs\">\nfn a() {}\n</file></system-reminder>";
        let messages = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": format!("<system-reminder>Plan mode is on.</system-reminder>\n\n{skill}\n\nreview 42")}),
            json!({"role": "user", "content": format!("{skill}\n\nFollow the skill.")}),
            json!({"role": "user", "content": format!("{skill}\n\nlook at @a.rs\n\n{files}")}),
            json!({"role": "user", "content": format!("explain @a.rs\n\n{files}")}),
        ];
        let shown = ["/pr-review review 42", "/pr-review", "/pr-review look at @a.rs", "explain @a.rs"];
        assert_eq!(user_prompts(&messages), shown);
        assert_eq!(replay(&loaded(messages), Path::new("/w"), 8), shown.map(|p| Replayed::Prompt(p.into())));
        // Requests a compaction kept before it kept them short: cut inside the files, or the skill.
        let cut = format!("{COMPACTED_MARK} to save space.</system-reminder>\n\n<user-requests>\n<request>\nexplain @a.rs\n\n{}\n[… 900 characters cut]\n</request>\n<request>\n{}\n[… 900 characters cut]\n</request>\n</user-requests>", &files[..60], &skill[..60]);
        assert_eq!(user_prompts(&[json!({"role": "user", "content": cut})]), ["explain @a.rs", "/pr-review"]);
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
    fn scan_prompts_matches_a_full_load_without_parsing_tool_results() {
        let dir = std::env::temp_dir().join(format!("killi-scan-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1-1.jsonl");
        let big = "x".repeat(200_000);
        let decoy = r#"{"role":"user","content":"not a message"}"#;
        let lines = [
            json!({"type": "session", "model": "user"}),
            json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
            json!({"type": "message", "message": {"role": "user", "content": "<system-reminder>note</system-reminder>\n\nfix it"}}),
            json!({"type": "message", "message": {"role": "assistant", "content": "on it", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{}"}}]}}),
            json!({"type": "message", "message": {"role": "tool", "tool_call_id": "c1", "content": format!("{big} {decoy} compaction")}}),
            json!({"type": "usage", "prompt": 900}),
            json!({"type": "message", "message": {"role": "user", "content": "<system-reminder>only a reminder</system-reminder>"}}),
            json!({"type": "compaction", "messages": [{"role": "system", "content": "sys"}, {"role": "user", "content": "before"}]}),
            json!({"type": "message", "message": {"role": "user", "content": "after\nmore"}}),
        ];
        fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        let loaded = load(&path).unwrap();
        let prompts = user_prompts(&loaded.messages);
        assert_eq!(prompts, ["before", "after\nmore"]);
        assert_eq!(scan_prompts(&path), Some(("before".to_string(), 2)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scan_prompts_skips_logs_a_load_would_refuse_or_find_empty() {
        let dir = std::env::temp_dir().join(format!("killi-scan-skip-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, lines: Vec<Value>| {
            let path = dir.join(name);
            fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
            path
        };
        let no_system =
            write("a.jsonl", vec![json!({"type": "message", "message": {"role": "user", "content": "hi"}})]);
        assert!(load(&no_system).is_err());
        assert_eq!(scan_prompts(&no_system), None);
        let assistant_first = write(
            "b.jsonl",
            vec![
                json!({"type": "message", "message": {"role": "assistant", "content": "hi"}}),
                json!({"type": "message", "message": {"role": "user", "content": "yo"}}),
            ],
        );
        assert!(load(&assistant_first).is_err());
        assert_eq!(scan_prompts(&assistant_first), None);
        let reminders_only = write(
            "c.jsonl",
            vec![
                json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
                json!({"type": "message", "message": {"role": "user", "content": "<system-reminder>only a reminder</system-reminder>"}}),
            ],
        );
        assert!(load(&reminders_only).is_ok());
        assert_eq!(scan_prompts(&reminders_only), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn child_logs_skip_existing_names() {
        let root = std::env::temp_dir().join(format!("killi-child-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let parent = Session::open_new(root.join("1-1.jsonl")).unwrap();
        fs::create_dir_all(&parent.dir).unwrap();
        fs::write(parent.dir.join("agent-1.jsonl"), "taken").unwrap();
        let (first, used) = parent.child(1).unwrap();
        assert_eq!(used, 2);
        assert!(first.log_path.ends_with("agent-2.jsonl"));
        let (_, reused) = parent.child(1).unwrap();
        assert_eq!(reused, 3, "agent-2 now exists too");
        drop(first);
        fs::remove_dir_all(root).unwrap();
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
    fn a_log_open_in_one_session_cannot_be_opened_again() {
        let dir = std::env::temp_dir().join(format!("killi-lock-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1-1.jsonl");
        let mut one = Session::open_new(path.clone()).unwrap();
        one.record(&json!({"type": "message", "message": {"role": "system", "content": "sys"}})).unwrap();

        assert!(is_locked(&path), "the log is locked while its session runs");
        match Session::open(&path) {
            Ok(_) => panic!("a log held by one session opened again"),
            Err(e) => assert!(e.to_string().starts_with("session is in use"), "{e:#}"),
        }
        drop(one);

        assert!(!is_locked(&path), "the lock goes when the session ends");
        assert!(Session::open(&path).is_ok());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_second_session_in_the_same_second_takes_the_next_name() {
        let root = std::env::temp_dir().join(format!("killi-names-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let one = Session::create_in(&root).unwrap();
        let two = Session::create_in(&root).unwrap();
        assert_ne!(one.log_path, two.log_path, "the same second and pid don't share a log");

        let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let taken = root.join(format!("{secs}-{}.jsonl", std::process::id()));
        fs::write(&taken, "x").unwrap();
        let three = Session::create_in(&root).unwrap();
        assert_ne!(three.log_path, taken, "a name already used is skipped");
        fs::remove_dir_all(&root).unwrap();
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
    fn an_entry_is_one_stamped_line() {
        let entry = json!({"type": "message", "message": {"role": "user", "content": "a \"quoted\"\nline"}});
        let line = stamped(&entry, 7);
        assert!(line.ends_with("}\n") && line.matches('\n').count() == 1, "{line}");
        let back: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(back["ts"], 7);
        assert_eq!(back["message"], entry["message"]);
        assert_eq!(serde_json::from_str::<Value>(&stamped(&json!({}), 3)).unwrap(), json!({"ts": 3}));
        assert_eq!(stamped(&json!("not an object"), 3), "\"not an object\"\n");
    }

    #[test]
    fn recorded_entries_load_back() {
        let dir = std::env::temp_dir().join(format!("killi-record-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut session = Session::open_new(dir.join("1-1.jsonl")).unwrap();
        session.record(&json!({"type": "message", "message": {"role": "system", "content": "sys"}})).unwrap();
        session.record(&json!({"type": "message", "message": {"role": "user", "content": "hi\nthere"}})).unwrap();
        let loaded = load(&session.log_path).unwrap();
        let contents: Vec<_> = loaded.messages.iter().map(|m| m["content"].as_str().unwrap()).collect();
        assert_eq!(contents, ["sys", "hi\nthere"]);
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

    #[test]
    fn prune_skips_a_locked_log_and_a_live_listed_one() {
        let root = std::env::temp_dir().join(format!("killi-prune-live-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let project = root.join("proj");
        fs::create_dir_all(&project).unwrap();
        let logs = [project.join("1-1.jsonl"), project.join("2-2.jsonl"), project.join("3-3.jsonl")];
        for (i, log) in logs.iter().enumerate() {
            fs::write(log, "x".repeat(50)).unwrap();
            let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i as u64 * 60);
            File::options().write(true).open(log).unwrap().set_modified(when).unwrap();
        }
        let held = Session::open(&logs[0]).unwrap();
        let live: HashSet<PathBuf> = [logs[2].clone()].into_iter().collect();
        assert!(!deletable(&logs[0], Path::new("nowhere"), &live), "a locked log stays");
        assert!(!deletable(&logs[2], Path::new("nowhere"), &live), "a live-listed log stays");
        assert!(deletable(&logs[1], Path::new("nowhere"), &live));
        assert!(!deletable(&logs[1], &logs[1], &HashSet::new()), "the current session stays");
        let pruned = prune_in(&root, 0, Path::new("nowhere")).unwrap();
        assert!(logs[0].exists(), "a log locked by a running session is skipped");
        assert!(!logs[1].exists() && !logs[2].exists());
        assert_eq!(pruned.sessions, 2);
        drop(held);
        assert_eq!(prune_in(&root, 0, Path::new("nowhere")).unwrap().sessions, 1);
        assert!(!logs[0].exists(), "an unlocked log prunes once its session ends");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opening_a_torn_log_starts_the_next_record_on_its_own_line() {
        let dir = std::env::temp_dir().join(format!("killi-torn-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1-1.jsonl");
        fs::write(&path, format!("{}\n{}", json!({"type": "message", "message": {"role": "system", "content": "sys"}}), "{\"type\":\"messa")).unwrap();
        let mut session = Session::open(&path).unwrap();
        session.record(&json!({"type": "message", "message": {"role": "user", "content": "next"}})).unwrap();
        drop(session);
        let report = load_reported(&path).unwrap();
        let contents: Vec<_> = report.loaded.messages.iter().map(|m| m["content"].as_str().unwrap()).collect();
        assert_eq!((contents, report.skipped), (vec!["sys", "next"], 1));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_skips_an_undecodable_line_and_reports_how_many() {
        let dir = std::env::temp_dir().join(format!("killi-badline-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1-1.jsonl");
        let mut bytes = format!("{}\n", json!({"type": "message", "message": {"role": "system", "content": "sys"}})).into_bytes();
        bytes.extend_from_slice(b"\xff\xfe not utf-8\n");
        bytes.extend_from_slice(format!("{}\n", json!({"type": "message", "message": {"role": "user", "content": "hi"}})).as_bytes());
        fs::write(&path, &bytes).unwrap();
        let report = load_reported(&path).unwrap();
        let contents: Vec<_> = report.loaded.messages.iter().map(|m| m["content"].as_str().unwrap()).collect();
        assert_eq!((contents, report.skipped), (vec!["sys", "hi"], 1));
        assert_eq!(load(&path).unwrap().messages.len(), 2, "load keeps its shape for existing callers");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_repairs_a_mid_history_tool_call_that_never_returned() {
        let dir = std::env::temp_dir().join(format!("killi-repair-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1-1.jsonl");
        let call = |id: &str| json!({"id": id, "type": "function", "function": {"name": "read", "arguments": "{}"}});
        let lines = [
            json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
            json!({"type": "message", "message": {"role": "user", "content": "do it"}}),
            json!({"type": "message", "message": {"role": "assistant", "content": "on it", "tool_calls": [call("c1")]}}),
            json!({"type": "message", "message": {"role": "user", "content": "keep going"}}),
            json!({"type": "message", "message": {"role": "assistant", "content": "later", "tool_calls": [call("c2")]}}),
        ];
        fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        let report = load_reported(&path).unwrap();
        assert_eq!(report.repaired, 1, "only the mid-history call is repaired, not the trailing pending one");
        let messages = &report.loaded.messages;
        assert_eq!(messages.len(), 6);
        assert_eq!(messages[3]["tool_call_id"], "c1");
        assert_eq!(messages[3]["role"], "tool");
        assert!(messages[3]["content"].as_str().is_some_and(|c| !c.is_empty()));
        assert_eq!(messages[5]["tool_calls"][0]["id"], "c2");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_logs_and_folders_are_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("killi-perms-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let session = Session::create_in(&root).unwrap();
        assert_eq!(session.log_path.parent(), Some(root.as_path()));
        assert_eq!(session.log_path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        let (child, _) = session.child(0).unwrap();
        assert_eq!(session.dir.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(child.log_path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        drop(child);
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }
}
