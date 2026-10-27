//! Live sessions: every top-level killi says where it works and what it is doing in
//! `~/.killi/live/<pid>.json`, so the sessions on this machine know about each other, and an
//! interactive one listens on a socket of its own, so another session or the user's shell can
//! see what it is doing (`killi sessions <pid>`), ask it about its work (`killi ask`) or leave
//! it a message (`killi tell`).
//!
//! A session holds an exclusive `flock` on its file for as long as it runs. The kernel lets go
//! of it when the process ends, however it ends, so a file nobody holds belongs to a session
//! that is gone and the next reader removes it; a pid taken again by another process can't
//! pass for the old session. Unlike the MCP broker's socket, nothing here has the version in
//! its name: sessions on different releases still see each other, and unknown fields are
//! ignored.

mod note;

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Seek};
use std::os::unix::fs::{DirBuilderExt, FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

pub use note::{ask_note, tell_note, tree_note};

/// Set on every command killi runs to the pid of the session running it, so `killi ask` and
/// `killi tell` say who is asking and refuse to talk to their own session.
pub const PID_VAR: &str = "KILLI_PID";

/// The longest question or message, in bytes.
pub const MAX_TEXT: usize = 8 * 1024;

/// The longest request line a session reads.
const MAX_REQUEST: u64 = 16 * 1024;

/// How long `killi ask` waits: the other session's model reads its whole conversation first.
const ASK_TIMEOUT: Duration = Duration::from_secs(300);

/// How long a status or a message waits for the other session to take it.
const QUICK_TIMEOUT: Duration = Duration::from_secs(10);

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
}

/// Milliseconds since the epoch.
pub fn now() -> u64 {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()).unwrap_or(u64::MAX)
}

/// The folder of the sessions' files.
pub fn dir() -> PathBuf {
    crate::config::killi_home().join("live")
}

/// The git work tree `cwd` is in (a worktree's own root, since its `.git` is a file), or
/// `cwd` outside one.
pub fn work_tree(cwd: &Path) -> PathBuf {
    cwd.ancestors().find(|dir| dir.join(".git").exists()).unwrap_or(cwd).to_path_buf()
}

/// Where the session with `pid` listens, in the temporary folder because socket paths are
/// limited to about 100 bytes.
pub fn socket_path(pid: u32) -> PathBuf {
    // SAFETY: getuid has no preconditions and can't fail.
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir().join(format!("killi-live-{uid}")).join(format!("{pid}.sock"))
}

/// Joins the live sessions from `cwd`: writes this session's file and, when `listen`, opens
/// the socket other sessions reach it on. None when the file can't be written.
pub fn join(cwd: &Path, listen: bool) -> Option<(Registration, Option<mpsc::UnboundedReceiver<Incoming>>)> {
    let pid = std::process::id();
    let socket = socket_path(pid);
    let incoming = if listen { self::listen(&socket).ok() } else { None };
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
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("could not create {}", dir.display()))?;
        let path = dir.join(format!("{}.json", entry.pid));
        let staging = path.with_extension("json.new");
        let file = OpenOptions::new().create(true).truncate(true).read(true).write(true).mode(0o600).open(&staging)?;
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
        Here { pid: self.entry.pid, tree: self.entry.tree.clone(), dir: self.path.parent().map(Path::to_path_buf).unwrap_or_else(dir) }
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
        list_in(&self.dir).into_iter().filter(|e| e.pid != self.pid && e.tree == self.tree).collect()
    }
}

/// Every session running on this machine, oldest first. Files of sessions that ended are
/// removed on the way.
pub fn list() -> Vec<Entry> {
    list_in(&dir())
}

fn list_in(dir: &Path) -> Vec<Entry> {
    let Ok(read) = fs::read_dir(dir) else { return Vec::new() };
    let mut entries = Vec::new();
    for path in read.flatten().map(|e| e.path()) {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let staging = name.ends_with(".json.new");
        if !staging && !name.ends_with(".json") {
            continue;
        }
        let Ok(mut file) = File::open(&path) else { continue };
        match file.try_lock_shared() {
            Ok(()) => {
                // A file still being written can be unlocked for a moment; one left behind is old.
                let old = fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > Duration::from_secs(10));
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
        if file.rewind().is_ok() && file.read_to_string(&mut text).is_ok()
            && let Ok(entry) = serde_json::from_str(&text)
        {
            return Some(entry);
        }
    }
    None
}

/// What one session asks of another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Request {
    /// What it is doing, answered without the model.
    Status,
    /// A question its model answers from its conversation, without keeping it.
    Ask {
        #[serde(default)]
        from: Option<u32>,
        text: String,
    },
    /// A message for its model's next step, or its user's next prompt.
    Tell {
        #[serde(default)]
        from: Option<u32>,
        text: String,
    },
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    #[serde(default)]
    v: u32,
    #[serde(flatten)]
    request: Request,
}

/// A request from another session and where its reply goes: `{"ok": true, …}`, or
/// `{"error": "…"}` to say why not.
pub struct Incoming {
    pub request: Request,
    pub reply: oneshot::Sender<Value>,
}

/// Listens on `socket` for other sessions of this user and hands each request to the
/// receiver; the connection waits for the reply.
pub fn listen(socket: &Path) -> Result<mpsc::UnboundedReceiver<Incoming>> {
    if let Some(dir) = socket.parent() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("could not create {}", dir.display()))?;
    }
    let _ = fs::remove_file(socket);
    let listener = UnixListener::bind(socket).with_context(|| format!("could not listen on {}", socket.display()))?;
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let _ = serve(stream, tx).await;
            });
        }
    });
    Ok(rx)
}

async fn serve(stream: UnixStream, session: mpsc::UnboundedSender<Incoming>) -> Result<()> {
    // SAFETY: getuid has no preconditions and can't fail.
    if stream.peer_cred()?.uid() != unsafe { libc::getuid() } {
        bail!("a process of another user connected");
    }
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    BufReader::new(reader.take(MAX_REQUEST)).read_line(&mut line).await?;
    let reply = match serde_json::from_str::<Envelope>(&line) {
        Ok(envelope) => {
            let (reply, answer) = oneshot::channel();
            match session.send(Incoming { request: envelope.request, reply }) {
                Ok(()) => answer.await.unwrap_or_else(|_| json!({"error": "the session ended before it answered"})),
                Err(_) => json!({"error": "the session is ending"}),
            }
        }
        Err(e) => json!({"error": format!("killi {} doesn't understand this request: {e}", env!("CARGO_PKG_VERSION"))}),
    };
    writer.write_all(format!("{reply}\n").as_bytes()).await?;
    Ok(())
}

/// Sends `request` to the session `to` and gives back its reply.
pub async fn send(to: &Entry, request: Request) -> Result<Value> {
    let socket = to.socket.as_ref().ok_or_else(|| anyhow!("session {} runs headless (-p) and takes no questions or messages", to.pid))?;
    let wait = if matches!(request, Request::Ask { .. }) { ASK_TIMEOUT } else { QUICK_TIMEOUT };
    let exchange = async {
        let mut stream = UnixStream::connect(socket).await.with_context(|| format!("could not reach session {}", to.pid))?;
        stream.write_all(format!("{}\n", serde_json::to_string(&Envelope { v: 1, request })?).as_bytes()).await?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await?;
        serde_json::from_str::<Value>(&line).context("the session closed the connection without answering")
    };
    let reply = tokio::time::timeout(wait, exchange).await.map_err(|_| anyhow!("session {} didn't answer within {} s", to.pid, wait.as_secs()))??;
    if let Some(error) = reply["error"].as_str() {
        bail!("{error}");
    }
    Ok(reply)
}

/// `killi sessions [<pid>] [--json]`, `killi ask <pid> <question>` and `killi tell <pid> <message>`.
pub async fn command(args: &[String]) -> Result<ExitCode> {
    let me = std::env::var(PID_VAR).ok().and_then(|pid| pid.parse::<u32>().ok());
    let sessions = list();
    let find = |pid: Option<&String>| -> Result<Entry> {
        let pid: u32 = pid.and_then(|p| p.parse().ok()).ok_or_else(|| anyhow!("say which session, by its pid; `killi sessions` lists them"))?;
        if Some(pid) == me && args[0] != "sessions" {
            bail!("session {pid} is this session");
        }
        sessions.iter().find(|e| e.pid == pid).cloned().ok_or_else(|| anyhow!("no killi session with pid {pid} is running; `killi sessions` lists them"))
    };
    let text = || -> Result<String> {
        let text = args.get(2..).unwrap_or_default().join(" ").trim().to_string();
        if text.is_empty() {
            bail!("say what to send: killi {} <pid> \"…\"", args[0]);
        }
        if text.len() > MAX_TEXT {
            bail!("that is {} bytes; the limit is {MAX_TEXT}", text.len());
        }
        Ok(text)
    };
    match args[0].as_str() {
        "sessions" => {
            let json_mode = args.iter().any(|a| a == "--json");
            let pid = args.iter().skip(1).find(|a| !a.starts_with('-'));
            match pid {
                Some(pid) => {
                    let entry = find(Some(pid))?;
                    let status = if entry.socket.is_some() { Some(send(&entry, Request::Status).await?) } else { None };
                    if json_mode {
                        println!("{}", json!({"session": entry, "status": status}));
                    } else {
                        println!("{}", status_text(&entry, status.as_ref(), now()));
                    }
                }
                None if json_mode => println!("{}", serde_json::to_string(&sessions)?),
                None => println!("{}", list_text(&sessions, me, now())),
            }
        }
        "ask" => {
            let to = find(args.get(1))?;
            let reply = send(&to, Request::Ask { from: me, text: text()? }).await?;
            println!("{}", reply["answer"].as_str().unwrap_or_default());
        }
        "tell" => {
            let to = find(args.get(1))?;
            let reply = send(&to, Request::Tell { from: me, text: text()? }).await?;
            println!("{}", reply["delivered"].as_str().unwrap_or("Delivered."));
        }
        other => bail!("unknown command `{other}`"),
    }
    Ok(ExitCode::SUCCESS)
}

/// `killi sessions`: one line for each session, this one marked.
fn list_text(sessions: &[Entry], me: Option<u32>, now: u64) -> String {
    if sessions.is_empty() {
        return "No killi session is running.".into();
    }
    sessions
        .iter()
        .map(|e| {
            let mut line = describe(e, now);
            if e.socket.is_none() {
                line.push_str(" · headless (-p)");
            }
            if Some(e.pid) == me {
                line.push_str(" · this session");
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `killi sessions <pid>`: the line from the list, then what it is doing now.
fn status_text(entry: &Entry, status: Option<&Value>, now: u64) -> String {
    let mut text = format!("{} · {} · killi {}", describe(entry, now), entry.model, entry.version);
    let Some(status) = status else { return text + "\nIt runs headless (-p) and can't say more." };
    if status["plan"].as_bool() == Some(true) {
        text.push_str("\nIn plan mode.");
    }
    let todos = status["todos"].as_array().cloned().unwrap_or_default();
    if !todos.is_empty() {
        let done = todos.iter().filter(|t| t["status"] == "completed").count();
        text.push_str(&format!("\nTasks {done}/{}:", todos.len()));
        for todo in &todos {
            let mark = match todo["status"].as_str() {
                Some("completed") => "✓",
                Some("in_progress") => "◐",
                _ => "○",
            };
            text.push_str(&format!("\n  {mark} {}", todo["content"].as_str().unwrap_or_default()));
        }
    }
    if let Some(waiting) = status["waiting"].as_str() {
        text.push_str(&format!("\nWaiting on:\n{waiting}"));
    }
    text
}

/// A session in a line: pid, state, folder and branch, and its prompt.
pub fn describe(entry: &Entry, now: u64) -> String {
    let state = if entry.working { "working" } else { "idle" };
    let took = span(now.saturating_sub(entry.since));
    let branch = entry.branch.as_ref().map(|b| format!(" ({b})")).unwrap_or_default();
    let prompt = if entry.prompt.is_empty() { String::new() } else { format!(" · “{}”", crate::llm::truncate(&entry.prompt, 80)) };
    format!("{} · {state} for {took} · {}{branch}{prompt}", entry.pid, tilde(&entry.cwd))
}

/// The sender of a question or message in a few words, from its file when it has one.
pub fn sender(from: Option<u32>) -> String {
    let Some(pid) = from else { return "outside any killi session, through `killi tell` or `killi ask`".into() };
    match list().into_iter().find(|e| e.pid == pid) {
        Some(entry) => {
            let prompt = if entry.prompt.is_empty() { String::new() } else { format!(", on “{}”", crate::llm::truncate(&entry.prompt, 80)) };
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
