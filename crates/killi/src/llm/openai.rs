//! OpenAI Chat Completions: history keeps its stored form; structured reasoning is converted on a copy.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};

use super::{
    Delta, ErrorKind, Failure, IMAGE_OMITTED, Prepared, Reply, StreamParser, ToolCall, Usage,
    cap_output, truncate,
};
use crate::config::{ResolvedModel, Thinking};

pub const REASONING_KEYS: [&str; 3] = ["reasoning_content", "reasoning_details", "reasoning"];

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: &'a [Value],
    tools: &'a [Value],
    stream: bool,
    stream_options: StreamOptions,
    prompt_cache_key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

pub(super) fn prepare(
    model: &ResolvedModel,
    messages: &[Value],
    tools: &[Value],
    cache_key: &str,
) -> Result<Prepared> {
    let max_output = model
        .max_output
        .map(|n| cap_output(model, messages, tools, n).clamp(1, 131_072));
    let mut messages = replay_history(messages);
    if !model.images {
        omit_images(&mut messages);
    }
    if model.cache_control {
        mark_cache_breakpoints(&mut messages);
    }
    map_history_calls(&mut messages, &model.tool_names);
    let completion_style = model.model.starts_with("gpt-5")
        || (model.model.starts_with('o')
            && model.model[1..].starts_with(|c: char| c.is_ascii_digit()));
    // Some gateways reject reasoning in history unless reasoning is switched on.
    let has_reasoning = || {
        messages
            .iter()
            .any(|m| REASONING_KEYS.iter().any(|k| m.get(*k).is_some()))
    };
    let reasoning_effort = match &model.thinking {
        Thinking::Effort(effort) => Some(effort.as_str()),
        Thinking::On if has_reasoning() => Some("medium"),
        Thinking::On | Thinking::Off => None,
    };
    let tools = map_tools(tools, &model.tool_names);
    let request = Request {
        model: &model.model,
        messages: &messages,
        tools: &tools,
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
        prompt_cache_key: cache_key,
        max_tokens: max_output.filter(|_| !completion_style),
        max_completion_tokens: max_output.filter(|_| completion_style),
        reasoning_effort,
    };
    Ok(Prepared {
        url: format!("{}/chat/completions", model.base_url),
        body: serde_json::to_vec(&request)?,
        headers: vec![
            ("authorization".into(), format!("Bearer {}", model.api_key)),
            ("content-type".into(), "application/json".into()),
        ],
    })
}

/// Puts `cache_control` on the system message and the newest message, on the copy being sent.
/// A gateway in front of a model with explicit prompt caching (Claude on Bedrock) reads the
/// prefix up to the newest mark from the cache, and finds the previous request's prefix by
/// looking back from it, so history itself stays unmarked and byte-for-byte as stored.
fn mark_cache_breakpoints(messages: &mut [Value]) {
    let last = messages.len().saturating_sub(1);
    for (i, message) in messages.iter_mut().enumerate() {
        if i == last || (i == 0 && message["role"] == "system") {
            mark_content(message);
        }
    }
}

fn mark_content(message: &mut Value) {
    let mark = json!({"type": "ephemeral"});
    match &mut message["content"] {
        Value::String(text) if !text.is_empty() => {
            message["content"] =
                json!([{"type": "text", "text": text.clone(), "cache_control": mark}]);
        }
        Value::Array(blocks) => {
            if let Some(block) = blocks.iter_mut().rev().find(|b| b["type"] == "text") {
                block["cache_control"] = mark;
            }
        }
        _ => {}
    }
}

/// Image parts become plain text when the model can't see images. The words never vary,
/// so the conversion stays deterministic per model and the cached prefix still hits.
fn omit_images(messages: &mut [Value]) {
    for message in messages.iter_mut() {
        let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in parts.iter_mut() {
            if part["type"] == "image_url" {
                *part = json!({"type": "text", "text": IMAGE_OMITTED});
            }
        }
    }
}

fn replay_history(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| {
            let mut message = message.clone();
            if let Some(object) = message.as_object_mut()
                && let Some(items) = object.remove("reasoning_items")
            {
                let details: Vec<Value> = items
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|item| {
                        !matches!(
                            item["type"].as_str(),
                            Some("reasoning" | "thinking" | "redacted_thinking")
                        )
                    })
                    .map(|item| {
                        let mut item = item.clone();
                        // Kimi's summary/encrypted dialect uses `index` only while streaming and
                        // rejects it when the items are replayed; other providers keep theirs.
                        if matches!(item["type"].as_str(), Some("summary" | "encrypted"))
                            && let Some(object) = item.as_object_mut()
                        {
                            object.remove("index");
                        }
                        item
                    })
                    .collect();
                if !details.is_empty() {
                    object.insert("reasoning_details".into(), details.into());
                }
            }
            message
        })
        .collect()
}

/// The copy being sent carries the provider's tool names where it maps any, so an API that
/// refuses another client's names still serves the request; the history stays as stored.
fn map_history_calls(messages: &mut [Value], tool_names: &BTreeMap<String, String>) {
    for call in messages
        .iter_mut()
        .filter_map(|message| message.get_mut("tool_calls"))
        .filter_map(Value::as_array_mut)
        .flatten()
    {
        if let Some(name) = call["function"]["name"].as_str().map(str::to_string)
            && let Some(wire) = tool_names.get(&name)
        {
            call["function"]["name"] = json!(wire);
        }
    }
}

/// The tools as the API takes them: the provider's names where it maps any.
fn map_tools(tools: &[Value], tool_names: &BTreeMap<String, String>) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let mut tool = tool.clone();
            if let Some(name) = tool["function"]["name"].as_str().map(str::to_string)
                && let Some(wire) = tool_names.get(&name)
            {
                tool["function"]["name"] = json!(wire);
            }
            tool
        })
        .collect()
}

/// The most streamed tool calls one reply keeps, so a provider that floods or replays
/// indices can't grow it without bound.
const MAX_STREAMED_CALLS: usize = 128;

#[derive(Default)]
pub(super) struct Parser {
    done: bool,
    /// Each provider call index, in arrival order, to the reply slot it was first given.
    call_slots: Vec<(u64, usize)>,
    /// The provider's `tool_names` inverted: the name the API sends back to the session's.
    names: BTreeMap<String, String>,
}

impl Parser {
    pub(super) fn new(tool_names: &BTreeMap<String, String>) -> Self {
        Self {
            names: tool_names
                .iter()
                .map(|(ours, wire)| (wire.clone(), ours.clone()))
                .collect(),
            ..Parser::default()
        }
    }
}

impl StreamParser for Parser {
    fn event(
        &mut self,
        event: &Value,
        reply: &mut Reply,
        on_delta: &mut dyn FnMut(Delta),
    ) -> Result<(), Failure> {
        if let Some(error) = event.get("error").filter(|e| !e.is_null()) {
            let code = error.get("code").and_then(Value::as_u64).map(|c| c as u16);
            return Err(Failure::from_status(
                code,
                format!(
                    "provider error mid-stream: {}",
                    truncate(&error.to_string(), 2000)
                ),
            ));
        }
        if let Some(usage) = event.get("usage").filter(|u| u.is_object()) {
            reply.usage = parse_usage(usage);
        }
        let Some(choice) = event.get("choices").and_then(|c| c.get(0)) else {
            return Ok(());
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            reply.finish_reason = Some(reason.to_string());
        }
        let Some(delta) = choice.get("delta") else {
            return Ok(());
        };
        if delta
            .get("refusal")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
        {
            return Err(Failure::fatal("provider refused the request".into()));
        }
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            reply.text.push_str(text);
            on_delta(Delta::Text(text));
        }
        for key in REASONING_KEYS {
            if let Some(text) = delta.get(key).and_then(Value::as_str) {
                reply.reasoning.push_str(text);
                reply.reasoning_key.get_or_insert(key);
                on_delta(Delta::Reasoning(text));
                break;
            }
        }
        for detail in delta
            .get("reasoning_details")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            merge_reasoning_detail(reply, detail);
        }
        if let Some(object) = delta.as_object() {
            for key in object.keys() {
                if !matches!(key.as_str(), "role" | "content" | "refusal" | "tool_calls")
                    && !REASONING_KEYS.contains(&key.as_str())
                {
                    reply.note_unread(key);
                }
            }
        }
        for call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let index = call.get("index").and_then(Value::as_u64);
            let slot = match index.and_then(|index| {
                self.call_slots
                    .iter()
                    .find(|(seen, _)| *seen == index)
                    .map(|(_, slot)| *slot)
            }) {
                Some(slot) => slot,
                None if reply.tool_calls.len() < MAX_STREAMED_CALLS => {
                    let slot = reply.tool_calls.len();
                    reply.tool_calls.push(ToolCall::default());
                    if let Some(index) = index {
                        self.call_slots.push((index, slot));
                    }
                    slot
                }
                None => continue,
            };
            let slot = &mut reply.tool_calls[slot];
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                slot.id.push_str(id);
            }
            if let Some(function) = call.get("function") {
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    slot.name.push_str(name);
                }
                if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                    slot.arguments.push_str(arguments);
                }
            }
        }
        Ok(())
    }

    fn done(&mut self) {
        self.done = true;
    }

    fn finish(&mut self, reply: &mut Reply) -> Result<(), Failure> {
        // A name can arrive in pieces, so it maps back to the session's once whole.
        for call in &mut reply.tool_calls {
            if let Some(ours) = self.names.get(&call.name) {
                call.name = ours.clone();
            }
        }
        if !self.done && reply.finish_reason.is_none() {
            return Err(Failure::of(
                ErrorKind::BadResponse,
                None,
                true,
                "OpenAI stream ended without finish_reason or [DONE]".into(),
            ));
        }
        let before = reply.tool_calls.len();
        reply.tool_calls.retain(|call| !call.name.is_empty());
        if reply.tool_calls.len() < before
            && reply.tool_calls.is_empty()
            && reply.text.trim().is_empty()
        {
            return Err(Failure::of(
                ErrorKind::BadResponse,
                None,
                true,
                format!(
                    "provider streamed {} tool call(s) without names",
                    before - reply.tool_calls.len()
                ),
            ));
        }
        Ok(())
    }
}

/// Providers identify a reasoning detail by its stream index or id; without either, it continues
/// the latest item of that type. Text-like fields accumulate; metadata is replaced, not repeated.
fn merge_reasoning_detail(reply: &mut Reply, detail: &Value) {
    let same_type = |item: &Value| item["type"] == detail["type"];
    let position = detail
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| {
            reply
                .reasoning_items
                .iter()
                .position(|item| same_type(item) && item["index"].as_u64() == Some(index))
        })
        .or_else(|| {
            detail.get("id").and_then(Value::as_str).and_then(|id| {
                reply
                    .reasoning_items
                    .iter()
                    .position(|item| same_type(item) && item["id"].as_str() == Some(id))
            })
        })
        .or_else(|| {
            if detail.get("index").and_then(Value::as_u64).is_none()
                && detail.get("id").and_then(Value::as_str).is_none()
            {
                reply.reasoning_items.iter().rposition(same_type)
            } else {
                None
            }
        });
    let Some(position) = position else {
        reply.reasoning_items.push(detail.clone());
        return;
    };
    let Some(current) = reply.reasoning_items[position].as_object_mut() else {
        reply.reasoning_items[position] = detail.clone();
        return;
    };
    for (key, value) in detail.as_object().into_iter().flatten() {
        let append = matches!(
            key.as_str(),
            "text" | "summary" | "data" | "encrypted_content" | "signature"
        );
        if append
            && let (Some(Value::String(text)), Some(delta)) = (current.get_mut(key), value.as_str())
        {
            text.push_str(delta);
        } else {
            current.insert(key.clone(), value.clone());
        }
    }
}

fn parse_usage(usage: &Value) -> Usage {
    let num = |v: Option<&Value>| v.and_then(Value::as_u64).unwrap_or(0);
    let cached = num(usage.pointer("/prompt_tokens_details/cached_tokens"))
        .max(num(usage.get("prompt_cache_hit_tokens")))
        .max(num(usage.get("cached_tokens")));
    Usage {
        prompt: num(usage.get("prompt_tokens")),
        cached,
        completion: num(usage.get("completion_tokens")),
        reasoning: num(usage.pointer("/completion_tokens_details/reasoning_tokens")),
    }
}

#[cfg(test)]
mod tests;
