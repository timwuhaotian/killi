//! History read in its stored OpenAI chat form: the system message, message text, and tool-call arguments.

use super::*;

/// The system prompt: the first message, which is always the system message.
pub(super) fn system_text(messages: &[Value]) -> (&str, &[Value]) {
    match messages.first() {
        Some(first) if first["role"] == "system" => (
            first["content"].as_str().unwrap_or_default(),
            &messages[1..],
        ),
        _ => ("", messages),
    }
}

/// A message's text, whether `content` is a string or a list of parts.
pub(super) fn content_text(message: &Value) -> String {
    match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Tool-call arguments as a JSON object, or an empty object when they aren't one.
pub(super) fn arguments_object(arguments: &str) -> Value {
    match serde_json::from_str::<Value>(arguments) {
        Ok(value @ Value::Object(_)) => value,
        _ => Value::Object(Default::default()),
    }
}

/// Whether tool-call arguments are JSON an API accepts back. A provider can end a streamed
/// call without ever sending arguments, and the empty string is not valid JSON.
pub(super) fn arguments_valid(arguments: &str) -> bool {
    serde_json::from_str::<Value>(arguments).is_ok()
}

/// Tool-call arguments as they go back to a provider: as stored when valid, `{}` when not,
/// so a broken stored call can't fail every later request with the same 400.
pub(super) fn json_arguments(arguments: &str) -> &str {
    if arguments_valid(arguments) {
        arguments
    } else {
        "{}"
    }
}
