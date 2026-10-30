//! `bash`, plus the background tasks it can leave running: a command that outlives its
//! timeout keeps going in the background instead of being killed, and so does what a finished
//! command left running in its process group, such as `nohup server &`. A `Process` kills its
//! group when dropped, so an interrupted turn or a closed session leaves nothing behind.
//! A call that waits stops waiting when the user sends a message, so a long wait never keeps
//! the model from reading it.

mod process;
mod running;
mod tasks;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Context, MAX_BYTES, MAX_LINES, Output, spill};
#[cfg(test)]
use process::groups;
use process::{Exit, Process, wait_for_group};
pub use process::{OwnSession, Tracked, kill_all, track};
pub use running::Running;
use running::waiting;
pub use tasks::Tasks;

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

#[cfg(test)]
pub(crate) fn registered(pgid: u32) -> bool {
    groups().contains_key(&pgid)
}

/// The seconds of a `sleep N` that opens `command`.
fn leading_sleep(command: &str) -> Option<f64> {
    let rest = command.trim_start().strip_prefix("sleep")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let number: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    number.parse().ok()
}

/// Where a command runs: the working directory, or `cwd` when given (absolute, `~/…`, or
/// relative to the working directory), which must exist.
fn run_dir(ctx: &Context, cwd: Option<&str>) -> Result<PathBuf, String> {
    let Some(cwd) = cwd.filter(|c| !c.is_empty()) else {
        return Ok(ctx.cwd.clone());
    };
    let dir = match cwd.strip_prefix("~/") {
        Some(rest) => crate::config::home().join(rest),
        None => ctx.cwd.join(cwd),
    };
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(format!("cwd {cwd} is not a directory."))
    }
}

pub async fn bash(ctx: &Context, args: &Value, wake: Wake<'_>) -> Output {
    let Some(command) = args.get("command").and_then(Value::as_str) else {
        return Output::error("bash needs `command`.");
    };
    let timeout = super::uint(args, "timeout")
        .unwrap_or(DEFAULT_TIMEOUT)
        .clamp(1, MAX_TIMEOUT);
    let background = args
        .get("run_in_background")
        .and_then(Value::as_bool)
        .unwrap_or(false);
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
        return Output::ok(format!(
            "Started background task {id}. Read its output with task_output."
        ))
        .brief(format!("task {id}"));
    }

    let _waiting = waiting(
        ctx,
        format!("bash `{}`", first_line(command)),
        process.output.clone(),
    );
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
            format!(
                "Still running after {timeout}s, so it moved to background task {id}. Use task_output to wait for it or read more, task_stop to end it."
            )
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
    let left = if process.running() {
        Some(ctx.tasks.borrow_mut().add(command, process, shown))
    } else {
        None
    };
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
    let start = from
        .saturating_sub(captured.dropped)
        .min(captured.bytes.len());
    let lost = captured.dropped.saturating_sub(from);
    let read_to = captured.dropped + captured.bytes.len();
    if failed && from == 0 {
        let missing = lost.saturating_sub(captured.head.len());
        let overlap = captured
            .head
            .len()
            .saturating_sub(captured.dropped)
            .min(captured.bytes.len());
        let mut bytes = Vec::with_capacity(captured.head.len() + captured.bytes.len() - overlap);
        bytes.extend_from_slice(&captured.head);
        bytes.extend_from_slice(&captured.bytes[overlap..]);
        let text = String::from_utf8_lossy(&bytes);
        return (failed_tail(ctx, &text, missing), read_to);
    }
    let text = String::from_utf8_lossy(&captured.bytes[start..]);
    (tail(ctx, &text, lost), read_to)
}

pub async fn task_output(ctx: &Context, args: &Value, wake: Wake<'_>) -> Output {
    let Some(id) = super::uint(args, "id").map(|id| id as u32) else {
        return Output::error("task_output needs `id`.");
    };
    let wait = super::uint(args, "wait").unwrap_or(0).min(MAX_TIMEOUT);
    let (exit_rx, started, pgid, what, output) = {
        let tasks = ctx.tasks.borrow();
        let Some(task) = tasks.list.get(&id) else {
            return Output::error(format!("No background task {id}."));
        };
        let what = format!("task_output on task {id} `{}`", first_line(&task.command));
        (
            task.process.exit.clone(),
            task.started,
            task.process.pgid,
            what,
            task.process.output.clone(),
        )
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
    let failed = task
        .process
        .exit()
        .is_some_and(|exit| !matches!(exit, Exit::Code(0)));
    let (text, read_to) = take_output(ctx, &task.process, task.read_to, failed);
    task.read_to = read_to;
    task.release();
    let status = match task.process.exit() {
        Some(exit) => {
            task.reported = true;
            let left = if task.process.running() {
                format!(
                    "; what it started is still running ({}s)",
                    started.elapsed().as_secs()
                )
            } else {
                String::new()
            };
            format!("Task {id} {}{left}.", exit.describe())
        }
        None => format!(
            "Task {id} is still running ({}s).",
            started.elapsed().as_secs()
        ),
    };
    let mut body = if text == NO_OUTPUT {
        "(no new output)".to_string()
    } else {
        text
    };
    if interrupted && task.process.exit().is_none() {
        body.push_str("\n[Stopped waiting: a message from the user is waiting. Read it first; task_output with `wait` picks the wait up again.]");
    }
    Output::ok(format!("{status}\n{body}")).brief(status)
}

pub fn task_stop(ctx: &Context, args: &Value) -> Output {
    let Some(id) = super::uint(args, "id").map(|id| id as u32) else {
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
    let shown = if shown.len() > MAX_BYTES {
        &shown[shown.len() - floor_boundary(shown, MAX_BYTES)..]
    } else {
        shown
    };
    let saved = match spill(ctx, "bash", output) {
        Some(path) => format!(" Full output: {}", path.display()),
        None => String::new(),
    };
    let lost = if dropped > 0 {
        format!(" The first {dropped} bytes were not kept.")
    } else {
        String::new()
    };
    format!(
        "[Showing lines {}-{total} of {total}.{saved}{lost}]\n{shown}",
        start + 1
    )
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
            format!(
                " {}: {}",
                if missing > 0 {
                    "Retained output"
                } else {
                    "Full output"
                },
                path.display()
            )
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
mod tests;
