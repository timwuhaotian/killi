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
            Mode::Focus => "subagents run in the foreground; a message you send moves them to the background",
            Mode::Orchestrate => "every subagent runs in the background while you keep talking to the main agent",
        }
    }

    pub fn parse(name: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|mode| mode.name() == name)
    }
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
