//! Hooks: your commands, run at points in the agent's work, with Claude Code's protocol. The
//! event arrives as JSON on stdin; exit code 2 blocks and its stderr goes to the model, and a
//! JSON answer on stdout can block or add context. Configured in TOML:
//!
//! ```toml
//! [[hooks.PreToolUse]]
//! matcher = "bash|edit"   # a regex over the tool name; leave it out to match every tool
//! command = "~/bin/check.sh"
//! timeout = 60
//! ```

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::tools::{OwnSession, Tracked, track};

/// Bytes of a hook's stdout or stderr kept; the rest is read and dropped, so a chatty hook
/// neither grows memory nor blocks on a full pipe.
const OUTPUT_CAP: usize = 64 * 1024;

/// Bytes of one hook's text that reach the model, as context or a block reason.
const MODEL_TEXT_BYTES: usize = 16 * 1024;

/// How long the pipes may stay open after the hook exits: something it started in the
/// background (`afplay x &`) holds them, and the hook's answer is already in.
const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(200);

#[cfg(test)]
thread_local! {
    /// Tests flip this instead of the environment, so one test's override never leaks
    /// into another test running on a different thread.
    static NO_HOOKS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Hooks are off for the whole process when `KILLI_NO_HOOKS` is set: every event runs
/// nothing, and the agent behaves as if no hooks were configured.
pub fn disabled() -> bool {
    #[cfg(test)]
    if NO_HOOKS.with(|v| v.get()) {
        return true;
    }
    std::env::var_os("KILLI_NO_HOOKS").is_some()
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(transparent)]
pub struct HooksConfig(BTreeMap<String, Vec<Hook>>);

#[derive(Debug, Deserialize, Clone)]
pub struct Hook {
    pub matcher: Option<String>,
    pub command: String,
    pub timeout: Option<u64>,
}

/// What the hooks for one event decided.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Set when a hook blocked: the reason, for the model or the user.
    pub block: Option<String>,
    /// Set when a hook answered `"continue": false`: the reason it gave. On `Stop` and
    /// `SubagentStop` it ends the turn; on other events it blocks like `block`.
    pub halt: Option<String>,
    /// Text the hooks asked to add to the model's context.
    pub context: Vec<String>,
    /// Commands of hooks that never ran: timed out or failed to start. Staying fail-open,
    /// the agent shows one notice per hook instead of blocking.
    pub failures: Vec<String>,
}

impl HooksConfig {
    pub fn is_empty(&self) -> bool {
        disabled() || self.0.values().all(Vec::is_empty)
    }

    /// Whether any hook could see a tool call: reads run side by side only when none could.
    pub fn has_tool_hooks(&self) -> bool {
        !disabled()
            && ["PreToolUse", "PostToolUse"]
                .iter()
                .any(|event| self.0.get(*event).is_some_and(|hooks| !hooks.is_empty()))
    }

    /// Runs every hook for `event` whose matcher fits `tool`, in order.
    pub async fn run(
        &self,
        event: &str,
        tool: Option<&str>,
        cwd: &Path,
        session: &str,
        mut payload: Value,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        if disabled() {
            return outcome;
        }
        let Some(hooks) = self.0.get(event) else {
            return outcome;
        };
        payload["hook_event_name"] = event.into();
        payload["session_id"] = session.into();
        payload["cwd"] = cwd.display().to_string().into();
        let input = payload.to_string();
        for hook in hooks {
            if !matches(hook.matcher.as_deref(), tool) {
                continue;
            }
            let timeout = Duration::from_secs(hook.timeout.unwrap_or(60));
            let Some((code, stdout, stderr)) = execute(&hook.command, cwd, &input, timeout).await
            else {
                outcome.failures.push(hook.command.clone());
                continue;
            };
            if code == 2 {
                let reason = if stderr.trim().is_empty() {
                    format!("blocked by hook `{}`", hook.command)
                } else {
                    for_model(stderr.trim())
                };
                outcome.block.get_or_insert(reason);
                continue;
            }
            if code != 0 {
                continue;
            }
            match serde_json::from_str::<Value>(stdout.trim()) {
                Ok(answer) if answer.is_object() => {
                    let specific = &answer["hookSpecificOutput"];
                    let halt = answer["continue"] == false;
                    if halt {
                        let reason = answer["stopReason"]
                            .as_str()
                            .or(answer["reason"].as_str())
                            .unwrap_or("no reason given");
                        outcome.halt.get_or_insert(for_model(reason));
                    }
                    // On a stop event `block` means "keep going", the opposite of a halt.
                    let stop_event = matches!(event, "Stop" | "SubagentStop");
                    if answer["decision"] == "block"
                        || specific["permissionDecision"] == "deny"
                        || (halt && !stop_event)
                    {
                        let reason = answer["reason"]
                            .as_str()
                            .or(specific["permissionDecisionReason"].as_str())
                            .or(answer["stopReason"].as_str())
                            .unwrap_or("blocked by a hook");
                        outcome.block.get_or_insert(for_model(reason));
                    }
                    if let Some(context) = specific["additionalContext"]
                        .as_str()
                        .filter(|c| !c.is_empty())
                    {
                        outcome.context.push(for_model(context));
                    }
                }
                _ if !stdout.trim().is_empty()
                    && matches!(event, "UserPromptSubmit" | "SessionStart") =>
                {
                    outcome.context.push(for_model(stdout.trim()));
                }
                _ => {}
            }
        }
        outcome
    }
}

/// A hook's text as the model gets it: cut at `MODEL_TEXT_BYTES`, saying so.
fn for_model(text: &str) -> String {
    let kept = crate::llm::truncate(text, MODEL_TEXT_BYTES);
    if kept.len() == text.len() {
        return text.to_string();
    }
    format!(
        "{kept}\n[hook output cut at {} of {} bytes]",
        kept.len(),
        text.len()
    )
}

fn matches(matcher: Option<&str>, tool: Option<&str>) -> bool {
    let (Some(matcher), Some(tool)) = (matcher.filter(|m| !m.is_empty() && *m != "*"), tool) else {
        return true;
    };
    regex::RegexBuilder::new(&format!("^(?:{matcher})$"))
        .case_insensitive(true)
        .build()
        .is_ok_and(|re| re.is_match(tool))
}

/// Ends a hook's whole process group when dropped, unless it finished: a timeout or an
/// interrupted turn would otherwise kill only the shell and leave what it started running.
/// The group is tracked while the hook runs, so quitting on a signal ends it too.
struct Group {
    pgid: Option<u32>,
    _tracked: Option<Tracked>,
}

impl Group {
    fn new(id: Option<u32>) -> Self {
        Self {
            pgid: id,
            _tracked: id.map(track),
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(pgid) = self.pgid {
            // SAFETY: killpg only signals the process group this hook was started in.
            unsafe { libc::killpg(pgid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

/// Starts a hook with its input piped, in its own session so its group can be killed whole.
fn spawn_hook(command: &str, cwd: &Path) -> std::io::Result<tokio::process::Child> {
    let command = match command.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", crate::config::home().display()),
        None => command.to_string(),
    };
    Command::new("/bin/sh")
        .arg("-c")
        .arg(&command)
        .current_dir(cwd)
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("KILLI_PROJECT_DIR", cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .own_session()
        .kill_on_drop(true)
        .spawn()
}

/// Runs a hook to its exit and returns its exit code, stdout and stderr, or `None` when it
/// timed out or could not run. The hook's own exit ends the wait: what it left running in the
/// background may hold the pipes open, so they get only `DRAIN_AFTER_EXIT` more.
async fn execute(
    command: &str,
    cwd: &Path,
    input: &str,
    timeout: Duration,
) -> Option<(i32, String, String)> {
    let mut child = spawn_hook(command, cwd).ok()?;
    let mut group = Group::new(child.id());
    let mut stdin = child.stdin.take();
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = {
        // The input is written beside the wait, under the same timeout: a hook that never
        // reads it would otherwise block the write, and the agent, for as long as it runs.
        let io = async {
            let feed = async {
                if let Some(stdin) = stdin.as_mut() {
                    let _ = stdin.write_all(input.as_bytes()).await;
                }
                drop(stdin);
            };
            tokio::join!(feed, drain(stdout, &mut out), drain(stderr, &mut err))
        };
        let wait = child.wait();
        tokio::pin!(io, wait);
        tokio::time::timeout(timeout, async {
            tokio::select! {
                status = &mut wait => {
                    let _ = tokio::time::timeout(DRAIN_AFTER_EXIT, &mut io).await;
                    status
                }
                _ = &mut io => wait.await,
            }
        })
        .await
    };
    let status = status.ok()?.ok()?;
    group.pgid = None;
    group._tracked = None;
    Some((
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    ))
}

/// Reads `pipe` to its end into `kept`, keeping at most `OUTPUT_CAP` bytes.
async fn drain(pipe: Option<impl AsyncRead + Unpin>, kept: &mut Vec<u8>) {
    let Some(mut pipe) = pipe else {
        return;
    };
    let mut buf = [0; 8192];
    while let Ok(read @ 1..) = pipe.read(&mut buf).await {
        let room = OUTPUT_CAP.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..read.min(room)]);
    }
}

/// The payload for tool events, in Claude Code's shape.
pub fn tool_payload(name: &str, input: &Value, response: Option<&str>) -> Value {
    let mut payload = json!({"tool_name": name, "tool_input": input});
    if let Some(response) = response {
        payload["tool_response"] = response.into();
    }
    payload
}

#[cfg(test)]
mod tests;
