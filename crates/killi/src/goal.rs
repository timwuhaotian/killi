//! Session goals (`/goal`): one user-set task the session can't end until it is done or
//! cleared. Like plan and swarm mode, the goal reaches the model as an appended reminder,
//! so the system prompt and tool list stay as they are.

/// Told to the model on the step after `/goal <text>`: the goal stands until the user ends it.
pub fn set_note(goal: &str) -> String {
    format!(
        "<system-reminder>Session goal: {goal}\nThis goal stands until the user ends it with /goal done or /goal clear. Break it into todo_write items and keep that list current; don't claim it is done until every part is finished and verified. If it turns out truly impossible, say what blocks it and what would unblock it, then wait for the user — only they can clear it.</system-reminder>"
    )
}

/// Told to the model on the step after the user ends the goal, so it stops pursuing it.
pub fn end_note(goal: &str) -> String {
    format!(
        "<system-reminder>The session goal is over and no longer stands: {goal}\nCarry on with what the user asks next.</system-reminder>"
    )
}

/// Times one turn may be held open by `check_note`. The check comes back only after tool
/// calls, so a reply that answers it without working ends the turn sooner.
pub const MAX_CHECKS: u32 = 3;

/// Told when a reply would end the turn while the goal stands: the turn's request may be
/// answered with the goal still short, and the user would otherwise have to ask again.
pub fn check_note(goal: &str) -> String {
    format!(
        "<system-reminder>The session goal still stands: {goal}\nBefore this turn ends, check it against the goal itself, not only the last request. Do exactly one: if any part is unfinished or unverified, carry on now with tool calls, without asking whether to; if every part is done, prove it (if this turn already shows the proof, say so in one line instead of checking again) and tell the user to confirm with /goal done; if it is blocked, needs a decision only the user can make, or the user's latest message said to stop or wait, say so in one line and stop. Don't repeat your last reply.</system-reminder>"
    )
}

/// What `/goal` shows when no goal is set.
pub const EMPTY: &str = "No session goal. Set one with /goal <text>.";

const REFUSALS: &[(&str, &str)] = &[
    ("Quitting", "quit"),
    ("Starting a new session", "start a new one"),
    ("Resuming another session", "resume one"),
];

/// Why `action` won't run while `goal` stands: ending the session waits for `/goal done`,
/// and only the impossible or unwanted is `/goal clear`ed. None when no goal stands.
pub fn blocks(action: &str, goal: Option<String>) -> Option<String> {
    let goal = goal?;
    let verb = REFUSALS
        .iter()
        .find(|(prefix, _)| *prefix == action)
        .map(|(_, verb)| *verb)
        .unwrap_or("leave");
    Some(format!(
        "Session goal still open: {goal}\n{action} waits until the goal is 100% done (/goal done). {verb_cap} only when it is truly impossible or no longer wanted (/goal clear).",
        verb_cap = capitalize(verb)
    ))
}

fn capitalize(verb: &str) -> String {
    let mut chars = verb.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_note_carries_the_goal_and_how_it_ends() {
        let note = set_note("ship v2");
        assert!(note.contains("Session goal: ship v2"), "{note}");
        assert!(
            note.contains("/goal done") && note.contains("/goal clear"),
            "{note}"
        );
        assert!(note.starts_with("<system-reminder>") && note.ends_with("</system-reminder>"));
    }

    #[test]
    fn check_note_restates_the_goal_and_its_three_endings() {
        let note = check_note("ship v2");
        assert!(note.contains("still stands: ship v2"), "{note}");
        assert!(
            note.contains("carry on now")
                && note.contains("/goal done")
                && note.contains("blocked"),
            "{note}"
        );
        assert!(note.starts_with("<system-reminder>") && note.ends_with("</system-reminder>"));
    }

    #[test]
    fn end_note_names_the_goal_that_stopped() {
        let note = end_note("ship v2");
        assert!(note.contains("ship v2"), "{note}");
        assert!(note.contains("no longer stands"), "{note}");
        assert!(note.starts_with("<system-reminder>") && note.ends_with("</system-reminder>"));
    }

    #[test]
    fn nothing_blocks_an_exit_without_a_goal() {
        assert_eq!(blocks("Quitting", None), None);
    }

    #[test]
    fn an_open_goal_blocks_each_way_out_by_name() {
        for (action, verb) in [
            ("Quitting", "Quit"),
            ("Starting a new session", "Start"),
            ("Resuming another session", "Resume"),
        ] {
            let note = blocks(action, Some("ship v2".into())).expect("an open goal blocks");
            assert!(note.contains("ship v2"), "{note}");
            assert!(
                note.contains("/goal done") && note.contains("/goal clear"),
                "{note}"
            );
            assert!(note.contains(verb), "{note}");
        }
    }
}
