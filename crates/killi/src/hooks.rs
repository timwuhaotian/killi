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
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::tools::{OwnSession, Tracked, track};

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
    /// Text the hooks asked to add to the model's context.
    pub context: Vec<String>,
    /// Commands of hooks that never ran: timed out or failed to start. Staying fail-open,
    /// the agent shows one notice per hook instead of blocking.
    pub failures: Vec<String>,
}

impl HooksConfig {
    pub fn is_empty(&self) -> bool {
        self.0.values().all(Vec::is_empty)
    }

    /// Whether any hook could see a tool call: reads run side by side only when none could.
    pub fn has_tool_hooks(&self) -> bool {
        ["PreToolUse", "PostToolUse"].iter().any(|event| self.0.get(*event).is_some_and(|hooks| !hooks.is_empty()))
    }

    /// Runs every hook for `event` whose matcher fits `tool`, in order.
    pub async fn run(&self, event: &str, tool: Option<&str>, cwd: &Path, session: &str, mut payload: Value) -> Outcome {
        let mut outcome = Outcome::default();
        let Some(hooks) = self.0.get(event) else { return outcome };
        payload["hook_event_name"] = event.into();
        payload["session_id"] = session.into();
        payload["cwd"] = cwd.display().to_string().into();
        let input = payload.to_string();
        for hook in hooks {
            if !matches(hook.matcher.as_deref(), tool) {
                continue;
            }
            let timeout = Duration::from_secs(hook.timeout.unwrap_or(60));
            let Some((code, stdout, stderr)) = execute(&hook.command, cwd, &input, timeout).await else {
                outcome.failures.push(hook.command.clone());
                continue;
            };
            if code == 2 {
                let reason = if stderr.trim().is_empty() { format!("blocked by hook `{}`", hook.command) } else { stderr.trim().to_string() };
                outcome.block.get_or_insert(reason);
                continue;
            }
            if code != 0 {
                continue;
            }
            match serde_json::from_str::<Value>(stdout.trim()) {
                Ok(answer) if answer.is_object() => {
                    let specific = &answer["hookSpecificOutput"];
                    if answer["decision"] == "block" || specific["permissionDecision"] == "deny" || answer["continue"] == false {
                        let reason = answer["reason"]
                            .as_str()
                            .or(specific["permissionDecisionReason"].as_str())
                            .or(answer["stopReason"].as_str())
                            .unwrap_or("blocked by a hook");
                        outcome.block.get_or_insert(reason.to_string());
                    }
                    if let Some(context) = specific["additionalContext"].as_str().filter(|c| !c.is_empty()) {
                        outcome.context.push(context.to_string());
                    }
                }
                _ if !stdout.trim().is_empty() && matches!(event, "UserPromptSubmit" | "SessionStart") => {
                    outcome.context.push(stdout.trim().to_string());
                }
                _ => {}
            }
        }
        outcome
    }
}

fn matches(matcher: Option<&str>, tool: Option<&str>) -> bool {
    let (Some(matcher), Some(tool)) = (matcher.filter(|m| !m.is_empty() && *m != "*"), tool) else { return true };
    regex::RegexBuilder::new(&format!("^(?:{matcher})$")).case_insensitive(true).build().is_ok_and(|re| re.is_match(tool))
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
        Self { pgid: id, _tracked: id.map(track) }
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

async fn execute(command: &str, cwd: &Path, input: &str, timeout: Duration) -> Option<(i32, String, String)> {
    let mut child = spawn_hook(command, cwd).ok()?;
    let mut group = Group::new(child.id());
    let mut stdin = child.stdin.take();
    // The input is written beside the wait, under the same timeout: a hook that never reads it
    // would otherwise block the write, and the agent, for as long as the hook runs.
    let feed = async move {
        if let Some(stdin) = stdin.as_mut() {
            let _ = stdin.write_all(input.as_bytes()).await;
        }
    };
    let (_, output) = tokio::time::timeout(timeout, async { tokio::join!(feed, child.wait_with_output()) }).await.ok()?;
    let output = output.ok()?;
    group.pgid = None;
    group._tracked = None;
    Some((
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
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
mod tests {
    use super::*;

    fn config(toml_text: &str) -> HooksConfig {
        #[derive(Deserialize)]
        struct Wrapper {
            hooks: HooksConfig,
        }
        toml::from_str::<Wrapper>(toml_text).unwrap().hooks
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exit_code_two_blocks_with_stderr() {
        let hooks = config("[[hooks.PreToolUse]]\nmatcher = \"bash\"\ncommand = \"grep -q 'rm -rf' && echo 'no rm -rf' >&2 && exit 2; exit 0\"\n");
        let cwd = std::env::temp_dir();
        let blocked = hooks.run("PreToolUse", Some("bash"), &cwd, "s", tool_payload("bash", &json!({"command": "rm -rf /"}), None)).await;
        assert_eq!(blocked.block.as_deref(), Some("no rm -rf"));
        let allowed = hooks.run("PreToolUse", Some("bash"), &cwd, "s", tool_payload("bash", &json!({"command": "ls"}), None)).await;
        assert!(allowed.block.is_none());
        let other = hooks.run("PreToolUse", Some("read"), &cwd, "s", tool_payload("read", &json!({}), None)).await;
        assert!(other.block.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_hook_that_ignores_a_large_input_still_times_out_and_leaves_nothing_running() {
        let marker = std::env::temp_dir().join(format!("killi-hook-orphan-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let hooks = config(&format!(
            "[[hooks.PostToolUse]]\ncommand = \"(sleep 2; touch {}) & sleep 30\"\ntimeout = 1\n",
            marker.display()
        ));
        let big = "x".repeat(300_000);
        let cwd = std::env::temp_dir();
        let started = std::time::Instant::now();
        hooks.run("PostToolUse", Some("bash"), &cwd, "s", tool_payload("bash", &json!({}), Some(&big))).await;
        assert!(started.elapsed() < Duration::from_secs(5), "the timeout did not apply: {:?}", started.elapsed());
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!marker.exists(), "a process the hook started outlived its timeout");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_running_hook_is_tracked_so_kill_all_covers_it() {
        use crate::tools::registered;

        let tmp = crate::test_temp::TempDir::new("killi-hook-track");
        let mut child = spawn_hook("sleep 30", tmp.path()).unwrap();
        let pgid = child.id().unwrap();
        {
            let _group = Group::new(child.id());
            assert!(registered(pgid), "the hook's group is tracked while the hook runs");
        }
        assert!(!registered(pgid), "a finished hook unregisters its group");
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    #[test]
    fn tool_hooks_are_detected() {
        assert!(!config("[hooks]\n").has_tool_hooks());
        assert!(!config("[[hooks.SessionStart]]\ncommand = ':'\n").has_tool_hooks());
        assert!(config("[[hooks.PreToolUse]]\ncommand = ':'\n").has_tool_hooks());
        assert!(config("[[hooks.PostToolUse]]\ncommand = ':'\n").has_tool_hooks());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn json_answers_block_or_add_context() {
        let hooks = config(concat!(
            "[[hooks.UserPromptSubmit]]\ncommand = \"echo 'Today is release day.'\"\n",
            "[[hooks.Stop]]\ncommand = \"echo '{\\\"decision\\\": \\\"block\\\", \\\"reason\\\": \\\"run the tests first\\\"}'\"\n",
        ));
        let cwd = std::env::temp_dir();
        let prompt = hooks.run("UserPromptSubmit", None, &cwd, "s", json!({"prompt": "ship it"})).await;
        assert_eq!(prompt.context, vec!["Today is release day.".to_string()]);
        let stop = hooks.run("Stop", None, &cwd, "s", json!({"stop_hook_active": false})).await;
        assert_eq!(stop.block.as_deref(), Some("run the tests first"));
    }
}
