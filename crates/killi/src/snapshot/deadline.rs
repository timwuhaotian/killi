//! One command run to a deadline. A snapshot's `git add -A` reads the whole tree, and a huge
//! file, a slow disk or a clean filter from the user's git config can make it take minutes.

use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// How long the output pipes may stay open after the command exited, held by something it
/// started, before its output is given up on.
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// How often a thread reading a pipe that stays quiet checks whether it was given up on.
const DRAIN_POLL_MS: libc::c_int = 50;

/// A command stopped for running past its deadline.
#[derive(Debug)]
pub(super) struct TimedOut(Duration);

impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "it ran past {} s and was stopped", self.0.as_secs())
    }
}

impl std::error::Error for TimedOut {}

/// A command stopped because the snapshot it was part of was given up on.
#[derive(Debug)]
pub(super) struct Canceled;

impl std::fmt::Display for Canceled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("it was given up on and stopped")
    }
}

impl std::error::Error for Canceled {}

/// Whether a command stopped early, at its deadline or because its work was given up on, so
/// it may have left its lock behind and is not worth another try.
pub(super) fn stopped(error: &anyhow::Error) -> bool {
    error.is::<TimedOut>() || error.is::<Canceled>()
}

/// Runs `command` to its end, writing `input` to its stdin, and returns what it printed. It
/// runs in a process group of its own; past `limit`, or once `cancel` is set, that whole group
/// is killed, so what it started goes too, and this fails. The threads reading its output end
/// with this call, even when something outside that group still holds the pipes.
pub(super) fn output(
    command: &mut Command,
    input: Option<Vec<u8>>,
    limit: Duration,
    cancel: &AtomicBool,
) -> Result<Output> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Canceled.into());
    }
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("could not run git")?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let given_up = GiveUp(Arc::new(AtomicBool::new(false)));
    let stdout = drain(child.stdout.take(), given_up.0.clone());
    let stderr = drain(child.stderr.take(), given_up.0.clone());
    let start = Instant::now();
    let mut pause = Duration::from_millis(1);
    let status = loop {
        if let Some(status) = child.try_wait().context("could not wait for git")? {
            break status;
        }
        let elapsed = start.elapsed();
        let canceled = cancel.load(Ordering::Relaxed);
        if canceled || elapsed >= limit {
            // SAFETY: killpg only signals the group this command leads; the leader isn't
            // reaped yet, so its id still names that group.
            unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
            let _ = child.wait();
            return Err(if canceled {
                Canceled.into()
            } else {
                TimedOut(limit).into()
            });
        }
        std::thread::sleep(pause.min(limit - elapsed));
        pause = (pause * 2).min(Duration::from_millis(20));
    };
    let (Ok(stdout), Ok(stderr)) = (
        stdout.recv_timeout(PIPE_GRACE),
        stderr.recv_timeout(PIPE_GRACE),
    ) else {
        bail!("git exited, but something it started still holds its output open");
    };
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Tells the threads reading a command's output to stop when the call that started them
/// returns.
struct GiveUp(Arc<AtomicBool>);

impl Drop for GiveUp {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Reads a pipe to its end on a thread of its own, so a command that prints more than a pipe
/// holds never waits on this one. The thread stops early once `given_up` is set.
fn drain(
    pipe: Option<impl Read + AsRawFd + Send + 'static>,
    given_up: Arc<AtomicBool>,
) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(mut pipe) = pipe {
            let mut buffer = vec![0; 64 * 1024];
            while !given_up.load(Ordering::Relaxed) {
                let mut ready = libc::pollfd {
                    fd: pipe.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one pollfd for a descriptor this thread owns, for the length given.
                match unsafe { libc::poll(&mut ready, 1, DRAIN_POLL_MS) } {
                    0 => continue,
                    n if n < 0 => {
                        if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                            continue;
                        }
                        break;
                    }
                    _ => {}
                }
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => out.extend_from_slice(&buffer[..n]),
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        let _ = tx.send(out);
    });
    rx
}
