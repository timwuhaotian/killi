use super::*;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
    /// Adds `chunk` to the output. Past `MAX_CAPTURE` the oldest bytes go down to three
    /// quarters of it, so the memmove of what remains happens once per 4 MB of output, not on
    /// every read.
    pub(super) fn append(&mut self, chunk: &[u8]) {
        let head = HEAD_CAPTURE
            .saturating_sub(self.head.len())
            .min(chunk.len());
        self.head.extend_from_slice(&chunk[..head]);
        self.bytes.extend_from_slice(chunk);
        self.last = Some(Instant::now());
        if self.bytes.len() > MAX_CAPTURE {
            self.trim(MAX_CAPTURE / 4 * 3);
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
    /// The shell exited 0, but an earlier stage of its last pipeline (`false | true`) ended
    /// with this code, so what the command was after failed. Only the last pipeline is seen:
    /// one that ran before a closing `echo` or `&&` is gone by the time the shell exits.
    Pipeline(i32),
    Signal,
}

/// How a stage killed by SIGPIPE shows in `PIPESTATUS`; `yes | head` always ends that way.
const SIGPIPE: i32 = 128 + libc::SIGPIPE;

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
            Exit::Pipeline(code) => format!(
                "exited with code 0 but an earlier stage of its pipeline exited with {code}"
            ),
            Exit::Signal => "was killed by a signal".into(),
        }
    }
}

/// Where the EXIT trap of a piped command leaves `PIPESTATUS`; no two commands share one.
/// The file is created here, so no one else can hold that name and have the shell write to
/// it: the name is only used when this process could take it.
fn status_file() -> Option<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path =
        std::env::temp_dir().join(format!("killi-pipestatus-{}-{nanos}", std::process::id()));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()
        .map(|_| path)
}

/// `path` inside double quotes within the EXIT trap.
fn quoted(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

/// How a shell that ended with `code` exited, given the `PIPESTATUS` its EXIT trap wrote to
/// `path`, which is removed. `pipefail` would do this job, but it also fails `| head` on the
/// SIGPIPE its first stage dies of, so a stage killed that way doesn't count here.
fn shell_exit(code: Option<i32>, path: Option<&Path>) -> Exit {
    let failed_stage = path.and_then(|path| {
        let statuses = std::fs::read_to_string(path).ok();
        let _ = std::fs::remove_file(path);
        statuses.and_then(|statuses| {
            let codes = statuses
                .split_whitespace()
                .map(|code| code.parse::<i32>().ok())
                .collect::<Option<Vec<_>>>()?;
            let (last, earlier) = codes.split_last()?;
            if *last != 0 {
                return None;
            }
            earlier
                .iter()
                .copied()
                .find(|&code| code != 0 && code != SIGPIPE)
        })
    });
    match (code, failed_stage) {
        (Some(0), Some(code)) => Exit::Pipeline(code),
        (Some(code), _) => Exit::Code(code),
        (None, _) => Exit::Signal,
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

/// Git config forced on a read-only command: a bare repository committed inside a project is
/// used only when named with `--git-dir`, and no repository's `core.fsmonitor` runs, so a
/// `git status` in one can't run code its config names.
const READ_ONLY_GIT: [(&str, &str); 2] = [
    ("safe.bareRepository", "explicit"),
    ("core.fsmonitor", "false"),
];

/// More inherited `GIT_CONFIG_COUNT` entries than any real setup has: git would fail looking
/// for keys that aren't there.
const MAX_INHERITED_GIT_CONFIG: usize = 1_000;

/// The environment that adds `READ_ONLY_GIT` to git's command-line config, after the entries
/// `count` (the inherited `GIT_CONFIG_COUNT`) says are already set, so those still apply. A
/// count git would reject, or one past `MAX_INHERITED_GIT_CONFIG`, is replaced: git then
/// runs with `READ_ONLY_GIT` alone instead of failing on it.
fn read_only_git_env(count: Option<&str>) -> Vec<(String, String)> {
    let (first, total) = count
        .and_then(|n| n.trim().parse::<usize>().ok())
        .filter(|n| *n <= MAX_INHERITED_GIT_CONFIG)
        .and_then(|n| Some((n, n.checked_add(READ_ONLY_GIT.len())?)))
        .unwrap_or((0, READ_ONLY_GIT.len()));
    let mut env = vec![("GIT_CONFIG_COUNT".to_string(), total.to_string())];
    for (n, (key, value)) in READ_ONLY_GIT.iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{}", first + n), key.to_string()));
        env.push((format!("GIT_CONFIG_VALUE_{}", first + n), value.to_string()));
    }
    env
}

impl Process {
    /// Starts `command` in `dir`. A `read_only` command (plan mode's) runs its git under
    /// `READ_ONLY_GIT`.
    pub(super) fn spawn(dir: &Path, command: &str, read_only: bool) -> std::io::Result<Self> {
        // A command with a pipe gets an EXIT trap that saves `PIPESTATUS`, so a failure before
        // the last stage (`npm test | tail`) isn't hidden by the last stage's exit 0. One without
        // runs as it was written. A command that sets its own EXIT trap, or `exec`s, loses the
        // trap and is judged by its exit code alone, as every command used to be.
        let status_file = command.contains('|').then(status_file).flatten();
        let script = match &status_file {
            Some(path) => format!(
                "exec 2>&1\ntrap 'echo \"${{PIPESTATUS[@]}}\" >\"{}\"' EXIT\n{command}",
                quoted(path)
            ),
            None => format!("exec 2>&1\n{command}"),
        };
        let git = if read_only {
            read_only_git_env(std::env::var("GIT_CONFIG_COUNT").ok().as_deref())
        } else {
            Vec::new()
        };
        let spawned = Command::new("/bin/bash")
            .envs(git)
            .arg("-c")
            .arg(script)
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
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                if let Some(path) = &status_file {
                    let _ = std::fs::remove_file(path);
                }
                return Err(error);
            }
        };
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
                        let _ = tx.send(Some(shell_exit(
                            status.ok().and_then(|s| s.code()),
                            status_file.as_deref(),
                        )));
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

    /// Kills the whole group. On macOS a child the shell was forking as the signal went out
    /// can come up after it and outlive it, so the group is signalled again for a moment
    /// while anything of it is left.
    pub(super) fn kill(&self) {
        if !self.running() {
            return;
        }
        let pgid = self.pgid;
        // SAFETY: killpg only sends a signal to the process group we started.
        unsafe { libc::killpg(pgid as libc::pid_t, libc::SIGKILL) };
        let exit = self.exit.clone();
        std::thread::spawn(move || {
            for _ in 0..20 {
                std::thread::sleep(Duration::from_millis(50));
                if !group_running(pgid, exit.borrow().is_some()) {
                    return;
                }
                // SAFETY: as above; the check just made says the group is still ours.
                unsafe { libc::killpg(pgid as libc::pid_t, libc::SIGKILL) };
            }
        });
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

#[cfg(test)]
mod tests {
    use super::read_only_git_env;

    #[test]
    fn read_only_git_config_follows_the_entries_already_set() {
        let pairs = |count| {
            read_only_git_env(count)
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            pairs(None),
            [
                "GIT_CONFIG_COUNT=2",
                "GIT_CONFIG_KEY_0=safe.bareRepository",
                "GIT_CONFIG_VALUE_0=explicit",
                "GIT_CONFIG_KEY_1=core.fsmonitor",
                "GIT_CONFIG_VALUE_1=false",
            ]
        );
        assert_eq!(
            pairs(Some("3")),
            [
                "GIT_CONFIG_COUNT=5",
                "GIT_CONFIG_KEY_3=safe.bareRepository",
                "GIT_CONFIG_VALUE_3=explicit",
                "GIT_CONFIG_KEY_4=core.fsmonitor",
                "GIT_CONFIG_VALUE_4=false",
            ]
        );
        let alone = pairs(None);
        for bogus in [
            "x",
            "-1",
            "",
            "18446744073709551615",
            "99999999999999999999",
            "1001",
        ] {
            assert_eq!(pairs(Some(bogus)), alone, "GIT_CONFIG_COUNT={bogus:?}");
        }
        assert_eq!(pairs(Some(" 1000 "))[0], "GIT_CONFIG_COUNT=1002");
    }
}
