//! Input-size estimates that keep the output cap below the model's context window.

use super::*;

/// Tokens reserved per inline image: its base64 is about a hundred times what it bills.
pub(crate) const IMAGE_TOKEN_RESERVE: u64 = 2_048;

/// What an image part becomes when the model can't see images or the provider rejected
/// them: plain text, so the turn goes on. The same words everywhere keep the conversion
/// deterministic per model, which keeps the cached prefix hitting.
pub(crate) const IMAGE_OMITTED: &str = "[image omitted: this model can't see images]";

/// Estimated serialized input size, with inline image data replaced by a fixed per-image
/// reserve instead of treating base64 as text. This only caps output; compaction has its own
/// estimate in `agent.rs`, which reserves the same per image.
fn estimated_value_bytes(value: &Value) -> u64 {
    match value {
        Value::Null => 4,
        Value::Bool(value) => 4 + u64::from(!*value),
        Value::Number(value) => value.to_string().len() as u64,
        Value::String(value) if value.starts_with("data:") && value.contains(";base64,") => {
            IMAGE_TOKEN_RESERVE * 4
        }
        Value::String(value) => value.len() as u64 + 2,
        Value::Array(values) => values.iter().fold(2, |total, value| {
            total
                .saturating_add(estimated_value_bytes(value))
                .saturating_add(1)
        }),
        Value::Object(values) => values.iter().fold(2, |total, (key, value)| {
            let value_bytes =
                if key == "data" && values.get("type").and_then(Value::as_str) == Some("base64") {
                    IMAGE_TOKEN_RESERVE * 4
                } else {
                    estimated_value_bytes(value)
                };
            total
                .saturating_add(key.len() as u64)
                .saturating_add(4)
                .saturating_add(value_bytes)
        }),
    }
}

/// A conservative four-bytes-per-token estimate, including JSON framing and tool schemas.
pub(super) fn estimated_input_tokens(messages: &[Value], tools: &[Value]) -> u64 {
    let bytes = messages.iter().chain(tools).fold(0u64, |total, value| {
        total.saturating_add(estimated_value_bytes(value))
    });
    bytes.saturating_add(3) / 4
}

pub(super) const OUTPUT_SAFETY_TOKENS: u64 = 256;

pub(super) fn cap_output(
    model: &ResolvedModel,
    messages: &[Value],
    tools: &[Value],
    max: u64,
) -> u64 {
    let Some(window) = model.context_window else {
        return max.max(1);
    };
    let reserved = estimated_input_tokens(messages, tools).saturating_add(OUTPUT_SAFETY_TOKENS);
    max.min(window.saturating_sub(reserved)).max(1)
}
