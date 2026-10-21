//! `bash`, plus the background tasks it can leave running: a command that outlives its
//! timeout keeps going in the background instead of being killed.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::watch;

use super::{Context, MAX_BYTES, MAX_LINES, Output, spill};

const DEFAULT_TIMEOUT: u64 = 60;
const MAX_TIMEOUT: u64 = 300;
/// Output beyond this is dropped from memory, oldest first; the model only ever sees the tail.
const MAX_CAPTURE: usize = 16 * 1024 * 1024;

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
    /// Bytes dropped from the front to stay under MAX_CAPTURE.
    dropped: usize,
}

#[derive(Clone, Copy, Debug)]
enum Exit {
    Code(i32),
    Signal,
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
    fn spawn(ctx: &Context, command: &str) -> std::io::Result<Self> {
        let mut child = Command::new("/bin/bash")
            .arg("-c")
            .arg(format!("exec 2>&1\n{command}"))
            .current_dir(&ctx.cwd)
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
        tokio::spawn(async move {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut captured = sink.lock().expect("output lock");
                        captured.bytes.extend_from_slice(&buf[..n]);
                        if captured.bytes.len() > MAX_CAPTURE {
                            let excess = captured.bytes.len() - MAX_CAPTURE;
                            captured.bytes.drain(..excess);
                            captured.dropped += excess;
                        }
                    }
                }
            }
            let status = child.wait().await.ok();
            let _ = tx.send(Some(match status.and_then(|s| s.code()) {
                Some(code) => Exit::Code(code),
                None => Exit::Signal,
            }));
        });
        Ok(Self { pgid, output, exit })
    }

    fn exit(&self) -> Option<Exit> {
        *self.exit.borrow()
    }

    async fn wait(&mut self, limit: Duration) -> Option<Exit> {
        let _ = tokio::time::timeout(limit, self.exit.wait_for(Option::is_some)).await;
        self.exit()
    }

    fn kill(&self) {
        if self.pgid > 0 {
            // SAFETY: killpg only sends a signal to the process group we started.
            unsafe { libc::killpg(self.pgid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

/// Kills the process group while armed, so an interrupted turn leaves nothing running.
struct KillOnDrop {
    pgid: u32,
    armed: bool,
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if self.armed && self.pgid > 0 {
            // SAFETY: killpg only sends a signal to the process group we started.
            unsafe { libc::killpg(self.pgid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

impl Drop for Tasks {
    fn drop(&mut self) {
        for task in self.list.values() {
            if task.process.exit().is_none() {
                task.process.kill();
            }
        }
    }
}

pub async fn bash(ctx: &Context, args: &Value) -> Output {
    let Some(command) = args.get("command").and_then(Value::as_str) else {
        return Output::error("bash needs `command`.");
    };
    let timeout = args.get("timeout").and_then(Value::as_u64).unwrap_or(DEFAULT_TIMEOUT).clamp(1, MAX_TIMEOUT);
    let background = args.get("run_in_background").and_then(Value::as_bool).unwrap_or(false);
    let mut process = match Process::spawn(ctx, command) {
        Ok(process) => process,
        Err(e) => return Output::error(format!("Could not start bash: {e}")),
    };
    if background {
        let id = ctx.tasks.borrow_mut().add(command, process, 0);
        return Output::ok(format!("Started background task {id}. Read its output with task_output."))
            .brief(format!("task {id}"));
    }

    let mut guard = KillOnDrop { pgid: process.pgid, armed: true };
    let exit = process.wait(Duration::from_secs(timeout)).await;
    guard.armed = false;
    let exit = match exit {
        Some(exit) => exit,
        None => {
            let (text, shown) = take_output(ctx, &process, 0);
            let id = ctx.tasks.borrow_mut().add(command, process, shown);
            return Output::ok(format!(
                "{text}\n[Still running after {timeout}s, so it moved to background task {id}. Use task_output to wait for it or read more, task_stop to end it.]"
            ))
            .brief(format!("still running → task {id}"));
        }
    };
    let (mut text, _) = take_output(ctx, &process, 0);
    let lines = match text.lines().count() {
        _ if text == "(no output)" => "no output".to_string(),
        1 => "1 line".to_string(),
        n => format!("{n} lines"),
    };
    match exit {
        Exit::Code(0) => Output::ok(text).brief(lines),
        other => {
            text.push_str(&format!("\n[Command {}]", other.describe()));
            Output::error(text).brief(other.describe())
        }
    }
}

/// The output after `from`, cut to its last 2000 lines within 50KB, and the new read offset.
fn take_output(ctx: &Context, process: &Process, from: usize) -> (String, usize) {
    let captured = process.output.lock().expect("output lock");
    let start = from.saturating_sub(captured.dropped).min(captured.bytes.len());
    let text = String::from_utf8_lossy(&captured.bytes[start..]).into_owned();
    let lost = captured.dropped.saturating_sub(from);
    (tail(ctx, &text, lost), captured.dropped + captured.bytes.len())
}

impl Tasks {
    /// Registers a running command; `read_to` is how much of its output was already shown.
    fn add(&mut self, command: &str, process: Process, read_to: usize) -> u32 {
        self.next += 1;
        let task = Task { command: command.to_string(), process, started: Instant::now(), read_to, reported: false };
        self.list.insert(self.next, task);
        self.next
    }

    /// Background tasks that finished since the last call, as reminder lines.
    pub fn newly_finished(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        for (id, task) in &mut self.list {
            if let Some(exit) = task.process.exit().filter(|_| !task.reported) {
                task.reported = true;
                lines.push(format!("Background task {id} (`{}`) {}.", first_line(&task.command), exit.describe()));
            }
        }
        lines
    }
}

pub async fn task_output(ctx: &Context, args: &Value) -> Output {
    let Some(id) = args.get("id").and_then(Value::as_u64).map(|id| id as u32) else {
        return Output::error("task_output needs `id`.");
    };
    let wait = args.get("wait").and_then(Value::as_u64).unwrap_or(0).min(MAX_TIMEOUT);
    let (mut exit_rx, started) = {
        let tasks = ctx.tasks.borrow();
        let Some(task) = tasks.list.get(&id) else {
            return Output::error(format!("No background task {id}."));
        };
        (task.process.exit.clone(), task.started)
    };
    if wait > 0 {
        let _ = tokio::time::timeout(Duration::from_secs(wait), exit_rx.wait_for(Option::is_some)).await;
    }
    let mut tasks = ctx.tasks.borrow_mut();
    let task = tasks.list.get_mut(&id).expect("task exists");
    let (text, read_to) = take_output(ctx, &task.process, task.read_to);
    task.read_to = read_to;
    let status = match task.process.exit() {
        Some(exit) => {
            task.reported = true;
            format!("Task {id} {}.", exit.describe())
        }
        None => format!("Task {id} is still running ({}s).", started.elapsed().as_secs()),
    };
    let body = if text == "(no output)" { "(no new output)".to_string() } else { text };
    Output::ok(format!("{status}\n{body}")).brief(status)
}

pub fn task_stop(ctx: &Context, args: &Value) -> Output {
    let Some(id) = args.get("id").and_then(Value::as_u64).map(|id| id as u32) else {
        return Output::error("task_stop needs `id`.");
    };
    let tasks = ctx.tasks.borrow();
    match tasks.list.get(&id) {
        Some(task) if task.process.exit().is_none() => {
            task.process.kill();
            Output::ok(format!("Stopped task {id}."))
        }
        Some(_) => Output::ok(format!("Task {id} had already finished.")),
        None => Output::error(format!("No background task {id}.")),
    }
}

fn first_line(text: &str) -> &str {
    crate::llm::truncate(text.lines().next().unwrap_or_default(), 80)
}

/// Keeps the last 2000 lines within 50KB, saving the full output to a file when cut.
fn tail(ctx: &Context, output: &str, dropped: usize) -> String {
    let output = output.trim_end_matches('\n');
    if output.is_empty() {
        return "(no output)".into();
    }
    let lines: Vec<&str> = output.split('\n').collect();
    let total = lines.len();
    let mut start = total;
    let mut bytes = 0;
    while start > 0 && total - start < MAX_LINES {
        let next = lines[start - 1].len() + 1;
        if bytes + next > MAX_BYTES && start < total {
            break;
        }
        bytes += next;
        start -= 1;
    }
    if start == 0 && dropped == 0 {
        return output.to_string();
    }
    let shown = lines[start..].join("\n");
    let shown = if shown.len() > MAX_BYTES { &shown[shown.len() - floor_boundary(&shown, MAX_BYTES)..] } else { &shown };
    let saved = match spill(ctx, "bash", output) {
        Some(path) => format!(" Full output: {}", path.display()),
        None => String::new(),
    };
    let lost = if dropped > 0 { format!(" The first {dropped} bytes were not kept.") } else { String::new() };
    format!("[Showing lines {}-{total} of {total}.{saved}{lost}]\n{shown}", start + 1)
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

    fn ctx() -> Context {
        let dir = std::env::temp_dir();
        Context::new(dir.clone(), dir.join(format!("killi-spill-{}", std::process::id())))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn merges_stderr_and_reports_exit_code() {
        let out = bash(&ctx(), &json!({"command": "echo out; echo err >&2; exit 3"})).await;
        assert!(out.is_error);
        assert_eq!(out.text, "out\nerr\n[Command exited with code 3]");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_slow_command_moves_to_the_background() {
        let ctx = ctx();
        let out = bash(&ctx, &json!({"command": "echo start; sleep 2; echo done", "timeout": 1})).await;
        assert!(out.text.starts_with("start\n[Still running after 1s, so it moved to background task 1."));
        let out = task_output(&ctx, &json!({"id": 1, "wait": 10})).await;
        assert_eq!(out.text, "Task 1 exited with code 0.\ndone");
        assert!(ctx.tasks.borrow_mut().newly_finished().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn background_tasks_report_when_they_finish() {
        let ctx = ctx();
        let out = bash(&ctx, &json!({"command": "exit 4", "run_in_background": true})).await;
        assert_eq!(out.text, "Started background task 1. Read its output with task_output.");
        let mut exit = ctx.tasks.borrow().list[&1].process.exit.clone();
        exit.wait_for(Option::is_some).await.unwrap();
        assert_eq!(ctx.tasks.borrow_mut().newly_finished(), vec!["Background task 1 (`exit 4`) exited with code 4.".to_string()]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn task_stop_kills_the_process_group() {
        let ctx = ctx();
        bash(&ctx, &json!({"command": "sleep 30 & sleep 30", "run_in_background": true})).await;
        assert_eq!(task_stop(&ctx, &json!({"id": 1})).text, "Stopped task 1.");
        let out = task_output(&ctx, &json!({"id": 1, "wait": 5})).await;
        assert!(out.text.starts_with("Task 1 was killed by a signal."), "{}", out.text);
    }

    #[test]
    fn long_output_keeps_the_tail() {
        let output: String = (1..=3000).map(|n| format!("{n}\n")).collect();
        let text = tail(&ctx(), &output, 0);
        assert!(text.starts_with("[Showing lines 1001-3000 of 3000. Full output: "));
        assert!(text.ends_with("\n3000"));
    }
}
