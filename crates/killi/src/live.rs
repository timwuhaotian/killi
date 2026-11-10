//! Live sessions: every top-level killi says where it works and what it is doing in
//! `~/.killi/live/<pid>.json`, so the sessions on this machine know about each other, and an
//! interactive one listens on a socket of its own, so another session or the user's shell can
//! see what it is doing and what it can be asked to do (`killi sessions <pid>`, `killi card
//! <pid>`), ask it about its work (`killi ask`), leave it a message (`killi tell`) or hand it a
//! task (`killi send`). The requests and the socket are in `wire`, a task's states and events
//! in `task`, and the commands with the text they print in `command`.
//!
//! A session holds an exclusive `flock` on its file for as long as it runs. The kernel lets go
//! of it when the process ends, however it ends, so a file nobody holds belongs to a session
//! that is gone and the next reader removes it; a pid taken again by another process can't
//! pass for the old session. Unlike the MCP broker's socket, nothing here has the version in
//! its name: sessions on different releases still see each other, and unknown fields are
//! ignored.

mod card;
mod command;
mod note;
mod task;
mod wire;

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Seek};
use std::os::unix::fs::{DirBuilderExt, FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub use card::{card, card_text};
pub use command::command;
pub use note::{ask_note, tell_note, tree_note};
pub use task::{
    MAX_TASK_TIMEOUT, TASK_TIMEOUT, TaskState, TaskView, event_line, task_owner, view_line,
};
pub use wire::{Incoming, Reply, Request, follow, listen, send};

use wire::status_of;

#[cfg(test)]
use command::{event_text, list_text, next_event, send_args, status_text};
#[cfg(test)]
use wire::Envelope;

/// Set on every command killi runs to the pid of the session running it, so `killi ask` and
/// `killi tell` say who is asking and refuse to talk to their own session.
pub const PID_VAR: &str = "KILLI_PID";

/// The longest question or message, in bytes.
pub const MAX_TEXT: usize = 8 * 1024;

/// What a session says about itself in its file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub pid: u32,
    pub version: String,
    pub cwd: PathBuf,
    /// The git work tree it works in, or its folder outside one: sessions in one tree share
    /// files and branches.
    pub tree: PathBuf,
    pub branch: Option<String>,
    pub model: String,
    pub log: PathBuf,
    /// When it started, in ms since the epoch.
    pub started: u64,
    pub working: bool,
    /// When its current or last turn started, or its last turn ended, in ms since the epoch.
    pub since: u64,
    /// The first line of its current or last prompt.
    pub prompt: String,
    /// The socket it answers on; none for a `-p` run, which takes no questions or messages.
    pub socket: Option<PathBuf>,
    /// The tasks other sessions sent that it is running now: their agents change files in its
    /// tree while its own turn may sit idle.
    pub tasks: usize,
    /// When one of those tasks last started or ended, in ms since the epoch.
    pub task_since: u64,
}

impl Entry {
    /// Whether it worked since `since` (ms since the epoch) or still does, on a turn of its own
    /// or on a task another session sent it.
    pub fn worked_since(&self, since: u64) -> bool {
        self.working || self.tasks > 0 || self.since.max(self.task_since) >= since
    }
}

/// Milliseconds since the epoch.
pub fn now() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// The folder of the sessions' files.
pub fn dir() -> PathBuf {
    crate::config::killi_home().join("live")
}

/// The git work tree `cwd` is in (a worktree's own root, since its `.git` is a file), or
/// `cwd` outside one.
pub fn work_tree(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// Where the session with `pid` listens, in the temporary folder because socket paths are
/// limited to about 100 bytes.
pub fn socket_path(pid: u32) -> PathBuf {
    // SAFETY: getuid has no preconditions and can't fail.
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir()
        .join(format!("killi-live-{uid}"))
        .join(format!("{pid}.sock"))
}

/// Joins the live sessions from `cwd`: writes this session's file and, when `listen`, opens
/// the socket other sessions reach it on. None when the file can't be written.
pub fn join(
    cwd: &Path,
    listen: bool,
) -> Option<(Registration, Option<mpsc::UnboundedReceiver<Incoming>>)> {
    let pid = std::process::id();
    let socket = socket_path(pid);
    let incoming = if listen {
        self::listen(&socket).ok()
    } else {
        None
    };
    let started = now();
    let entry = Entry {
        pid,
        version: env!("CARGO_PKG_VERSION").to_string(),
        cwd: cwd.to_path_buf(),
        tree: work_tree(cwd),
        started,
        since: started,
        socket: incoming.as_ref().map(|_| socket.clone()),
        ..Entry::default()
    };
    match Registration::open(entry) {
        Ok(registration) => Some((registration, incoming)),
        Err(_) => {
            let _ = fs::remove_file(&socket);
            None
        }
    }
}

/// This session's file, held locked until it is dropped, which removes it.
pub struct Registration {
    file: File,
    path: PathBuf,
    pub entry: Entry,
}

impl Registration {
    pub fn open(entry: Entry) -> Result<Self> {
        Self::open_in(&dir(), entry)
    }

    /// Writes and locks the file under another name first, so a reader never finds it
    /// unlocked and takes it for a session that ended; the lock goes with the file when it
    /// is renamed.
    pub(crate) fn open_in(dir: &Path, entry: Entry) -> Result<Self> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
        let path = dir.join(format!("{}.json", entry.pid));
        let staging = path.with_extension("json.new");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&staging)?;
        file.lock()?;
        let mut registration = Self { file, path, entry };
        registration.write()?;
        fs::rename(&staging, &registration.path)?;
        Ok(registration)
    }

    /// Changes what the file says. A reader may catch it half written and skip it once.
    pub fn update(&mut self, change: impl FnOnce(&mut Entry)) {
        let before = self.entry.clone();
        change(&mut self.entry);
        if self.entry != before {
            let _ = self.write();
        }
    }

    fn write(&mut self) -> std::io::Result<()> {
        let text = serde_json::to_vec(&self.entry)?;
        self.file.write_all_at(&text, 0)?;
        self.file.set_len(text.len() as u64)
    }

    /// Where this session works, for the agent to find the sessions that work there too.
    pub fn here(&self) -> Here {
        Here {
            pid: self.entry.pid,
            tree: self.entry.tree.clone(),
            dir: self
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(dir),
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        if let Some(socket) = &self.entry.socket {
            let _ = fs::remove_file(socket);
        }
    }
}

/// Removes this process's file and socket, for a quit on a signal, which runs no destructors.
/// Without it, the next reader would remove them.
pub fn leave() {
    let pid = std::process::id();
    let _ = fs::remove_file(dir().join(format!("{pid}.json")));
    let _ = fs::remove_file(socket_path(pid));
}

/// A session's place among the live ones: its pid, its work tree, and the folder of the
/// sessions' files.
pub struct Here {
    pub pid: u32,
    pub tree: PathBuf,
    dir: PathBuf,
}

impl Here {
    /// The other sessions working in this tree now, oldest first.
    pub fn others(&self) -> Vec<Entry> {
        list_in(&self.dir)
            .into_iter()
            .filter(|e| e.pid != self.pid && e.tree == self.tree)
            .collect()
    }
}

/// Every session running on this machine, oldest first. Files of sessions that ended are
/// removed on the way.
pub fn list() -> Vec<Entry> {
    list_in(&dir())
}

fn list_in(dir: &Path) -> Vec<Entry> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for path in read.flatten().map(|e| e.path()) {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let staging = name.ends_with(".json.new");
        if !staging && !name.ends_with(".json") {
            continue;
        }
        let Ok(mut file) = File::open(&path) else {
            continue;
        };
        match file.try_lock_shared() {
            Ok(()) => {
                // A file still being written can be unlocked for a moment; one left behind is old.
                let old = fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(10));
                if !staging || old {
                    if let Some(socket) = read_entry(&mut file).and_then(|e| e.socket) {
                        let _ = fs::remove_file(socket);
                    }
                    let _ = fs::remove_file(&path);
                }
            }
            Err(TryLockError::WouldBlock) if !staging => entries.extend(read_entry(&mut file)),
            Err(_) => {}
        }
    }
    entries.sort_by_key(|e| (e.started, e.pid));
    entries
}

/// The entry in `file`, read again once if it was caught half written.
fn read_entry(file: &mut File) -> Option<Entry> {
    for _ in 0..2 {
        let mut text = String::new();
        if file.rewind().is_ok()
            && file.read_to_string(&mut text).is_ok()
            && let Ok(entry) = serde_json::from_str(&text)
        {
            return Some(entry);
        }
    }
    None
}

/// A session in a line: pid, state, folder and branch, and its prompt.
pub fn describe(entry: &Entry, now: u64) -> String {
    let state = if entry.working { "working" } else { "idle" };
    let took = span(now.saturating_sub(entry.since));
    let branch = entry
        .branch
        .as_ref()
        .map(|b| format!(" ({b})"))
        .unwrap_or_default();
    let prompt = if entry.prompt.is_empty() {
        String::new()
    } else {
        format!(" · “{}”", crate::llm::truncate(&entry.prompt, 80))
    };
    format!(
        "{} · {state} for {took} · {}{branch}{prompt}",
        entry.pid,
        tilde(&entry.cwd)
    )
}

/// The sender of a question or message in a few words, from its file when it has one.
pub fn sender(from: Option<u32>) -> String {
    let Some(pid) = from else {
        return "outside any killi session, through `killi tell` or `killi ask`".into();
    };
    match list().into_iter().find(|e| e.pid == pid) {
        Some(entry) => {
            let prompt = if entry.prompt.is_empty() {
                String::new()
            } else {
                format!(", on “{}”", crate::llm::truncate(&entry.prompt, 80))
            };
            format!("killi session {pid}, in {}{prompt}", tilde(&entry.cwd))
        }
        None => format!("killi session {pid}"),
    }
}

/// `path` with the home folder as `~`.
fn tilde(path: &Path) -> String {
    match path.strip_prefix(crate::config::home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// A duration in milliseconds as `45s`, `12m` or `2h05m`.
fn span(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m", secs / 60),
        _ => format!("{}h{:02}m", secs / 3_600, secs % 3_600 / 60),
    }
}

#[cfg(test)]
mod tests;
