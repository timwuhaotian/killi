//! How a turn ended, read from its last reply or its interruption, and what the model is told
//! about it: a reply that stops where the work was meant to start is asked to go on once, and
//! the prompt after an interruption names the work that was cut off.

use super::*;

/// Told once when a reply ends on a colon with no tool call after it: the sentence promised a
/// lookup or a list and the turn ended before giving it.
pub(super) const UNFINISHED: &str = "<system-reminder>Your reply ends with a colon, so something was meant to follow it, but nothing did. If you meant to look something up or run something, do it now with a tool call. If you had nothing more to say, answer in one short sentence. Don't repeat your last reply.</system-reminder>";

/// Told once when a short reply ends by saying what it will do next ("Let me check that.")
/// with no tool call after it: the turn ended where the work was meant to start.
pub(super) const ANNOUNCED: &str = "<system-reminder>Your reply says what you will do next, but the turn ended without a tool call. If you meant to do it, do it now with a tool call. If it is already done or nothing more is needed, answer in one short sentence. Don't repeat your last reply.</system-reminder>";

/// Told once when a turn that a skill started would end on a short reply before any tool
/// call: a skill is a procedure, so its turn ending untouched is a stop, in any language.
pub(super) const UNSTARTED: &str = "<system-reminder>You were asked to use a skill, and the turn ended before any of its steps ran. If the request means them to run now, start them with a tool call. If it asked you to wait, or nothing more is needed, answer in one short sentence. Don't repeat your last reply.</system-reminder>";

/// Whether the reply ends by asking the user something: its last non-empty line ends with
/// `?` (or the full-width `？` of Chinese and Japanese replies). An open task list isn't re-announced then; the question already waits for the user.
pub(super) fn ends_with_question(text: &str) -> bool {
    last_line_ends_with(text, &['?', '？'])
}

/// Whether the reply ends on a colon (or the full-width `：`): it announces something, a lookup
/// or a list, that the turn stopped before giving.
pub(super) fn ends_with_colon(text: &str) -> bool {
    last_line_ends_with(text, &[':', '：'])
}

/// The notice and the reminder for a reply that stops where it promised more: one that ends
/// on a colon, a short one whose last sentence announces a next step, or a short one that ends
/// a skill's turn before `skill_unstarted` says any tool ran.
pub(super) fn unfinished(
    text: &str,
    skill_unstarted: bool,
) -> Option<(&'static str, &'static str)> {
    if ends_with_colon(text) {
        Some((
            "The reply ended on a colon with nothing after it. Asked the agent to go on.",
            UNFINISHED,
        ))
    } else if ends_with_intent(text) {
        Some((
            "The reply announced a next step and stopped. Asked the agent to go on.",
            ANNOUNCED,
        ))
    } else if skill_unstarted && is_short(text) && !ends_with_question(text) {
        Some((
            "The skill's turn ended before any step ran. Asked the agent to go on.",
            UNSTARTED,
        ))
    } else {
        None
    }
}

/// Replies longer than this are taken as answers even when they close on a next step: a
/// summary that ends "I'll keep the old flag for now." is done, a one-liner that says only
/// "Checking web-core #1601." is not.
const INTENT_MAX_CHARS: usize = 200;

fn is_short(text: &str) -> bool {
    text.trim().chars().count() <= INTENT_MAX_CHARS
}

/// How a sentence that announces the next step starts, lowercased, besides an English gerund
/// (`opens_with_gerund`).
const INTENT_OPENERS: &[&str] = &[
    "let me",
    "let's",
    "i'll",
    "i will",
    "i'm going to",
    "now i'll",
    "next i'll",
    "on it",
    "我来",
    "让我",
    "我先",
    "我去",
    "接下来我",
];

/// Words ending in -ing that open a clause without announcing a step: a state ("Failing on
/// CI.", "Pending review.", "Working now."), a hold for the user ("Waiting for your go-ahead."),
/// a choice to leave things as they are ("Leaving it running.") or not a verb at all. Words
/// ending in -thing ("Nothing else.") are left out too.
const NOT_STEPS: &[&str] = &[
    "according",
    "assuming",
    "awaiting",
    "bring",
    "deferring",
    "during",
    "evening",
    "existing",
    "failing",
    "holding",
    "keeping",
    "leaving",
    "matching",
    "missing",
    "morning",
    "outstanding",
    "passing",
    "pausing",
    "pending",
    "remaining",
    "skipping",
    "standing",
    "stopping",
    "string",
    "using",
    "waiting",
    "working",
];

/// Whether a short reply ends by announcing what it will do next ("Got it. Let me ground those
/// numbers.", "On it — opening the PR.") rather than answering.
/// "Let me know …" closes an answer and doesn't count.
pub(super) fn ends_with_intent(text: &str) -> bool {
    let text = text.trim();
    if !is_short(text) || ends_with_question(text) {
        return false;
    }
    let Some(line) = text.lines().rev().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let line = line.trim_end_matches(['.', '。', '…', '!', '！', ' ']);
    let clause = [
        ". ", "! ", "; ", ": ", "— ", "– ", "。", "；", "：", "！", "，",
    ]
    .iter()
    .filter_map(|mark| line.rfind(mark).map(|at| at + mark.len()))
    .max()
    .map_or(line, |at| &line[at..])
    .trim()
    .to_lowercase();
    if clause.starts_with("let me know") {
        return false;
    }
    opens_with_gerund(&clause)
        || INTENT_OPENERS.iter().any(|opener| {
            clause.strip_prefix(opener).is_some_and(|rest| {
                rest.is_empty() || !opener.is_ascii() || rest.starts_with([' ', ','])
            })
        })
}

/// Whether a lowercased clause opens with an English gerund ("opening the PR",
/// "classifying the routes"): the shape of a step being started, whichever verb it is.
fn opens_with_gerund(clause: &str) -> bool {
    let word = clause
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or_default();
    let rest = &clause[word.len()..];
    word.len() >= 5
        && word.ends_with("ing")
        && !word.ends_with("thing")
        && !NOT_STEPS.contains(&word)
        && (rest.is_empty() || rest.starts_with([' ', ',']))
}

fn last_line_ends_with(text: &str, marks: &[char]) -> bool {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim_end().ends_with(marks))
}

/// The note the prompt after an interrupted turn carries: which request the turn was working
/// on and what its task list still had open. Without them a model reads the new prompt as
/// replacing the work it cut off, rewrites the task list and never comes back to it. Read from
/// the history, so it also holds for a turn interrupted before a resume.
pub(super) fn interrupted_note(messages: &[Value], todos: &[Todo]) -> String {
    let mut note = "The user interrupted your previous turn.".to_string();
    let prompts = crate::session::user_prompts(messages);
    let Some(request) = prompts.last() else {
        return note;
    };
    let request = request.split_whitespace().collect::<Vec<_>>().join(" ");
    let request = neutralize_reminder_tags(truncate(&request, 300)).into_owned();
    note = format!("The user interrupted your previous turn while it worked on “{request}”.");
    let open: Vec<&str> = todos
        .iter()
        .filter(|t| t.status != "completed")
        .map(|t| t.content.as_str())
        .collect();
    if !open.is_empty() {
        let list = neutralize_reminder_tags(truncate(&open.join("; "), 400)).into_owned();
        note.push_str(&format!(
            " Its task list still has {} open: {list}.",
            open.len()
        ));
    }
    note.push_str(" That work isn't finished. Do what the message below asks first; unless it drops or replaces the interrupted request, carry on with what is left of it afterwards, keeping its open items on your task list until they are done.");
    note
}
