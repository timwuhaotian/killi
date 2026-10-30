use super::*;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::watch;

/// A running command: its process group, its output so far, and its exit once known.
pub(super) struct Process {
    pub(super) pgid: u32,
    pub(super) output: Arc<Mutex<Captured>>,
    pub(super) exit: watch::Receiver<Option<Exit>>,
}

#[derive(Default)]
pub(super) struct Captured {
    pub(super) bytes: Vec<u8>,
    pub(super) head: Vec<u8>,
    /// Bytes dropped from the front to stay under MAX_CAPTURE.
    pub(super) dropped: usize,
    /// When the last output arrived.
    pub(super) last: Option<Instant>,
}

impl Captured {
    fn append(&mut self, chunk: &[u8]) {
        let head = HEAD_CAPTURE
            .saturating_sub(self.head.len())
            .min(chunk.len());
        self.head.extend_from_slice(&chunk[..head]);
        self.bytes.extend_from_slice(chunk);
        self.last = Some(Instant::now());
        if self.bytes.len() > MAX_CAPTURE {
            self.trim(MAX_CAPTURE);
        }
    }

    /// Drops the oldest bytes to leave at most `keep`.
    pub(super) fn trim(&mut self, keep: usize) {
        if let Some(excess) = self.bytes.len().checked_sub(keep).filter(|&n| n > 0) {
            self.bytes.drain(..excess);
            self.dropped += excess;
        }
        if self.bytes.capacity() > 2 * self.bytes.len().max(keep) {
            self.bytes.shrink_to_fit();
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Exit {
    Code(i32),
    Signal,
}

/// The process group of every live `Process`, with its leader's exit, so `kill_all` can end
/// them when killi is told to quit and nothing gets dropped.
static GROUPS: Mutex<BTreeMap<u32, watch::Receiver<Option<Exit>>>> = Mutex::new(BTreeMap::new());

pub(super) fn groups() -> MutexGuard<'static, BTreeMap<u32, watch::Receiver<Option<Exit>>>> {
    GROUPS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether process group `pgid` still has a process in it. Once its leader is reaped the
/// leader's pid is free again, so a live process with that pid means the id was reused and the
/// group is gone.
fn group_running(pgid: u32, leader_reaped: bool) -> bool {
    let pgid = pgid as libc::pid_t;
    // SAFETY: signal 0 sends nothing; it only checks that the group or process exists.
    let group = pgid > 0 && unsafe { libc::killpg(pgid, 0) } == 0;
    group && !(leader_reaped && unsafe { libc::kill(pgid, 0) } == 0)
}

/// Ends every process group killi started that still runs. For quitting on a signal, when
/// no `Process` gets dropped.
pub fn kill_all() {
    for (&pgid, exit) in groups().iter() {
        if group_running(pgid, exit.borrow().is_some()) {
            // SAFETY: killpg only sends a signal to a process group we started.
            unsafe { libc::killpg(pgid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

/// A process group killi started outside a `Process` — a hook, the browser, a directly
/// started MCP server, `pdftotext` — registered so `kill_all` ends it too. The guard
/// unregisters the group when the child is reaped; while it lives, `kill_all` covers it.
pub struct Tracked {
    pgid: u32,
}

/// Registers `pgid` (a session leader's pid, so its own process group) with `kill_all`.
pub fn track(pgid: u32) -> Tracked {
    let (_, exit) = watch::channel(None);
    groups().insert(pgid, exit);
    Tracked { pgid }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        groups().remove(&self.pgid);
    }
}

impl Exit {
    pub(super) fn describe(self) -> String {
        match self {
            Exit::Code(code) => format!("exited with code {code}"),
            Exit::Signal => "was killed by a signal".into(),
        }
    }
}

/// Starts a command in a session of its own, so it has no controlling terminal: `killpg` on its
/// pid still ends it with everything it started (a new session is also a new process group),
/// and nothing it runs can take the terminal from killi. A child in a process group of its own
/// (`process_group(0)`) still shares killi's session and terminal: a shell run with `-i` (zsh
/// opens `/dev/tty` when stdin isn't one) made itself the foreground process group, killi's
/// next read of the keyboard stopped it with SIGTTIN, and the user's shell printed `suspended
/// (tty input)` over the prompt with the mouse reports still on.
pub trait OwnSession {
    fn own_session(&mut self) -> &mut Self;
}

fn setsid() -> std::io::Result<()> {
    // SAFETY: setsid takes no arguments and is async-signal-safe, as `pre_exec` requires.
    if unsafe { libc::setsid() } == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl OwnSession for Command {
    fn own_session(&mut self) -> &mut Self {
        // SAFETY: the closure only calls setsid.
        unsafe { self.pre_exec(setsid) }
    }
}

impl OwnSession for std::process::Command {
    fn own_session(&mut self) -> &mut Self {
        use std::os::unix::process::CommandExt;
        // SAFETY: the closure only calls setsid.
        unsafe { self.pre_exec(setsid) }
    }
}

impl Process {
    pub(super) fn spawn(dir: &Path, command: &str) -> std::io::Result<Self> {
        let mut child = Command::new("/bin/bash")
            .arg("-c")
            .arg(format!("exec 2>&1\n{command}"))
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("PAGER", "cat")
            .env("GIT_PAGER", "cat")
            .env(crate::live::PID_VAR, std::process::id().to_string())
            .own_session()
            .spawn()?;
        let pgid = child.id().unwrap_or_default();
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let output = Arc::new(Mutex::new(Captured::default()));
        let (tx, exit) = watch::channel(None);
        let sink = output.clone();
        // One task reads and waits, so the exit is known when the shell ends even if a job it
        // started keeps the pipe open (`server &`), and that job's output is still captured.
        tokio::spawn(async move {
            let mut buf = [0u8; 16 * 1024];
            let mut reading = true;
            let mut ended = false;
            while reading || !ended {
                tokio::select! {
                    n = stdout.read(&mut buf), if reading => match n {
                        Ok(0) | Err(_) => reading = false,
                        Ok(n) => sink.lock().expect("output lock").append(&buf[..n]),
                    },
                    status = child.wait(), if !ended => {
                        ended = true;
                        // What the shell wrote before it ended is in the pipe: take it before
                        // announcing the exit, so a finished command's output is all there.
                        while reading {
                            match tokio::time::timeout(DRAIN, stdout.read(&mut buf)).await {
                                Ok(Ok(n)) if n > 0 => sink.lock().expect("output lock").append(&buf[..n]),
                                Ok(_) => reading = false,
                                Err(_) => break,
                            }
                        }
                        let _ = tx.send(Some(match status.ok().and_then(|s| s.code()) {
                            Some(code) => Exit::Code(code),
                            None => Exit::Signal,
                        }));
                    }
                }
            }
        });
        groups().insert(pgid, exit.clone());
        Ok(Self { pgid, output, exit })
    }

    pub(super) fn exit(&self) -> Option<Exit> {
        *self.exit.borrow()
    }

    /// Whether anything of the command still runs: its shell, or what it started and left
    /// running, such as `nohup server &`.
    pub(super) fn running(&self) -> bool {
        group_running(self.pgid, self.exit().is_some())
    }

    pub(super) async fn wait(&mut self, limit: Duration) -> Option<Exit> {
        let _ = tokio::time::timeout(limit, self.exit.wait_for(Option::is_some)).await;
        self.exit()
    }

    pub(super) fn kill(&self) {
        if self.running() {
            // SAFETY: killpg only sends a signal to the process group we started.
            unsafe { libc::killpg(self.pgid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.kill();
        groups().remove(&self.pgid);
    }
}

/// Waits up to `limit` for a command and what it left running: its shell's exit, then until
/// its process group is empty. `nohup job &` exits at once while the job runs on, so waiting
/// for the shell alone would return at once too.
pub(super) async fn wait_for_group(
    pgid: u32,
    mut exit: watch::Receiver<Option<Exit>>,
    limit: Duration,
) {
    let deadline = Instant::now() + limit;
    let _ = tokio::time::timeout(limit, exit.wait_for(Option::is_some)).await;
    while exit.borrow().is_some() && group_running(pgid, true) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
