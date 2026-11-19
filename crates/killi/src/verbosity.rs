//! Reply verbosity (`/verbosity`, `[agent] verbosity`): how long the model's text to the user
//! runs. Like plan and swarm mode, it reaches the model as appended reminders, so the system
//! prompt stays as it is. Ultra-concise repeats its reminder after every prompt, since a rule said
//! once fades over a long session.

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Verbosity {
    #[default]
    Normal,
    /// The fewest words that keep the meaning.
    UltraConcise,
}

impl Verbosity {
    pub const ALL: [Verbosity; 2] = [Verbosity::Normal, Verbosity::UltraConcise];

    pub fn name(self) -> &'static str {
        match self {
            Verbosity::Normal => "normal",
            Verbosity::UltraConcise => "ultra-concise",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Verbosity::Normal => "replies at their usual length",
            Verbosity::UltraConcise => "the fewest words that keep the meaning",
        }
    }

    pub fn parse(name: &str) -> Option<Verbosity> {
        Verbosity::ALL.into_iter().find(|verbosity| verbosity.name() == name)
    }
}

pub const ON: &str = "<system-reminder>Ultra-concise mode is on. Use the fewest words that keep the meaning: if 20 words say it, don't use 50; if 50, don't use 100. Lead with the answer. Drop greetings, hedges, restating, transitions, adjectives, background and side details. Keep conclusions, decisions, numbers, names, paths, commands, code and errors exact. Fragments and short lists are fine. Tables and lists count too: a few words per cell or item, no sentence where a phrase does, the shortest unambiguous path, and each point said once. When the user's latest message asks for detail, give it this once. This covers only your text to the user, not files, commits, plans or subagent prompts.</system-reminder>";

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
        for verbosity in Verbosity::ALL {
            assert_eq!(Verbosity::parse(verbosity.name()), Some(verbosity));
        }
        assert_eq!(Verbosity::parse("default"), None);
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
