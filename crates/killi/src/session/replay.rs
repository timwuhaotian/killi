//! What a resumed session shows again: the prompts, the model's text and each tool call.

use super::*;

pub const COMPACTED_MARK: &str = "<system-reminder>This conversation was compacted";

/// What the user typed, oldest first: reminders killi added are left out, and a compaction's
/// summary gives back the requests it kept.
pub fn user_prompts(messages: &[Value]) -> Vec<String> {
    messages
        .iter()
        .filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"].as_str())
        .flat_map(text_prompts)
        .collect()
}

/// The prompts in one user message: a compaction's summary gives back the requests it
/// kept, anything else is the typed text without its reminders.
pub(super) fn text_prompts(text: &str) -> Vec<String> {
    if text.starts_with(COMPACTED_MARK) {
        let mut prompts = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("<request>\n") {
            let body = &rest[start + 10..];
            let end = body.find("\n</request>").unwrap_or(body.len());
            prompts.extend(typed(&body[..end]));
            rest = &body[end..];
        }
        prompts
    } else {
        typed(text).into_iter().collect()
    }
}

/// One thing a resumed session shows again, in the order it happened.
#[derive(Debug, PartialEq)]
pub enum Replayed {
    Note(String),
    Prompt(String),
    Text(String),
    Tool {
        summary: String,
        output: String,
        is_error: bool,
        brief: String,
    },
}

/// The conversation as the terminal showed it, for a resumed session: prompts, the model's
/// text and each tool call with its result. Only the last `keep` prompts are shown, with a
/// note for the earlier ones. Logs from before tool results were kept with how they ended
/// get a best guess from the result's text.
pub fn replay(loaded: &Loaded, cwd: &Path, keep: usize) -> Vec<Replayed> {
    let results: HashMap<&str, &str> = loaded
        .messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| Some((m["tool_call_id"].as_str()?, m["content"].as_str()?)))
        .collect();
    let mut shown = Vec::new();
    for message in &loaded.messages {
        match message["role"].as_str() {
            Some("user") => {
                let Some(text) = message["content"].as_str() else {
                    continue;
                };
                if text.starts_with(COMPACTED_MARK) {
                    shown.push(Replayed::Note(
                        "The conversation before this was compacted into a summary.".into(),
                    ));
                    shown.extend(
                        user_prompts(std::slice::from_ref(message))
                            .into_iter()
                            .map(Replayed::Prompt),
                    );
                } else if let Some(prompt) = typed(text) {
                    shown.push(Replayed::Prompt(prompt));
                }
            }
            Some("assistant") => {
                if let Some(text) = message["content"].as_str().filter(|t| !t.trim().is_empty()) {
                    shown.push(Replayed::Text(text.to_string()));
                }
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    let name = call["function"]["name"].as_str().unwrap_or_default();
                    let args = serde_json::from_str::<Value>(
                        call["function"]["arguments"].as_str().unwrap_or_default(),
                    )
                    .unwrap_or_default();
                    let id = call["id"].as_str().unwrap_or_default();
                    let output = results
                        .get(id)
                        .copied()
                        .unwrap_or("(no result)")
                        .to_string();
                    let (is_error, brief) = match loaded.tools.get(id) {
                        Some(end) => (end.is_error, end.brief.clone()),
                        None => guess_end(&output),
                    };
                    shown.push(Replayed::Tool {
                        summary: crate::tools::summary(name, &args, cwd),
                        output,
                        is_error,
                        brief,
                    });
                }
            }
            _ => {}
        }
    }
    let prompts = shown
        .iter()
        .filter(|r| matches!(r, Replayed::Prompt(_)))
        .count();
    if prompts > keep {
        let first = shown
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, Replayed::Prompt(_)))
            .nth(prompts - keep)
            .map_or(0, |(i, _)| i);
        shown.drain(..first);
        let earlier = prompts - keep;
        shown.insert(
            0,
            Replayed::Note(format!(
                "{earlier} earlier prompt{} not shown.",
                if earlier == 1 { "" } else { "s" }
            )),
        );
    }
    shown
}

/// How a result ended, for logs that didn't keep it: a command that failed says so at the end.
pub(super) fn guess_end(output: &str) -> (bool, String) {
    let failed = output
        .lines()
        .last()
        .is_some_and(|l| l.starts_with("[Command "))
        || output.starts_with("Interrupted by the user");
    let lines = output.lines().count();
    match (failed, lines) {
        _ if output.starts_with("(no output") => (false, "no output".into()),
        (true, _) => (
            true,
            output
                .lines()
                .last()
                .unwrap_or_default()
                .trim_matches(['[', ']'])
                .to_string(),
        ),
        (false, 0 | 1) => (false, String::new()),
        (false, n) => (false, format!("{n} lines")),
    }
}

/// What the user typed for a message killi sent as `text`: without the reminders before it or
/// the files it named with `@` after it, which a compaction may have cut before their end, and
/// a skill run as the `/name request` that started it. `None` if it is only reminders.
fn typed(text: &str) -> Option<String> {
    let prompt = strip_reminders(text)?;
    let mut prompt = crate::skills::typed(prompt).unwrap_or_else(|| prompt.to_string());
    if let Some(at) = prompt.find("\n\n<system-reminder>") {
        prompt.truncate(at);
    }
    Some(prompt)
}

/// A user message without the reminders in front of it, or `None` if it is only reminders.
pub(super) fn strip_reminders(text: &str) -> Option<&str> {
    let mut rest = text;
    while let Some(after) = rest.strip_prefix("<system-reminder>") {
        let end = after.find("</system-reminder>")?;
        rest = after[end + 18..].trim_start();
    }
    (!rest.is_empty()).then_some(rest)
}
