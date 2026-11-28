//! The reminders about the task list `todo_write` keeps: mid-turn when it goes stale, at a
//! turn's end while items are open, and after a compaction took the calls that built it.

use super::*;

/// Tool calls the task list may go unchanged for with an item open before the model is
/// reminded of it mid-turn; otherwise a long turn can finish every item with the screen
/// still showing the first one in progress.
pub(super) const STALE_TASKS_AFTER: usize = 10;

/// Told to the agent mid-turn when its task list hasn't changed in `calls` tool calls with
/// items still open. None when the list is empty or finished.
pub(super) fn stale_tasks_note(todos: &[Todo], calls: usize) -> Option<String> {
    let current = todos
        .iter()
        .find(|t| t.status == "in_progress")
        .or_else(|| todos.iter().find(|t| t.status != "completed"))?;
    Some(format!(
        "<system-reminder>Your task list hasn't changed in {calls} tool calls; `{}` still shows as the current item. If it or others are finished, call todo_write now with them completed and the next one in_progress; if not, carry on.</system-reminder>",
        truncate(&current.content, 120)
    ))
}

/// Told to the agent when it ends a turn with its task list unfinished, which shows on the
/// user's screen as work still going: finished items are marked done, or the rest is carried on.
pub(super) fn open_tasks_note(todos: &[Todo]) -> Option<String> {
    let open: Vec<&str> = todos
        .iter()
        .filter(|t| t.status != "completed")
        .map(|t| t.content.as_str())
        .collect();
    if open.is_empty() {
        return None;
    }
    let list = open.join("; ");
    Some(format!(
        "<system-reminder>Your task list still has {} open: {}. Call todo_write now with what is finished marked completed (and dropped items removed). If work is left, do it. Don't repeat your last reply.</system-reminder>",
        open.len(),
        truncate(&list, 400)
    ))
}

/// Characters of one task shown in the note.
const TASK_CHARS: usize = 200;

/// The task list as `todo_write` last set it, for a model that no longer has the calls that
/// built it. None when nothing is open: a finished list needs no carrying.
pub(super) fn tasks_note(todos: &[Todo]) -> Option<String> {
    let open = todos.iter().filter(|t| t.status != "completed").count();
    if open == 0 {
        return None;
    }
    let list: String = todos
        .iter()
        .enumerate()
        .map(|(at, todo)| {
            let content = todo
                .content
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "\n{}. [{}] {}",
                at + 1,
                todo.status,
                truncate(&content, TASK_CHARS)
            )
        })
        .collect();
    Some(format!(
        "Your task list as todo_write last set it, {open} open:{list}\nThe user still sees this list: work from it and keep it current with todo_write."
    ))
}
