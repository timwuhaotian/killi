//! What a log holds, replayed from its records, and the repair a resume makes to a history
//! the log left open.

use super::*;

use serde::{Deserialize, Serialize};
use serde_json::json;

/// What a log holds, replayed.
pub struct Loaded {
    pub messages: Vec<Value>,
    /// How each tool call ended, by call id, for logs that kept it.
    pub tools: HashMap<String, ToolEnd>,
    /// Input tokens of the last request: how full the context was when the log stopped.
    pub context: u64,
    /// The model and effort the session last ran on.
    pub choice: Choice,
    /// The session goal `/goal` last set, if any.
    pub goal: Option<String>,
}

/// A model alias and thinking effort, as `Thinking::label` names it. The effort is what a
/// session log last recorded; `last-model.json` writes only the model.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// What `load_reported` found besides the conversation.
#[allow(dead_code)]
pub struct LoadReport {
    pub loaded: Loaded,
    /// Non-empty log lines skipped: undecodable UTF-8 or torn JSON.
    pub skipped: usize,
    /// Placeholder tool results inserted for mid-history calls the log never answered.
    pub repaired: usize,
}

/// `load`, also reporting how many lines were skipped and how many dangling tool calls were
/// repaired. Callers that keep showing `Loaded` are unaffected.
pub fn load_reported(path: &Path) -> Result<LoadReport> {
    // Decoded per line, so one bad-UTF-8 line is skipped instead of losing the whole log.
    // Splitting bytes on `\n` is safe: it never appears inside a multibyte UTF-8 sequence.
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let mut skipped = 0;
    let mut loaded = Loaded { messages: Vec::new(), tools: HashMap::new(), context: 0, choice: Choice::default(), goal: None };
    for chunk in bytes.split(|b| *b == b'\n') {
        if chunk.is_empty() {
            continue;
        }
        let Ok(line) = std::str::from_utf8(chunk) else {
            skipped += 1;
            continue;
        };
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            skipped += 1;
            continue;
        };
        match entry["type"].as_str() {
            Some("session" | "model") => {
                loaded.choice = Choice {
                    model: entry["model"].as_str().map(String::from),
                    effort: entry["effort"].as_str().map(String::from),
                };
            }
            Some("goal") => {
                loaded.goal = entry["goal"].as_str().map(String::from);
            }
            Some("message") => {
                if let (Some(id), Some(tool)) = (entry["message"]["tool_call_id"].as_str(), entry.get("tool")) {
                    let end = ToolEnd {
                        is_error: tool["is_error"].as_bool().unwrap_or(false),
                        brief: tool["brief"].as_str().unwrap_or_default().to_string(),
                    };
                    loaded.tools.insert(id.to_string(), end);
                }
                loaded.messages.push(entry["message"].clone());
            }
            Some("usage") => loaded.context = entry["prompt"].as_u64().unwrap_or(0),
            Some("compaction") => {
                loaded.messages = entry["messages"].as_array().cloned().unwrap_or_default();
                loaded.context = 0;
            }
            _ => {}
        }
    }
    let repaired = repair_dangling_calls(&mut loaded.messages);
    if loaded.messages.first().is_none_or(|m| m["role"] != "system") {
        bail!("{} has no system prompt; it isn't a killi session", path.display());
    }
    Ok(LoadReport { loaded, skipped, repaired })
}

/// A placeholder tool result, so a resumed session sends a history the provider accepts.
const DANGLING_TOOL_RESULT: &str = "(no result: the session stopped before this tool call returned)";

/// Gives every mid-history assistant tool call the log never answered a placeholder result
/// right after its calls, and returns how many were inserted. A call at the very end is left
/// alone: resuming runs it. This is a load-time repair only; a resume that wants it kept in
/// the log records the repaired messages as a compaction-kind entry, the way the media
/// recovery in `agent.rs` records its rewrite.
fn repair_dangling_calls(messages: &mut Vec<Value>) -> usize {
    let mut answered: HashSet<&str> = messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| m["tool_call_id"].as_str())
        .collect();
    let mut pending: Vec<(usize, Vec<Value>)> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if message["role"] != "assistant" || index + 1 >= messages.len() {
            continue;
        }
        let mut placeholders = Vec::new();
        for call in message["tool_calls"].as_array().into_iter().flatten() {
            if let Some(id) = call["id"].as_str()
                && !answered.contains(id)
            {
                answered.insert(id);
                placeholders.push(json!({"role": "tool", "tool_call_id": id, "content": DANGLING_TOOL_RESULT}));
            }
        }
        if !placeholders.is_empty() {
            pending.push((index, placeholders));
        }
    }
    let mut repaired = 0;
    for (index, placeholders) in pending.into_iter().rev() {
        repaired += placeholders.len();
        messages.splice(index + 1..index + 1, placeholders);
    }
    repaired
}
