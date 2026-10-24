//! `bash`, plus the background tasks it can leave running: a command that outlives its
//! timeout keeps going in the background instead of being killed, and so does what a finished
//! command left running in its process group, such as `nohup server &`. A `Process` kills its
//! group when dropped, so an interrupted turn or a closed session leaves nothing behind.
//! A call that waits stops waiting when the user sends a message, so a long wait never keeps
//! the model from reading it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::watch;

use super::{Context, MAX_BYTES, MAX_LINES, Output, spill};

const DEFAULT_TIMEOUT: u64 = 60;
const MAX_TIMEOUT: u64 = 300;
/// A command that starts with a `sleep` this many seconds long is refused: it waits the whole
/// time however soon the job it waits for ends.
const MAX_SLEEP: f64 = 30.0;
/// How often a waiting call asks whether the user has sent a message.
const WAKE_POLL: Duration = Duration::from_millis(100);
/// Output beyond this is dropped from memory, oldest first; the model only ever sees the tail.
const MAX_CAPTURE: usize = 16 * 1024 * 1024;
/// The beginning is also kept, within the display budget, so a failed command retains its root cause.
const HEAD_CAPTURE: usize = MAX_BYTES;
/// What a finished task keeps of output the model hasn't read; what it has read is let go.
const FINISHED_KEEP: usize = 1024 * 1024;
/// How long to read what a command wrote before it exited, once its shell is gone but a job it
/// started still holds the pipe.
const DRAIN: Duration = Duration::from_millis(20);
/// What a command that printed nothing returns.
const NO_OUTPUT: &str = "(no output)";
/// The same for a foreground command that exited 0, so an empty result isn't mistaken for a failure to run.
const NO_OUTPUT_OK: &str = "(no output, exit code 0)";

/// Whether the user has typed a message the model hasn't read yet, which ends a wait.
pub type Wake<'a> = &'a dyn Fn() -> bool;

/// Resolves once `wake` says so; checked after each poll, so a call that ends sooner wins.
async fn until(wake: Wake<'_>) {
    loop {
        tokio::time::sleep(WAKE_POLL).await;
        if wake() {
            return;
        }
    }
}

#[derive(Default)]
pub struct Tasks {
    next: u32,
    list: BTreeMap<u32, Task>,
}

struct Task {
    command: String,
    process: Process,
    started: Instant,
    /// How much of the output the model has already been shown.
    read_to: usize,
    reported: bool,
    /// The model was told at the end of a turn that this still runs, or stopped it.
    reminded: bool,
}

/// A running command: its process group, its output so far, and its exit once known.
struct Process {
    pgid: u32,
    output: Arc<Mutex<Captured>>,
    exit: watch::Receiver<Option<Exit>>,
}

#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    head: Vec<u8>,
    /// Bytes dropped from the front to stay under MAX_CAPTURE.
    dropped: usize,
    /// When the last output arrived.
    last: Option<Instant>,
}

impl Captured {
    fn append(&mut self, chunk: &[u8]) {
        let head = HEAD_CAPTURE.saturating_sub(self.head.len()).min(chunk.len());
        self.head.extend_from_slice(&chunk[..head]);
        self.bytes.extend_from_slice(chunk);
        self.last = Some(Instant::now());
        if self.bytes.len() > MAX_CAPTURE {
            self.trim(MAX_CAPTURE);
        }
    }

    /// Drops the oldest bytes to leave at most `keep`.
    fn trim(&mut self, keep: usize) {
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
enum Exit {
    Code(i32),
    Signal,
}

/// The process group of every live `Process`, with its leader's exit, so `kill_all` can end
/// them when killi is told to quit and nothing gets dropped.
static GROUPS: Mutex<BTreeMap<u32, watch::Receiver<Option<Exit>>>> = Mutex::new(BTreeMap::new());

fn groups() -> MutexGuard<'static, BTreeMap<u32, watch::Receiver<Option<Exit>>>> {
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

impl Exit {
    fn describe(self) -> String {
        match self {
            Exit::Code(code) => format!("exited with code {code}"),
            Exit::Signal => "was killed by a signal".into(),
        }
    }
}

impl Process {
    fn spawn(dir: &Path, command: &str) -> std::io::Result<Self> {
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
            .process_group(0)
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

    fn exit(&self) -> Option<Exit> {
        *self.exit.borrow()
    }

    /// Whether anything of the command still runs: its shell, or what it started and left
    /// running, such as `nohup server &`.
    fn running(&self) -> bool {
        group_running(self.pgid, self.exit().is_some())
    }

    async fn wait(&mut self, limit: Duration) -> Option<Exit> {
        let _ = tokio::time::timeout(limit, self.exit.wait_for(Option::is_some)).await;
        self.exit()
    }

    fn kill(&self) {
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

/// Foreground calls that haven't returned: a `bash` waiting for its command and a `task_output`
/// waiting for a task. `/btw` tells the model what they are and for how long, so a long wait
/// can be told from a hang.
#[derive(Default)]
pub struct Running {
    next: u32,
    list: BTreeMap<u32, Wait>,
}

struct Wait {
    what: String,
    started: Instant,
    output: Arc<Mutex<Captured>>,
}

/// Takes its call out of `Running` when the call returns or an interrupt drops it.
pub struct Waiting<'a> {
    running: &'a RefCell<Running>,
    id: u32,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.running.borrow_mut().list.remove(&self.id);
    }
}

fn waiting<'a>(ctx: &'a Context, what: String, output: Arc<Mutex<Captured>>) -> Waiting<'a> {
    let mut running = ctx.running.borrow_mut();
    running.next += 1;
    let id = running.next;
    running.list.insert(id, Wait { what, started: Instant::now(), output });
    Waiting { running: &ctx.running, id }
}

impl Running {
    /// What is in flight, one entry per call with how long it has run, how long it has been
    /// quiet and its last lines; `None` when nothing is.
    pub fn describe(&self) -> Option<String> {
        let entries: Vec<String> = self.list.values().map(Wait::describe).collect();
        (!entries.is_empty()).then(|| entries.join("\n"))
    }
}

impl Wait {
    fn describe(&self) -> String {
        let captured = self.output.lock().expect("output lock");
        let quiet = match captured.last {
            Some(at) => format!("last output {} ago", span(at.elapsed())),
            None => "no output yet".to_string(),
        };
        let from = captured.bytes.len().saturating_sub(2048);
        let recent = String::from_utf8_lossy(&captured.bytes[from..]);
        let mut lines: Vec<&str> = recent.trim_end().lines().rev().take(5).collect();
        lines.reverse();
        let lines: String = lines.iter().map(|line| format!("\n  {}", crate::llm::truncate(line, 160))).collect();
        format!("{} has run {}, {quiet}.{lines}", self.what, span(self.started.elapsed()))
    }
}

fn span(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 { format!("{secs}s") } else { format!("{}m{:02}s", secs / 60, secs % 60) }
}

/// Waits up to `limit` for a command and what it left running: its shell's exit, then until
/// its process group is empty. `nohup job &` exits at once while the job runs on, so waiting
/// for the shell alone would return at once too.
async fn wait_for_group(pgid: u32, mut exit: watch::Receiver<Option<Exit>>, limit: Duration) {
    let deadline = Instant::now() + limit;
    let _ = tokio::time::timeout(limit, exit.wait_for(Option::is_some)).await;
    while exit.borrow().is_some() && group_running(pgid, true) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The seconds of a `sleep N` that opens `command`.
fn leading_sleep(command: &str) -> Option<f64> {
    let rest = command.trim_start().strip_prefix("sleep")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let number: String = rest.trim_start().chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    number.parse().ok()
}

/// Where a command runs: the working directory, or `cwd` when given (absolute, `~/…`, or
/// relative to the working directory), which must exist.
fn run_dir(ctx: &Context, cwd: Option<&str>) -> Result<PathBuf, String> {
    let Some(cwd) = cwd.filter(|c| !c.is_empty()) else { return Ok(ctx.cwd.clone()) };
    let dir = match cwd.strip_prefix("~/") {
        Some(rest) => crate::config::home().join(rest),
        None => ctx.cwd.join(cwd),
    };
    if dir.is_dir() { Ok(dir) } else { Err(format!("cwd {cwd} is not a directory.")) }
}

pub async fn bash(ctx: &Context, args: &Value, wake: Wake<'_>) -> Output {
    let Some(command) = args.get("command").and_then(Value::as_str) else {
        return Output::error("bash needs `command`.");
    };
    let timeout = args.get("timeout").and_then(Value::as_u64).unwrap_or(DEFAULT_TIMEOUT).clamp(1, MAX_TIMEOUT);
    let background = args.get("run_in_background").and_then(Value::as_bool).unwrap_or(false);
    if let Some(secs) = leading_sleep(command).filter(|&secs| !background && secs >= MAX_SLEEP) {
        return Output::error(format!(
            "Not run: `sleep {secs}` waits the whole time even if the job ends in a second. To wait for a background task, call task_output with `wait`: it returns the moment the task ends. For anything else, loop on the condition: `until <check>; do sleep 5; done`, with a `timeout`."
        ));
    }
    let dir = match run_dir(ctx, args.get("cwd").and_then(Value::as_str)) {
        Ok(dir) => dir,
        Err(e) => return Output::error(e),
    };
    let mut process = match Process::spawn(&dir, command) {
        Ok(process) => process,
        Err(e) => return Output::error(format!("Could not start bash: {e}")),
    };
    if background {
        let id = ctx.tasks.borrow_mut().add(command, process, 0);
        return Output::ok(format!("Started background task {id}. Read its output with task_output."))
            .brief(format!("task {id}"));
    }

    let _waiting = waiting(ctx, format!("bash `{}`", first_line(command)), process.output.clone());
    let started = Instant::now();
    let exit = tokio::select! {
        biased;
        exit = process.wait(Duration::from_secs(timeout)) => exit,
        () = until(wake) => None,
    };
    let Some(exit) = exit else {
        let (text, shown) = take_output(ctx, &process, 0, false);
        let id = ctx.tasks.borrow_mut().add(command, process, shown);
        let why = if wake() {
            format!(
                "A message from the user is waiting, so it moved to background task {id} after {}s. Read it first; task_output with `wait` picks the wait up again, task_stop ends it.",
                started.elapsed().as_secs()
            )
        } else {
            format!("Still running after {timeout}s, so it moved to background task {id}. Use task_output to wait for it or read more, task_stop to end it.")
        };
        return Output::ok(format!("{text}\n[{why}]")).brief(format!("still running → task {id}"));
    };
    let failed = !matches!(exit, Exit::Code(0));
    let (mut text, shown) = take_output(ctx, &process, 0, failed);
    let lines = match text.lines().count() {
        _ if text == NO_OUTPUT => "no output".to_string(),
        1 => "1 line".to_string(),
        n => format!("{n} lines"),
    };
    let left = if process.running() { Some(ctx.tasks.borrow_mut().add(command, process, shown)) } else { None };
    let mut output = match exit {
        Exit::Code(0) => {
            if text == NO_OUTPUT {
                text = NO_OUTPUT_OK.into();
            }
            Output::ok(text).brief(lines)
        }
        other => {
            text.push_str(&format!("\n[Command {}]", other.describe()));
            Output::error(text).brief(other.describe())
        }
    };
    if let Some(id) = left {
        output.text.push_str(&format!("\n[What it started is still running, as background task {id}; task_output with `wait` returns when it ends, task_stop ends it.]"));
        output.brief.push_str(&format!(" · left task {id}"));
    }
    output
}

/// The output after `from`, cut to its last 2000 lines within 50KB, and the new read offset.
fn take_output(ctx: &Context, process: &Process, from: usize, failed: bool) -> (String, usize) {
    let captured = process.output.lock().expect("output lock");
    let start = from.saturating_sub(captured.dropped).min(captured.bytes.len());
    let lost = captured.dropped.saturating_sub(from);
    let read_to = captured.dropped + captured.bytes.len();
    if failed && from == 0 {
        let missing = lost.saturating_sub(captured.head.len());
        let overlap = captured.head.len().saturating_sub(captured.dropped).min(captured.bytes.len());
        let mut bytes = Vec::with_capacity(captured.head.len() + captured.bytes.len() - overlap);
        bytes.extend_from_slice(&captured.head);
        bytes.extend_from_slice(&captured.bytes[overlap..]);
        let text = String::from_utf8_lossy(&bytes);
        return (failed_tail(ctx, &text, missing), read_to);
    }
    let text = String::from_utf8_lossy(&captured.bytes[start..]);
    (tail(ctx, &text, lost), read_to)
}

impl Task {
    /// Lets go of a finished task's output: all of it once the model has read it, else the last
    /// megabyte. A task that ran for hours would otherwise hold up to 16 MB until the session ends.
    fn release(&mut self) {
        let Some(exit) = self.process.exit() else { return };
        if self.process.running() {
            return;
        }
        let mut captured = self.process.output.lock().expect("output lock");
        let read = self.read_to >= captured.dropped + captured.bytes.len();
        captured.trim(if read { 0 } else { FINISHED_KEEP });
        // A failed task's first read still needs the output's head for its root cause.
        if read || matches!(exit, Exit::Code(0)) {
            captured.head = Vec::new();
        }
    }
}

impl Tasks {
    /// Registers a running command; `read_to` is how much of its output was already shown. A
    /// command whose shell already exited was reported as it ended.
    fn add(&mut self, command: &str, process: Process, read_to: usize) -> u32 {
        self.next += 1;
        let reported = process.exit().is_some();
        let task = Task { command: command.to_string(), process, started: Instant::now(), read_to, reported, reminded: false };
        self.list.insert(self.next, task);
        self.next
    }

    /// Tasks still running that the model wasn't told about yet at the end of a turn, as
    /// `task N \`command\` (Ns)`. Each comes up once, so one kept on purpose isn't raised again.
    pub fn left_running(&mut self) -> Vec<String> {
        let mut left = Vec::new();
        for (id, task) in &mut self.list {
            if !task.reminded && task.process.running() {
                task.reminded = true;
                left.push(format!("task {id} `{}` ({}s)", first_line(&task.command), task.started.elapsed().as_secs()));
            }
        }
        left
    }

    /// Kills every task still running and gives their commands' first lines.
    pub fn stop_all(&mut self) -> Vec<String> {
        let mut stopped = Vec::new();
        for task in self.list.values() {
            if task.process.running() {
                task.process.kill();
                stopped.push(first_line(&task.command).to_string());
            }
        }
        stopped
    }

    /// Background tasks that finished since the last call, as reminder lines.
    pub fn newly_finished(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        for (id, task) in &mut self.list {
            task.release();
            if let Some(exit) = task.process.exit().filter(|_| !task.reported) {
                task.reported = true;
                lines.push(format!("Background task {id} (`{}`) {}.", first_line(&task.command), exit.describe()));
            }
        }
        lines
    }
}

pub async fn task_output(ctx: &Context, args: &Value, wake: Wake<'_>) -> Output {
    let Some(id) = args.get("id").and_then(Value::as_u64).map(|id| id as u32) else {
        return Output::error("task_output needs `id`.");
    };
    let wait = args.get("wait").and_then(Value::as_u64).unwrap_or(0).min(MAX_TIMEOUT);
    let (exit_rx, started, pgid, what, output) = {
        let tasks = ctx.tasks.borrow();
        let Some(task) = tasks.list.get(&id) else {
            return Output::error(format!("No background task {id}."));
        };
        let what = format!("task_output on task {id} `{}`", first_line(&task.command));
        (task.process.exit.clone(), task.started, task.process.pgid, what, task.process.output.clone())
    };
    let mut interrupted = false;
    if wait > 0 {
        let _waiting = waiting(ctx, what, output);
        tokio::select! {
            biased;
            () = wait_for_group(pgid, exit_rx, Duration::from_secs(wait)) => {}
            () = until(wake) => interrupted = true,
        }
    }
    let mut tasks = ctx.tasks.borrow_mut();
    let task = tasks.list.get_mut(&id).expect("task exists");
    let failed = task.process.exit().is_some_and(|exit| !matches!(exit, Exit::Code(0)));
    let (text, read_to) = take_output(ctx, &task.process, task.read_to, failed);
    task.read_to = read_to;
    task.release();
    let status = match task.process.exit() {
        Some(exit) => {
            task.reported = true;
            let left = if task.process.running() {
                format!("; what it started is still running ({}s)", started.elapsed().as_secs())
            } else {
                String::new()
            };
            format!("Task {id} {}{left}.", exit.describe())
        }
        None => format!("Task {id} is still running ({}s).", started.elapsed().as_secs()),
    };
    let mut body = if text == NO_OUTPUT { "(no new output)".to_string() } else { text };
    if interrupted && task.process.exit().is_none() {
        body.push_str("\n[Stopped waiting: a message from the user is waiting. Read it first; task_output with `wait` picks the wait up again.]");
    }
    Output::ok(format!("{status}\n{body}")).brief(status)
}

pub fn task_stop(ctx: &Context, args: &Value) -> Output {
    let Some(id) = args.get("id").and_then(Value::as_u64).map(|id| id as u32) else {
        return Output::error("task_stop needs `id`.");
    };
    let mut tasks = ctx.tasks.borrow_mut();
    match tasks.list.get_mut(&id) {
        Some(task) if task.process.running() => {
            task.process.kill();
            task.reminded = true;
            Output::ok(format!("Stopped task {id}."))
        }
        Some(_) => Output::ok(format!("Task {id} had already finished.")),
        None => Output::error(format!("No background task {id}.")),
    }
}

fn first_line(text: &str) -> &str {
    crate::llm::truncate(text.lines().next().unwrap_or_default(), 80)
}

/// Keeps the last 2000 lines within 50KB, saving the full output to a file when cut. Lines
/// are walked from the end, so a huge output of short lines costs no more than a small one.
fn tail(ctx: &Context, output: &str, dropped: usize) -> String {
    let output = output.trim_end_matches('\n');
    if output.is_empty() {
        return NO_OUTPUT.into();
    }
    let total = output.bytes().filter(|&b| b == b'\n').count() + 1;
    let (mut kept, mut bytes) = (0, 0);
    for line in output.rsplit('\n') {
        let next = line.len() + 1;
        if kept == MAX_LINES || (bytes + next > MAX_BYTES && kept > 0) {
            break;
        }
        bytes += next;
        kept += 1;
    }
    let start = total - kept;
    if start == 0 && dropped == 0 {
        return output.to_string();
    }
    let shown = &output[output.len() - (bytes - 1)..];
    let shown = if shown.len() > MAX_BYTES { &shown[shown.len() - floor_boundary(shown, MAX_BYTES)..] } else { shown };
    let saved = match spill(ctx, "bash", output) {
        Some(path) => format!(" Full output: {}", path.display()),
        None => String::new(),
    };
    let lost = if dropped > 0 { format!(" The first {dropped} bytes were not kept.") } else { String::new() };
    format!("[Showing lines {}-{total} of {total}.{saved}{lost}]\n{shown}", start + 1)
}

/// Keeps both ends of a failed command within the same display budget as `tail`.
/// `missing` counts only retained-output bytes that were dropped between head and tail.
fn failed_tail(ctx: &Context, output: &str, missing: usize) -> String {
    let output = output.trim_end_matches('\n');
    if output.is_empty() {
        return NO_OUTPUT.into();
    }
    let total = output.bytes().filter(|&b| b == b'\n').count() + 1;
    if total <= MAX_LINES && output.len() <= MAX_BYTES && missing == 0 {
        return output.to_string();
    }
    let saved = match spill(ctx, "bash", output) {
        Some(path) => {
            format!(" {}: {}", if missing > 0 { "Retained output" } else { "Full output" }, path.display())
        }
        None => String::new(),
    };
    let lost = if missing > 0 {
        format!(" {missing} bytes between the retained head and tail were not kept.")
    } else {
        String::new()
    };
    let separator = "\n[... output omitted ...]\n";
    let body_budget = MAX_BYTES - separator.len();
    let line_budget = MAX_LINES - 1;
    let head_line_budget = line_budget / 2;
    let head_byte_budget = body_budget / 2;
    let mut head_end = 0;
    let mut head_lines = 0;
    for line in output.split('\n') {
        let next = line.len() + usize::from(head_lines > 0);
        if head_lines == head_line_budget || head_end + next > head_byte_budget {
            break;
        }
        head_end += next;
        head_lines += 1;
    }
    if head_lines == 0 {
        head_end = head_byte_budget.min(output.len());
        while !output.is_char_boundary(head_end) {
            head_end -= 1;
        }
        head_lines = 1;
    }

    let tail_byte_budget = body_budget - head_end;
    let tail_line_budget = line_budget - head_lines;
    let mut tail_start = output.len();
    let mut tail_lines = 0;
    for line in output.rsplit('\n') {
        let next = line.len() + usize::from(tail_lines > 0);
        if tail_lines == tail_line_budget || output.len() - tail_start + next > tail_byte_budget {
            break;
        }
        tail_start -= line.len();
        if tail_lines > 0 {
            tail_start -= 1;
        }
        tail_lines += 1;
    }
    if tail_lines == 0 {
        tail_start = output.len() - floor_boundary(output, tail_byte_budget);
        tail_lines = 1;
    }
    let head = &output[..head_end];
    let tail = &output[tail_start..];
    format!(
        "[Showing the first {head_lines} and last {tail_lines} lines of {total}.{saved}{lost}]\n{head}{separator}{tail}"
    )
}

fn floor_boundary(s: &str, max: usize) -> usize {
    let mut n = max.min(s.len());
    while !s.is_char_boundary(s.len() - n) {
        n -= 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn ctx() -> Context {
        let dir = std::env::temp_dir();
        Context::new(dir.clone(), dir.join(format!("killi-spill-{}", std::process::id())))
    }

    fn never() -> bool {
        false
    }

    #[tokio::test(flavor = "current_thread")]
    async fn merges_stderr_and_reports_exit_code() {
        let out = bash(&ctx(), &json!({"command": "echo out; echo err >&2; exit 3"}), &never).await;
        assert!(out.is_error);
        assert_eq!(out.text, "out\nerr\n[Command exited with code 3]");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cwd_picks_the_directory_and_must_exist() {
        let ctx = ctx();
        let sub = ctx.cwd.join(format!("killi-cwd-{}", std::process::id()));
        fs::create_dir_all(&sub).unwrap();
        let name = sub.file_name().unwrap().to_str().unwrap();
        for cwd in [sub.to_str().unwrap(), name] {
            let out = bash(&ctx, &json!({"command": "pwd", "cwd": cwd}), &never).await;
            assert_eq!(out.text.trim(), sub.canonicalize().unwrap().to_str().unwrap());
        }
        let out = bash(&ctx, &json!({"command": "pwd", "cwd": "no-such-dir"}), &never).await;
        assert!(out.is_error);
        assert_eq!(out.text, "cwd no-such-dir is not a directory.");
        fs::remove_dir_all(sub).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_silent_success_says_so_with_its_exit_code() {
        let out = bash(&ctx(), &json!({"command": "true"}), &never).await;
        assert!(!out.is_error);
        assert_eq!(out.text, "(no output, exit code 0)");
        assert_eq!(out.brief, "no output");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_slow_command_moves_to_the_background() {
        let ctx = ctx();
        let out = bash(&ctx, &json!({"command": "echo start; sleep 2; echo done", "timeout": 1}), &never).await;
        assert!(out.text.starts_with("start\n[Still running after 1s, so it moved to background task 1."));
        let out = task_output(&ctx, &json!({"id": 1, "wait": 10}), &never).await;
        assert_eq!(out.text, "Task 1 exited with code 0.\ndone");
        assert!(ctx.tasks.borrow_mut().newly_finished().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_message_from_the_user_ends_the_wait_and_keeps_the_command_running() {
        let ctx = ctx();
        let sent = std::cell::Cell::new(false);
        let wake = || sent.get();
        let started = Instant::now();
        let args = json!({"command": "echo start; sleep 20", "timeout": 60});
        let (out, ()) = tokio::join!(bash(&ctx, &args, &wake), async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            sent.set(true);
        });
        assert!(started.elapsed() < Duration::from_secs(5), "it waited for the command");
        assert!(out.text.starts_with("start\n[A message from the user is waiting, so it moved to background task 1 after 0s."), "{}", out.text);
        assert!(ctx.tasks.borrow().list[&1].process.running());
        assert_eq!(task_stop(&ctx, &json!({"id": 1})).text, "Stopped task 1.");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_command_that_ends_first_is_not_moved_when_a_message_waits() {
        let out = bash(&ctx(), &json!({"command": "echo quick"}), &|| true).await;
        assert_eq!(out.text, "quick");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_message_from_the_user_ends_a_task_output_wait() {
        let ctx = ctx();
        bash(&ctx, &json!({"command": "sleep 20", "run_in_background": true}), &never).await;
        let sent = std::cell::Cell::new(false);
        let wake = || sent.get();
        let started = Instant::now();
        let args = json!({"id": 1, "wait": 60});
        let (out, ()) = tokio::join!(task_output(&ctx, &args, &wake), async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            sent.set(true);
        });
        assert!(started.elapsed() < Duration::from_secs(5), "it waited for the task");
        assert!(out.text.starts_with("Task 1 is still running"), "{}", out.text);
        assert!(out.text.contains("[Stopped waiting: a message from the user is waiting."), "{}", out.text);
        task_stop(&ctx, &json!({"id": 1}));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn background_tasks_report_when_they_finish() {
        let ctx = ctx();
        let out = bash(&ctx, &json!({"command": "exit 4", "run_in_background": true}), &never).await;
        assert_eq!(out.text, "Started background task 1. Read its output with task_output.");
        let mut exit = ctx.tasks.borrow().list[&1].process.exit.clone();
        exit.wait_for(Option::is_some).await.unwrap();
        assert_eq!(ctx.tasks.borrow_mut().newly_finished(), vec!["Background task 1 (`exit 4`) exited with code 4.".to_string()]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn task_stop_kills_the_process_group() {
        let ctx = ctx();
        bash(&ctx, &json!({"command": "sleep 30 & sleep 30", "run_in_background": true}), &never).await;
        assert_eq!(task_stop(&ctx, &json!({"id": 1})).text, "Stopped task 1.");
        let out = task_output(&ctx, &json!({"id": 1, "wait": 5}), &never).await;
        assert!(out.text.starts_with("Task 1 was killed by a signal."), "{}", out.text);
    }

    async fn until_stopped(ctx: &Context, id: u32) -> bool {
        for _ in 0..100 {
            if !ctx.tasks.borrow().list[&id].process.running() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[tokio::test(flavor = "current_thread")]
    async fn what_a_finished_command_leaves_running_becomes_a_task_raised_once() {
        let ctx = ctx();
        let out = bash(&ctx, &json!({"command": "nohup sleep 30 >/dev/null 2>&1 &"}), &never).await;
        assert_eq!(out.text, "(no output, exit code 0)\n[What it started is still running, as background task 1; task_output with `wait` returns when it ends, task_stop ends it.]");
        assert_eq!(out.brief, "no output · left task 1");
        assert!(ctx.tasks.borrow_mut().newly_finished().is_empty());
        let out = task_output(&ctx, &json!({"id": 1}), &never).await;
        assert!(out.text.starts_with("Task 1 exited with code 0; what it started is still running ("), "{}", out.text);
        let left = ctx.tasks.borrow_mut().left_running();
        assert_eq!(left.len(), 1);
        assert!(left[0].starts_with("task 1 `nohup sleep 30 >/dev/null 2>&1 &` ("), "{left:?}");
        assert!(ctx.tasks.borrow_mut().left_running().is_empty());
        assert_eq!(task_stop(&ctx, &json!({"id": 1})).text, "Stopped task 1.");
        assert!(until_stopped(&ctx, 1).await);
        assert_eq!(task_stop(&ctx, &json!({"id": 1})).text, "Task 1 had already finished.");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_command_that_leaves_nothing_running_is_not_kept() {
        let ctx = ctx();
        let out = bash(&ctx, &json!({"command": "sleep 0.1 & wait; echo hi"}), &never).await;
        assert_eq!(out.text, "hi");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(ctx.tasks.borrow_mut().left_running().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closing_the_session_kills_what_still_runs() {
        let ctx = ctx();
        bash(&ctx, &json!({"command": "nohup sleep 30 >/dev/null 2>&1 &"}), &never).await;
        bash(&ctx, &json!({"command": "sleep 30", "run_in_background": true}), &never).await;
        let pgids: Vec<u32> = ctx.tasks.borrow().list.values().map(|task| task.process.pgid).collect();
        assert_eq!(ctx.tasks.borrow_mut().stop_all(), vec!["nohup sleep 30 >/dev/null 2>&1 &".to_string(), "sleep 30".to_string()]);
        assert!(until_stopped(&ctx, 1).await && until_stopped(&ctx, 2).await);
        drop(ctx);
        assert!(pgids.iter().all(|pgid| !groups().contains_key(pgid)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waiting_on_a_task_waits_for_what_it_left_running() {
        let ctx = ctx();
        bash(&ctx, &json!({"command": "nohup sleep 1 >/dev/null 2>&1 &"}), &never).await;
        let started = Instant::now();
        let out = task_output(&ctx, &json!({"id": 1, "wait": 20}), &never).await;
        assert!(out.text.starts_with("Task 1 exited with code 0.\n"), "{}", out.text);
        assert!(started.elapsed() >= Duration::from_millis(800), "returned before the job ended");
        assert!(started.elapsed() < Duration::from_secs(10), "waited past the job's end");
        assert!(!ctx.tasks.borrow().list[&1].process.running());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn waiting_on_a_task_gives_up_at_its_limit() {
        let ctx = ctx();
        bash(&ctx, &json!({"command": "nohup sleep 30 >/dev/null 2>&1 &"}), &never).await;
        let out = task_output(&ctx, &json!({"id": 1, "wait": 1}), &never).await;
        assert!(out.text.starts_with("Task 1 exited with code 0; what it started is still running ("), "{}", out.text);
        ctx.tasks.borrow_mut().stop_all();
    }

    #[test]
    fn only_a_long_leading_sleep_is_refused() {
        assert_eq!(leading_sleep("sleep 270; tail log"), Some(270.0));
        assert_eq!(leading_sleep("  sleep 45&&echo done"), Some(45.0));
        assert_eq!(leading_sleep("sleep 0.5"), Some(0.5));
        assert_eq!(leading_sleep("echo start; sleep 40"), None);
        assert_eq!(leading_sleep("sleepy 40"), None);
        assert_eq!(leading_sleep("sleep"), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_long_fixed_sleep_does_not_run() {
        let out = bash(&ctx(), &json!({"command": "sleep 270; tail /tmp/log"}), &never).await;
        assert!(out.is_error);
        assert!(out.text.starts_with("Not run: `sleep 270` waits the whole time"), "{}", out.text);
        assert!(out.text.contains("task_output"), "{}", out.text);
        let out = bash(&ctx(), &json!({"command": "sleep 0.1; echo ok"}), &never).await;
        assert_eq!(out.text, "ok");
        let ctx = ctx();
        let out = bash(&ctx, &json!({"command": "sleep 30", "run_in_background": true}), &never).await;
        assert!(!out.is_error, "{}", out.text);
        ctx.tasks.borrow_mut().stop_all();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_call_in_flight_says_how_long_it_ran_and_what_it_printed() {
        let ctx = ctx();
        assert_eq!(ctx.running.borrow().describe(), None);
        let command = json!({"command": "echo one; echo two; sleep 2", "timeout": 10});
        let (_, seen) = tokio::join!(bash(&ctx, &command, &never), async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            ctx.running.borrow().describe()
        });
        let seen = seen.expect("the command is still running");
        assert!(seen.starts_with("bash `echo one; echo two; sleep 2` has run 0s, last output 0s ago.\n  one\n  two"), "{seen}");
        assert_eq!(ctx.running.borrow().describe(), None);
    }

    #[test]
    fn long_output_keeps_the_tail() {
        let output: String = (1..=3000).map(|n| format!("{n}\n")).collect();
        let ctx = ctx();
        let text = tail(&ctx, &output, 0);
        assert!(text.starts_with("[Showing lines 1001-3000 of 3000. Full output: "));
        assert!(text.ends_with("\n3000"));
        let _ = fs::remove_dir_all(&ctx.spill_dir);
    }

    #[test]
    fn failed_output_keeps_the_head_and_tail_within_budget_and_utf8() {
        let ctx = ctx();
        let output = format!("根因: first\n{}末尾: last", (1..=3000).map(|n| format!("行 {n}\n")).collect::<String>());
        let text = failed_tail(&ctx, &output, 0);
        assert!(text.starts_with("[Showing the first "), "{}", &text[..160]);
        assert!(text.contains(" lines of 3002. Full output: /"), "{}", &text[..160]);
        let body = text.split_once('\n').unwrap().1;
        assert!(body.starts_with("根因: first\n"));
        assert!(body.ends_with("末尾: last"));
        assert!(body.len() <= MAX_BYTES);
        assert!(body.lines().count() <= MAX_LINES);
        assert_eq!(failed_tail(&ctx, "first\nlast", 0), "first\nlast");
        let _ = fs::remove_dir_all(&ctx.spill_dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_command_returns_both_ends_and_the_spill_path() {
        let ctx = ctx();
        let command = "echo '根因: first'; for n in {1..2100}; do echo \"line $n\"; done; echo '末尾: last'; exit 9";
        let out = bash(&ctx, &json!({"command": command}), &never).await;
        assert!(out.is_error);
        assert!(out.text.contains("根因: first"), "{}", &out.text[..160]);
        assert!(out.text.contains("末尾: last\n[Command exited with code 9]"));
        assert!(out.text.contains("Full output: /"), "{}", &out.text[..200]);
        let _ = fs::remove_dir_all(&ctx.spill_dir);
    }

    #[test]
    fn failed_output_cuts_a_wide_line_at_utf8_boundaries() {
        let ctx = ctx();
        let output = "界".repeat(MAX_BYTES);
        let text = failed_tail(&ctx, &output, 0);
        assert!(text.contains("[... output omitted ...]"));
        assert!(text.split_once('\n').unwrap().1.len() <= MAX_BYTES);
        let _ = fs::remove_dir_all(&ctx.spill_dir);
    }

    #[test]
    fn the_tail_is_cut_by_lines_or_by_bytes_whichever_comes_first() {
        let ctx = ctx();
        let short = "y\n".repeat(1_000_000);
        let text = tail(&ctx, &short, 0);
        assert!(text.starts_with("[Showing lines 998001-1000000 of 1000000. Full output: "), "{}", &text[..80]);
        assert_eq!(text.lines().count(), 2001);
        let wide = format!("{}\n", "x".repeat(99)).repeat(1000);
        let text = tail(&ctx, &wide, 0);
        assert!(text.starts_with("[Showing lines 489-1000 of 1000. Full output: "), "{}", &text[..80]);
        assert_eq!(text.lines().count(), 513);
        assert_eq!(tail(&ctx, "a\nb\n", 0), "a\nb");
        let _ = fs::remove_dir_all(&ctx.spill_dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_job_holding_the_pipe_does_not_hold_the_command() {
        let ctx = ctx();
        let started = Instant::now();
        let out = bash(&ctx, &json!({"command": "echo started; (sleep 1; echo late) &", "timeout": 20}), &never).await;
        assert!(started.elapsed() < Duration::from_secs(5), "waited for the job's pipe: {:?}", started.elapsed());
        assert!(out.text.starts_with("started\n[What it started is still running, as background task 1"), "{}", out.text);
        let mut seen = String::new();
        for _ in 0..100 {
            seen.push_str(&task_output(&ctx, &json!({"id": 1}), &never).await.text);
            if seen.contains("late") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the job's later output was not captured: {seen}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_finished_task_keeps_only_what_the_model_has_not_read() {
        let ctx = ctx();
        for _ in 0..2 {
            bash(&ctx, &json!({"command": "head -c 3000000 /dev/zero | tr '\\0' a", "run_in_background": true}), &never).await;
        }
        let out = task_output(&ctx, &json!({"id": 1, "wait": 30}), &never).await;
        assert!(out.text.starts_with("Task 1 exited with code 0."), "{}", out.text);
        let mut exit = ctx.tasks.borrow().list[&2].process.exit.clone();
        exit.wait_for(Option::is_some).await.unwrap();
        ctx.tasks.borrow_mut().newly_finished();
        let held = |id: u32| {
            let tasks = ctx.tasks.borrow();
            let captured = tasks.list[&id].process.output.lock().unwrap();
            (captured.bytes.len(), captured.dropped)
        };
        assert_eq!(held(1), (0, 3_000_000), "read output is let go");
        assert_eq!(held(2), (FINISHED_KEEP, 3_000_000 - FINISHED_KEEP), "unread output keeps its last megabyte");
        let out = task_output(&ctx, &json!({"id": 2}), &never).await;
        assert!(out.text.contains(&format!("The first {} bytes were not kept.", 3_000_000 - FINISHED_KEEP)), "{}", &out.text[..120]);
        assert_eq!(held(2).0, 0);
        assert_eq!(task_output(&ctx, &json!({"id": 1}), &never).await.text, "Task 1 exited with code 0.\n(no new output)");
        let _ = fs::remove_dir_all(&ctx.spill_dir);
    }
}
