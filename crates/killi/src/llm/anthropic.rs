//! Anthropic Messages, also spoken by MiniMax and other Anthropic-compatible providers.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::{Delta, ErrorKind, Failure, Prepared, Reply, StreamParser, ToolCall, IMAGE_OMITTED, arguments_object, cap_output, content_text, system_text, truncate};
use crate::config::{ResolvedModel, Thinking};

const VERSION: &str = "2023-06-01";
const FALLBACK_MAX_TOKENS: u64 = 64_000;

pub(super) fn prepare(model: &ResolvedModel, messages: &[Value], tools: &[Value]) -> Result<Prepared> {
    let claude = ClaudeVersion::parse(&model.model);
    let (system, history) = system_text(messages);
    let mut body = Map::new();
    body.insert("model".into(), model.model.clone().into());
    let max_tokens = max_tokens(model, claude, messages, tools);
    body.insert("max_tokens".into(), max_tokens.into());
    body.insert("stream".into(), true.into());
    if !system.is_empty() {
        body.insert("system".into(), json!([{"type": "text", "text": system, "cache_control": {"type": "ephemeral"}}]));
    }
    body.insert("tools".into(), convert_tools(tools));
    body.insert("messages".into(), convert_messages(history, claude.is_some(), model.images).into());
    let budget = claude.is_some_and(|v| v.budget_thinking());
    for (key, value) in thinking_params(&model.thinking, budget, max_tokens) {
        body.insert(key, value);
    }
    let mut headers = vec![
        ("x-api-key".to_string(), model.api_key.clone()),
        ("anthropic-version".to_string(), VERSION.to_string()),
        ("content-type".to_string(), "application/json".to_string()),
    ];
    if budget && model.thinking != Thinking::Off {
        headers.push(("anthropic-beta".into(), "interleaved-thinking-2025-05-14".into()));
    }
    Ok(Prepared { url: format!("{}/v1/messages", model.base_url), body: serde_json::to_vec(&body)?, headers })
}

/// Recognizes Claude model names, whose thinking rules differ from compatible providers'.
#[derive(Debug, Clone, Copy)]
struct ClaudeVersion {
    family: Family,
    major: u32,
    minor: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Family {
    Opus,
    Sonnet,
    Haiku,
    Other,
}

impl ClaudeVersion {
    fn parse(model: &str) -> Option<Self> {
        let name = model.to_ascii_lowercase();
        let rest = name.rsplit('/').next()?.strip_prefix("claude-")?;
        let family = ["opus", "sonnet", "haiku"].iter().zip([Family::Opus, Family::Sonnet, Family::Haiku]).find_map(
            |(word, family)| rest.contains(word).then_some(family),
        );
        let numbers: Vec<u32> = rest.split(['-', '.']).filter_map(|part| part.parse().ok()).filter(|n| *n < 100).collect();
        let major = *numbers.first()?;
        Some(Self { family: family.unwrap_or(Family::Other), major, minor: numbers.get(1).copied().unwrap_or(0) })
    }

    /// Older Claude models take a token budget; newer ones decide adaptively.
    fn budget_thinking(self) -> bool {
        self.major < 4 || (self.major == 4 && self.minor <= 5 && self.family != Family::Other)
    }

    fn max_tokens(self) -> u64 {
        match (self.family, self.major, self.minor) {
            (_, 5.., _) | (Family::Opus, 4, 6..) | (Family::Sonnet, 4, 6..) => 128_000,
            (Family::Opus, 4, 0..=1) => 32_000,
            (_, 4, _) => 64_000,
            (Family::Haiku, 3, _) => 4_096,
            _ => 8_192,
        }
    }
}

fn max_tokens(model: &ResolvedModel, claude: Option<ClaudeVersion>, messages: &[Value], tools: &[Value]) -> u64 {
    let max = match claude {
        Some(version) => model.max_output.map_or(version.max_tokens(), |n| n.min(version.max_tokens())),
        None => model.max_output.unwrap_or(FALLBACK_MAX_TOKENS),
    };
    cap_output(model, messages, tools, max)
}

/// Kimi Code's encoding: budget tokens for older Claude models, adaptive thinking plus an
/// effort for everything else, including Anthropic-compatible providers.
fn thinking_params(thinking: &Thinking, budget: bool, max_tokens: u64) -> Vec<(String, Value)> {
    let effort = match thinking {
        Thinking::Off => return vec![("thinking".into(), json!({"type": "disabled"}))],
        Thinking::On => None,
        Thinking::Effort(effort) => Some(effort.as_str()),
    };
    if budget {
        // Anthropic's minimum explicit budget is 1024; below that, disable thinking rather than
        // consume the entire response or emit an invalid budget.
        if max_tokens <= 1_024 {
            return vec![("thinking".into(), json!({"type": "disabled"}))];
        }
        let tokens = match effort {
            Some("low") => Some(1_024),
            Some("medium") => Some(4_096),
            None | Some("high") => Some(32_000),
            Some(_) => None,
        };
        let mut params = vec![(
            "thinking".to_string(),
            match tokens {
                Some(tokens) => json!({"type": "enabled", "budget_tokens": tokens.min(max_tokens - 1)}),
                None => json!({"type": "enabled"}),
            },
        )];
        if let (None, Some(effort)) = (tokens, effort) {
            params.push(("output_config".into(), json!({"effort": effort})));
        }
        return params;
    }
    let mut params = vec![("thinking".to_string(), json!({"type": "adaptive", "display": "summarized"}))];
    if let Some(effort) = effort {
        params.push(("output_config".into(), json!({"effort": effort})));
    }
    params
}

fn convert_tools(tools: &[Value]) -> Value {
    let mut converted: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let function = &tool["function"];
            json!({"name": function["name"], "description": function["description"], "input_schema": function["parameters"]})
        })
        .collect();
    if let Some(last) = converted.last_mut() {
        last["cache_control"] = json!({"type": "ephemeral"});
    }
    converted.into()
}

/// Converts OpenAI-format history: tool results become user `tool_result` blocks, consecutive
/// user turns merge (tool results first), and the last block carries a cache breakpoint.
fn convert_messages(history: &[Value], claude: bool, images: bool) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for message in history {
        match message["role"].as_str() {
            Some("assistant") => {
                let blocks = assistant_blocks(message, claude);
                if !blocks.is_empty() {
                    out.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            Some("tool") => {
                let text = content_text(message);
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": message["tool_call_id"],
                    "content": if text.is_empty() { "(empty)".to_string() } else { text },
                });
                push_user(&mut out, vec![block], true);
            }
            Some("system") => {
                push_user(&mut out, vec![json!({"type": "text", "text": format!("<system>{}</system>", content_text(message))})], false);
            }
            _ => {
                let blocks = user_blocks(message, images);
                if !blocks.is_empty() {
                    push_user(&mut out, blocks, false);
                }
            }
        }
    }
    if let Some(block) = out.last_mut().and_then(|m| m["content"].as_array_mut()).and_then(|blocks| blocks.last_mut())
        && !matches!(block["type"].as_str(), Some("thinking" | "redacted_thinking"))
    {
        block["cache_control"] = json!({"type": "ephemeral"});
    }
    out
}

fn push_user(out: &mut Vec<Value>, blocks: Vec<Value>, tool_result: bool) {
    if let Some(last) = out.last_mut().filter(|m| m["role"] == "user") {
        let only_results = last["content"].as_array().is_some_and(|b| b.iter().all(|b| b["type"] == "tool_result"));
        if !tool_result || only_results {
            last["content"].as_array_mut().expect("user content is a list").extend(blocks);
            return;
        }
    }
    out.push(json!({"role": "user", "content": blocks}));
}

fn user_blocks(message: &Value, images: bool) -> Vec<Value> {
    match &message["content"] {
        Value::String(text) if !text.is_empty() => vec![json!({"type": "text", "text": text})],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part["type"].as_str() {
                Some("text") => part["text"].as_str().filter(|t| !t.is_empty()).map(|t| json!({"type": "text", "text": t})),
                Some("image_url") => match images {
                    true => image_block(part["image_url"]["url"].as_str()?),                    false => Some(json!({"type": "text", "text": IMAGE_OMITTED})),
                },
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn image_block(url: &str) -> Option<Value> {
    let rest = url.strip_prefix("data:")?;
    let (media_type, data) = rest.split_once(";base64,")?;
    Some(json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data}}))
}

fn assistant_blocks(message: &Value, claude: bool) -> Vec<Value> {
    let mut blocks = Vec::new();
    if let Some(items) = message["reasoning_items"].as_array() {
        for item in items {
            match item["type"].as_str() {
                Some("redacted_thinking") => blocks.push(item.clone()),
                Some("thinking") if !claude || item.get("signature").is_some() => blocks.push(item.clone()),
                _ => {}
            }
        }
    }
    // A chat reply can carry structured details Anthropic does not use beside its scalar
    // reasoning. Only an emitted thinking block suppresses the scalar fallback.
    if blocks.is_empty()
        && !claude
        && let Some(text) = super::REASONING_KEYS.iter().find_map(|k| message[*k].as_str()).filter(|t| !t.is_empty())
    {
        blocks.push(json!({"type": "thinking", "thinking": text}));
    }
    if let Some(text) = message["content"].as_str().filter(|t| !t.trim().is_empty()) {
        blocks.push(json!({"type": "text", "text": text}));
    }
    for call in message["tool_calls"].as_array().into_iter().flatten() {
        blocks.push(json!({
            "type": "tool_use",
            "id": call["id"],
            "name": call["function"]["name"],
            "input": arguments_object(call["function"]["arguments"].as_str().unwrap_or_default()),
        }));
    }
    blocks
}

/// Appends `text` to the string in `field` in place, starting one if there is none yet. Building
/// a new string per delta copies all the thinking so far each time.
fn append(field: &mut Value, text: &str) {
    match field {
        Value::String(current) => current.push_str(text),
        other => *other = text.into(),
    }
}

/// Routes streamed deltas to the content block they belong to.
#[derive(Default)]
pub(super) struct Parser {
    blocks: HashMap<u64, Block>,
    input: InputUsage,
    message_stop: bool,
}

/// Input token counts. Anthropic reports them in `message_start`; MiniMax sends placeholders
/// there and the real counts in `message_delta`, so later values replace earlier ones.
#[derive(Default)]
struct InputUsage {
    fresh: u64,
    written: u64,
    read: u64,
}

impl InputUsage {
    fn update(&mut self, usage: &Value, reply: &mut Reply) {
        if !usage.is_object() {
            return;
        }
        if let Some(n) = usage["input_tokens"].as_u64() {
            self.fresh = n;
        }
        if let Some(n) = usage["cache_creation_input_tokens"].as_u64() {
            self.written = n;
        }
        if let Some(n) = usage["cache_read_input_tokens"].as_u64() {
            self.read = n;
        }
        reply.usage.prompt = self.fresh + self.written + self.read;
        reply.usage.cached = self.read;
    }
}

enum Block {
    Text,
    Thinking(usize),
    Tool(usize),
    Other,
}

impl StreamParser for Parser {
    fn event(&mut self, event: &Value, reply: &mut Reply, on_delta: &mut dyn FnMut(Delta)) -> Result<(), Failure> {
        let index = event["index"].as_u64().unwrap_or(0);
        match event["type"].as_str() {
            Some("message_start") => self.input.update(&event["message"]["usage"], reply),
            Some("content_block_start") => {
                let block = &event["content_block"];
                let kind = match block["type"].as_str() {
                    Some("text") => Block::Text,
                    Some("thinking") => {
                        reply.reasoning_items.push(json!({"type": "thinking", "thinking": ""}));
                        Block::Thinking(reply.reasoning_items.len() - 1)
                    }
                    Some("redacted_thinking") => {
                        reply.reasoning_items.push(json!({"type": "redacted_thinking", "data": block["data"]}));
                        Block::Other
                    }
                    Some("tool_use") => {
                        reply.tool_calls.push(ToolCall {
                            id: block["id"].as_str().unwrap_or_default().to_string(),
                            name: block["name"].as_str().unwrap_or_default().to_string(),
                            arguments: String::new(),
                        });
                        Block::Tool(reply.tool_calls.len() - 1)
                    }
                    _ => Block::Other,
                };
                self.blocks.insert(index, kind);
            }
            Some("content_block_delta") => {
                let delta = &event["delta"];
                match (self.blocks.get(&index), delta["type"].as_str()) {
                    (Some(Block::Text), Some("text_delta")) => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        reply.text.push_str(text);
                        on_delta(Delta::Text(text));
                    }
                    (Some(Block::Thinking(i)), Some("thinking_delta")) => {
                        let text = delta["thinking"].as_str().unwrap_or_default();
                        reply.reasoning.push_str(text);
                        append(&mut reply.reasoning_items[*i]["thinking"], text);
                        on_delta(Delta::Reasoning(text));
                    }
                    (Some(Block::Thinking(i)), Some("signature_delta")) => {
                        append(&mut reply.reasoning_items[*i]["signature"], delta["signature"].as_str().unwrap_or_default());
                    }
                    (Some(Block::Tool(i)), Some("input_json_delta")) => {
                        reply.tool_calls[*i].arguments.push_str(delta["partial_json"].as_str().unwrap_or_default());
                    }
                    _ => {}
                }
            }
            Some("message_delta") => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    reply.finish_reason = Some(
                        match reason {
                            "max_tokens" => "length",
                            "tool_use" => "tool_calls",
                            "end_turn" | "stop_sequence" => "stop",
                            other => other,
                        }
                        .to_string(),
                    );
                }
                if let Some(output) = event["usage"]["output_tokens"].as_u64() {
                    reply.usage.completion = output;
                }
                self.input.update(&event["usage"], reply);
            }
            Some("message_stop") => self.message_stop = true,
            Some("error") => {
                let error = &event["error"];
                let message = format!("provider error mid-stream: {}", truncate(&error.to_string(), 2000));
                return Err(match error["type"].as_str() {
                    Some("authentication_error") => Failure::of(ErrorKind::Auth, None, false, message),
                    Some("invalid_request_error" | "permission_error" | "not_found_error") => Failure::fatal(message),
                    _ => Failure::transient(message),
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(&mut self, reply: &mut Reply) -> Result<(), Failure> {
        if !self.message_stop {
            return Err(Failure::of(
                ErrorKind::BadResponse,
                None,
                true,
                "Anthropic stream ended without message_stop".into(),
            ));
        }
        for call in &mut reply.tool_calls {
            if call.arguments.trim().is_empty() {
                call.arguments = "{}".into();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
