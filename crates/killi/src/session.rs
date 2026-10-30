//! The append-only session log: every message exactly as it was sent, so a resumed session
//! sends the same bytes again and its cached prefix still hits. One live session holds the
//! log's lock, so no two sessions ever write the same file.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::config::killi_home;

mod choice;
mod list;
mod load;
mod prune;
mod replay;

pub use choice::{last_choice, save_choice};
pub use list::{Listed, list, list_all};
pub use load::{Choice, LoadReport, Loaded, load_reported};
pub use prune::prune;
pub use replay::{COMPACTED_MARK, Replayed, replay, user_prompts};

use list::list_all_in;
use replay::text_prompts;

#[cfg(test)]
use choice::{read_choice, write_choice};
#[cfg(test)]
use list::scan_prompts;
#[cfg(test)]
use prune::{deletable, prune_in};
#[cfg(test)]
use replay::{guess_end, strip_reminders};

pub struct Session {
    pub id: String,
    pub log_path: PathBuf,
    /// Holds files saved for this session: full outputs of truncated commands, subagent logs.
    pub dir: PathBuf,
    file: File,
}

pub(crate) fn project_dir(cwd: &Path) -> PathBuf {
    killi_home()
        .join("sessions")
        .join(cwd.to_string_lossy().replace('/', "-"))
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
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let stem = format!("{secs}-{}", std::process::id());
        for n in 1..=100 {
            let name = if n == 1 {
                stem.clone()
            } else {
                format!("{stem}-{n}")
            };
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
                file.write_all(b"\n")
                    .with_context(|| format!("could not write {}", log_path.display()))?;
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
        let id = log_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        Self {
            dir: log_path.with_extension(""),
            id,
            log_path,
            file,
        }
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
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let line = stamped(entry, u64::try_from(ts).unwrap_or(u64::MAX));
        self.file
            .write_all(line.as_bytes())
            .with_context(|| format!("could not write {}", self.log_path.display()))
    }
}

/// How a tool call ended, as the log kept it beside the result.
#[derive(Debug, Default, PartialEq)]
pub struct ToolEnd {
    pub is_error: bool,
    pub brief: String,
}

/// What `prune` removed: how many sessions, and how many bytes they took.
#[derive(Debug, Default, PartialEq)]
pub struct Pruned {
    pub sessions: usize,
    pub bytes: u64,
}

/// Replays a log: messages in order, restarting from the summary at each compaction.
/// Resume loads once via `load_reported`; only the tests call this.
#[allow(dead_code)]
pub fn load(path: &Path) -> Result<Loaded> {
    Ok(load_reported(path)?.loaded)
}

/// Whether `create`'s name was already taken, so it can try the next one.
fn name_taken(err: &anyhow::Error) -> bool {
    err.root_cause()
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists)
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

#[cfg(test)]
mod tests;
