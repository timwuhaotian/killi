//! What a resumed session rebuilds from its log beyond the messages themselves.

use super::*;

/// How `todo_write`'s result starts when it set the list.
const TODOS_UPDATED: &str = "Task list updated:";

/// The folder the resumed session ran in, from its log's `session` entry: resuming from
/// another folder keeps running here, so the mismatch is worth a notice. Old logs
/// carry no `cwd` and stay silent.
pub(super) fn logged_cwd(log: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(log).ok()?;
    for chunk in bytes.split(|b| *b == b'\n') {
        if chunk.is_empty() {
            continue;
        }
        let entry: Value = serde_json::from_str(std::str::from_utf8(chunk).ok()?).ok()?;
        if entry["type"] == "session"
            && let Some(cwd) = entry["cwd"].as_str().filter(|cwd| !cwd.is_empty())
        {
            return Some(PathBuf::from(cwd));
        }
    }
    None
}

impl Agent {
    /// Takes up the task list the log left, and shows it: the terminal's panel would
    /// otherwise stay empty until the model next writes the list. The last `todo_write` still
    /// in `messages` decides; without one, `kept` is the list the last compaction recorded.
    pub(super) fn restore_todos(&mut self, messages: &[Value], kept: Option<&Value>) {
        self.todos = todos_from(messages)
            .or_else(|| kept.and_then(|todos| serde_json::from_value(todos.clone()).ok()))
            .unwrap_or_default();
        if !self.todos.is_empty() {
            (self.sink)(Event::Todos(self.todos.clone()));
        }
    }
}

/// The task list the last successful `todo_write` in `messages` left, so a resumed session
/// keeps checking its open items. A call the tool refused, or one with no result, set nothing;
/// None when no call in `messages` set the list.
fn todos_from(messages: &[Value]) -> Option<Vec<Todo>> {
    let updated = |id: &str| {
        messages.iter().any(|message| {
            message["role"] == "tool"
                && message["tool_call_id"] == id
                && message["content"]
                    .as_str()
                    .is_some_and(|text| text.starts_with(TODOS_UPDATED))
        })
    };
    messages
        .iter()
        .rev()
        .filter(|message| message["role"] == "assistant")
        .flat_map(|message| message["tool_calls"].as_array().into_iter().flatten().rev())
        .filter(|call| call["function"]["name"] == "todo_write")
        .find_map(|call| {
            let id = call["id"].as_str()?;
            if !updated(id) {
                return None;
            }
            let args: Value = serde_json::from_str(call["function"]["arguments"].as_str()?).ok()?;
            serde_json::from_value(args["todos"].clone()).ok()
        })
}
