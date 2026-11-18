//! Reply modes (`/mode`, `[agent] mode`): how long the model's text to the user runs. Like
//! plan and swarm mode, a mode reaches the model as appended reminders, so the system prompt
//! stays as it is. Ultra-concise repeats its reminder after every prompt, since a rule said
//! once fades over a long session.

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    Normal,
    /// The fewest words that keep the meaning.
    UltraConcise,
}

impl Mode {
    pub const ALL: [Mode; 2] = [Mode::Normal, Mode::UltraConcise];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::UltraConcise => "ultra-concise",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Mode::Normal => "replies at their usual length",
            Mode::UltraConcise => "the fewest words that keep the meaning",
        }
    }

    pub fn parse(name: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|mode| mode.name() == name)
    }
}

pub const ON: &str = "<system-reminder>Ultra-concise mode is on. Use the fewest words that keep the meaning: if 20 words say it, don't use 50; if 50, don't use 100. Lead with the answer. Drop greetings, hedges, restating, transitions, adjectives, background and side details. Keep conclusions, decisions, numbers, names, paths, commands, code and errors exact. Fragments and short lists are fine. When the user's latest message asks for detail, give it this once. This covers only your text to the user, not files, commits, plans or subagent prompts.</system-reminder>";

pub const OFF: &str =
    "<system-reminder>Ultra-concise mode is off. Answer at your usual length.</system-reminder>";

/// Whether the log's last mode reminder turned ultra-concise on, so a resumed session knows
/// whether switching it off needs saying.
pub fn last_concise_state(messages: &[Value]) -> bool {
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
    use serde_json::json;

    #[test]
    fn names_round_trip() {
        for mode in Mode::ALL {
            assert_eq!(Mode::parse(mode.name()), Some(mode));
        }
        assert_eq!(Mode::parse("default"), None);
    }

    #[test]
    fn last_state_follows_the_latest_reminder() {
        let user = |text: &str| json!({"role": "user", "content": text});
        assert!(!last_concise_state(&[]));
        assert!(last_concise_state(&[user("hi"), user(ON)]));
        assert!(!last_concise_state(&[user(ON), user("hi"), user(OFF)]));
        assert!(last_concise_state(&[user(OFF), user(ON), user("next")]));
    }
}
