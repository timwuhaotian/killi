use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::{Context, MAX_BYTES, MAX_LINES, Output, spill};

const DEFAULT_TIMEOUT: u64 = 60;
const MAX_TIMEOUT: u64 = 300;
/// Output beyond this is dropped from memory; the model only ever sees the tail.
const MAX_CAPTURE: usize = 16 * 1024 * 1024;

pub async fn bash(ctx: &Context, args: &Value) -> Output {
    let Some(command) = args.get("command").and_then(Value::as_str) else {
        return Output::error("bash needs `command`.");
    };
    let timeout = args.get("timeout").and_then(Value::as_u64).unwrap_or(DEFAULT_TIMEOUT).clamp(1, MAX_TIMEOUT);

    let mut child = match Command::new("/bin/bash")
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
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return Output::error(format!("Could not start bash: {e}")),
    };
    let pgid = child.id();
    let mut stdout = child.stdout.take().expect("stdout is piped");

    let mut captured = Vec::new();
    let mut dropped = 0usize;
    let mut buf = [0u8; 16 * 1024];
    let read_all = async {
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) if captured.len() + n <= MAX_CAPTURE => captured.extend_from_slice(&buf[..n]),
                Ok(n) => {
                    let excess = captured.len() + n - MAX_CAPTURE;
                    captured.drain(..excess.min(captured.len()));
                    captured.extend_from_slice(&buf[..n]);
                    dropped += excess;
                }
            }
        }
        child.wait().await
    };
    let status = match tokio::time::timeout(Duration::from_secs(timeout), read_all).await {
        Ok(status) => status.ok(),
        Err(_) => {
            if let Some(pgid) = pgid {
                unsafe { libc::killpg(pgid as libc::pid_t, libc::SIGKILL) };
            }
            None
        }
    };

    let output = String::from_utf8_lossy(&captured);
    let mut text = tail(ctx, &output, dropped);
    match status {
        Some(status) if status.success() => Output::ok(text),
        Some(status) => {
            let code = status.code().map_or("killed by a signal".to_string(), |c| format!("exited with code {c}"));
            text.push_str(&format!("\n[Command {code}]"));
            Output::error(text)
        }
        None => {
            text.push_str(&format!("\n[Command timed out after {timeout}s and was killed]"));
            Output::error(text)
        }
    }
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
        Context { cwd: dir.clone(), spill_dir: dir.join(format!("killi-spill-{}", std::process::id())) }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn merges_stderr_and_reports_exit_code() {
        let out = bash(&ctx(), &json!({"command": "echo out; echo err >&2; exit 3"})).await;
        assert!(out.is_error);
        assert_eq!(out.text, "out\nerr\n[Command exited with code 3]");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timeout_kills_the_process_group() {
        let out = bash(&ctx(), &json!({"command": "echo start; sleep 30 & sleep 30", "timeout": 1})).await;
        assert!(out.text.starts_with("start\n[Command timed out after 1s"));
    }

    #[test]
    fn long_output_keeps_the_tail() {
        let output: String = (1..=3000).map(|n| format!("{n}\n")).collect();
        let text = tail(&ctx(), &output, 0);
        assert!(text.starts_with("[Showing lines 1001-3000 of 3000. Full output: "));
        assert!(text.ends_with("\n3000"));
    }
}
