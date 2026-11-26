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
    let sentence = [". ", "! ", "; ", "。", "；", "！"]
        .iter()
        .filter_map(|mark| line.rfind(mark).map(|at| at + mark.len()))
        .max()
        .map_or(line, |at| &line[at..])
        .trim()
        .to_lowercase();
    names_a_coming_phase(&sentence)
        || opens_with_gerund(&clause)
        || opens_with_zh_step(&sentence)
        || INTENT_OPENERS.iter().any(|opener| {
            clause.strip_prefix(opener).is_some_and(|rest| {
                rest.is_empty() || !opener.is_ascii() || rest.starts_with([' ', ','])
            })
        })
}

/// Whether a lowercased last sentence, written as a fragment, says the work moves on to a
/// next phase ("Fix-first phase starting, 3 at a time.", "Phase A next: 12 resolve groups.").
/// Ultra-concise replies drop the "I'll" and the leading verb, so the opener rules miss them.
/// "next" counts only where it closes a phrase ("next:", "… next."), not in "next release";
/// a sentence that addresses the user ("you", "你") is advice to them, not the agent's plan.
fn names_a_coming_phase(sentence: &str) -> bool {
    let words: Vec<&str> = sentence
        .split(|c: char| !c.is_ascii_alphanumeric() && c != ':' && c != ',')
        .filter(|word| !word.is_empty())
        .collect();
    if words
        .iter()
        .any(|word| word.trim_end_matches([':', ',']) == "you")
        || sentence.contains('你')
    {
        return false;
    }
    let next = words.iter().enumerate().any(|(at, word)| match *word {
        "next" => at + 1 == words.len(),
        "next:" | "next," => true,
        _ => false,
    });
    next || words
        .iter()
        .any(|word| word.trim_end_matches([':', ',']) == "starting")
        || sentence.contains("接下来")
        || sentence.contains("下一步")
}

/// Whether a lowercased clause opens with an English gerund ("opening the PR",
/// "classifying the routes"): the shape of a step being started, whichever verb it is.
fn opens_with_gerund(clause: &str) -> bool {
    let clause = ["now ", "then "]
        .iter()
        .find_map(|lead| clause.strip_prefix(lead))
        .unwrap_or(clause)
        .trim_start();
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

/// How a clause in Chinese says a step is under way ("正在验证…", "正做隔离验证") or opening
/// ("最后一轮核对…"), wherever it stands in the last sentence: what follows is often the
/// reason ("正在验证…的覆盖顺序，避免误改。"), which would hide it from the last clause.
const ZH_STEPS: &[&str] = &["正在", "正做", "最后一轮"];

/// What a clause that opens on "正在" says is going on without being a step the agent takes:
/// a state the system is in ("正在运行的只有 nginx。"), as "Working now." is for the gerund.
const ZH_NOT_STEPS: &[&str] = &["正在运行", "正在等待", "正在使用", "正在占用", "正在挂起"];

/// Whether any clause of a lowercased last sentence, split at `，`, opens on a step in Chinese.
fn opens_with_zh_step(sentence: &str) -> bool {
    sentence.split('，').any(|clause| {
        let clause = clause.trim_start();
        !ZH_NOT_STEPS.iter().any(|state| clause.starts_with(state))
            && ZH_STEPS.iter().any(|step| clause.starts_with(step))
    })
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
/// the history, so it also holds for a turn interrupted before a resume. `failed_on` is the
/// error a turn stopped on when no one interrupted it.
pub(super) fn interrupted_note(
    messages: &[Value],
    todos: &[Todo],
    failed_on: Option<&str>,
) -> String {
    let (cut, after) = match failed_on {
        None => ("The user interrupted your previous turn", String::new()),
        Some(error) => {
            let error = error.split_whitespace().collect::<Vec<_>>().join(" ");
            let error = neutralize_reminder_tags(truncate(&error, 200)).into_owned();
            (
                "Your previous turn stopped on an error",
                format!("; the user didn't interrupt it (the error: {error})"),
            )
        }
    };
    let mut note = format!("{cut}{after}.");
    let prompts = crate::session::user_prompts(messages);
    let Some(request) = prompts.last() else {
        return note;
    };
    let request = request.split_whitespace().collect::<Vec<_>>().join(" ");
    let request = neutralize_reminder_tags(truncate(&request, 300)).into_owned();
    note = format!("{cut} while it worked on “{request}”{after}.");
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

impl Agent {
    /// The check a reply that would end the turn gets while the session goal stands, or None
    /// when no check is due. Only the terminal's main agent outside plan mode is checked:
    /// subagents hold no goal, `-p` doesn't enforce one a resume restored (decision 57), plan
    /// mode can't do the work, and background work
    /// still running ends the turn properly, since its report starts a turn of its own.
    pub(super) fn goal_check_note(&self) -> Option<String> {
        if self.depth > 0
            || !self.controls.interactive
            || self.controls.plan.get()
            || self.background_working()
        {
            return None;
        }
        let goal = self.controls.goal.borrow().clone()?;
        Some(crate::goal::check_note(&goal))
    }
}
