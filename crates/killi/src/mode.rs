//! Working modes (`/mode`, `[agent] mode`): how the session uses subagents. In `focus`, the
//! default, an `agent` or `agent_swarm` call runs in the foreground and moves to the background
//! only when the user sends a message meanwhile. In `orchestrate` every such call goes to the
//! background at once and the main agent keeps talking with the user, coordinating the reports
//! as they come in. Like plan and swarm mode, the mode reaches the model as appended
//! reminders, so the system prompt and tool list stay as they are.

use serde::Deserialize;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    Focus,
    /// Every subagent runs in the background; the main agent coordinates.
    Orchestrate,
}

impl Mode {
    pub const ALL: [Mode; 2] = [Mode::Focus, Mode::Orchestrate];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Focus => "focus",
            Mode::Orchestrate => "orchestrate",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Mode::Focus => {
                "subagents run in the foreground; a message you send moves them to the background"
            }
            Mode::Orchestrate => {
                "every subagent runs in the background while you keep talking to the main agent"
            }
        }
    }

    pub fn parse(name: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|mode| mode.name() == name)
    }
}

pub const ON: &str = "<system-reminder>Orchestrate mode is on. Every agent and agent_swarm call now runs in the background: it returns a task id at once, so the user can keep talking to you while it works. Each report arrives later as a reminder. Delegate substantial work and answer quick questions yourself. Give each subagent a self-contained piece that doesn't overlap another's files. An agent call without `fork` inherits this conversation. Use agent_message to give a running subagent new information, task_stop to stop one and task_output to read one early. Once you have dispatched work, tell the user in a sentence what is running and end your turn rather than waiting for it.</system-reminder>";

pub const OFF: &str = "<system-reminder>Orchestrate mode is off. agent and agent_swarm calls run in the foreground again and return their reports. Background subagents still running keep going and report as before.</system-reminder>";

/// Whether the log's last working-mode reminder turned orchestrate on, so a resumed session
/// knows whether switching it off needs saying.
pub fn last_orchestrate_state(messages: &[serde_json::Value]) -> bool {
    messages
        .iter()
        .rev()
        .filter(|message| message["role"] == "user")
        .find_map(|message| match message["content"].as_str()? {
            ON => Some(true),
            OFF => Some(false),
            _ => None,
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for mode in Mode::ALL {
            assert_eq!(Mode::parse(mode.name()), Some(mode));
        }
        assert_eq!(Mode::parse("normal"), None);
        assert_eq!(Mode::default(), Mode::Focus);
    }
}
