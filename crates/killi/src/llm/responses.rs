//! OpenAI Responses, stateless: `store: false`, with encrypted reasoning replayed each turn.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::{
    Delta, ErrorKind, Failure, IMAGE_OMITTED, Prepared, Reply, StreamParser, ToolCall, cap_output,
    content_text, json_arguments, spent_quota, system_text, truncate,
};
use crate::config::{ResolvedModel, Thinking};

pub(super) fn prepare(
    model: &ResolvedModel,
    messages: &[Value],
    tools: &[Value],
    cache_key: &str,
) -> Result<Prepared> {
    let (system, history) = system_text(messages);
    let wire = |name: &Value| {
        let name = name.as_str().unwrap_or_default();
        Value::String(
            model
                .tool_names
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_string()),
        )
    };
    let mut body = Map::new();
    body.insert("model".into(), model.model.clone().into());
    body.insert("stream".into(), true.into());
    body.insert("store".into(), false.into());
    if !system.is_empty() {
        body.insert("instructions".into(), system.into());
    }
    body.insert(
        "input".into(),
        convert_input(history, model.images, &model.tool_names, &model.alias).into(),
    );
    let tools: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let f = &tool["function"];
            json!({"type": "function", "name": wire(&f["name"]), "description": f["description"], "parameters": f["parameters"], "strict": false})
        })
        .collect();
    body.insert("prompt_cache_key".into(), cache_key.into());
    // A ChatGPT plan turns `max_output_tokens` away.
    if let Some(max) = model.max_output.filter(|_| !model.chatgpt) {
        body.insert(
            "max_output_tokens".into(),
            cap_output(model, messages, &tools, max)
                .clamp(1, 131_072)
                .into(),
        );
    }
    body.insert("tools".into(), tools.into());
    match &model.thinking {
        Thinking::Off => {}
        Thinking::On => {
            body.insert("reasoning".into(), json!({"summary": "auto"}));
        }
        Thinking::Effort(effort) => {
            body.insert(
                "reasoning".into(),
                json!({"effort": effort, "summary": "auto"}),
            );
        }
    }
    if model.thinking != Thinking::Off {
        body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    }
    let mut headers = vec![("content-type".into(), "application/json".into())];
    // A plan's token is added as each attempt is sent, since it is refreshed as it expires.
    if !model.chatgpt {
        headers.push(("authorization".into(), format!("Bearer {}", model.api_key)));
    }
    Ok(Prepared {
        url: format!("{}/responses", model.base_url),
        body: serde_json::to_vec(&body)?,
        headers,
    })
}

fn convert_input(
    history: &[Value],
    images: bool,
    tool_names: &BTreeMap<String, String>,
    origin: &str,
) -> Vec<Value> {
    let wire = |name: &Value| {
        let name = name.as_str().unwrap_or_default();
        Value::String(
            tool_names
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_string()),
        )
    };
    let mut input = Vec::new();
    for message in history {
        match message["role"].as_str() {
            Some("assistant") => {
                for item in message["reasoning_items"].as_array().into_iter().flatten() {
                    if item["type"] == "reasoning"
                        && item["killi_origin"] == origin
                        && replayable_reasoning(item)
                    {
                        let mut item = item.clone();
                        if let Some(object) = item.as_object_mut() {
                            object.remove("killi_origin");
                            // ChatGPT returns this output-only field but rejects it in input.
                            object.remove("status");
                        }
                        input.push(item);
                    }
                }
                if let Some(text) = message["content"].as_str().filter(|t| !t.is_empty()) {
                    input.push(json!({"role": "assistant", "content": text}));
                }
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    let arguments = call["function"]["arguments"].as_str().unwrap_or_default();
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call["id"],
                        "name": wire(&call["function"]["name"]),
                        "arguments": json_arguments(arguments),
                    }));
                }
            }
            Some("tool") => {
                input.push(json!({"type": "function_call_output", "call_id": message["tool_call_id"], "output": content_text(message)}));
            }
            Some(role) => match &message["content"] {
                Value::Array(parts) => {
                    let content: Vec<Value> = parts
                        .iter()
                        .filter_map(|part| match part["type"].as_str() {
                            Some("text") => Some(json!({"type": "input_text", "text": part["text"]})),
                            Some("image_url") => Some(match images {
                                true => json!({"type": "input_image", "image_url": part["image_url"]["url"]}),
                                false => json!({"type": "input_text", "text": IMAGE_OMITTED}),
                            }),
                            _ => None,
                        })
                        .collect();
                    input.push(json!({"role": role, "content": content}));
                }
                other => {
                    input.push(json!({"role": role, "content": other.as_str().unwrap_or_default()}))
                }
            },
            None => {}
        }
    }
    input
}

/// The error a failed event carries. A provider puts it beside the type, under `error`,
/// or under `response.error`; the code that says what went wrong sits inside it, and a
/// code read from the wrong place is an empty one — which reads as "try again".
fn failed_error(event: &Value) -> Value {
    let error = &event["error"];
    if error.is_object() {
        return error.clone();
    }
    let response_error = &event["response"]["error"];
    if response_error.is_object() {
        return response_error.clone();
    }
    event.clone()
}

/// A reasoning item can only be replayed under `store: false` when it carries its encrypted
/// content; summary text alone is kept for gateways that accept it.
fn replayable_reasoning(item: &Value) -> bool {
    item.get("encrypted_content").is_some()
        || item["summary"].as_array().is_some_and(|summary| {
            summary
                .iter()
                .any(|part| part["text"].as_str().is_some_and(|text| !text.is_empty()))
        })
}

#[derive(Default)]
pub(super) struct Parser {
    /// Output index of each function call, to route argument deltas.
    calls: HashMap<u64, usize>,
    /// The provider's `tool_names` inverted: the name the API sends back to the session's name.
    names: HashMap<String, String>,
    /// The model whose caller owns encrypted reasoning emitted by this stream.
    origin: String,
    terminal: bool,
}

impl Parser {
    pub(super) fn new(model: &ResolvedModel) -> Self {
        Self {
            names: model
                .tool_names
                .iter()
                .map(|(ours, wire)| (wire.clone(), ours.clone()))
                .collect(),
            origin: model.alias.clone(),
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
        let index = event["output_index"].as_u64().unwrap_or(0);
        match event["type"].as_str() {
            Some("response.refusal.delta") => {
                if event["delta"].as_str().is_some_and(|text| !text.is_empty()) {
                    return Err(Failure::fatal("provider refused the request".into()));
                }
            }
            Some("response.output_text.delta") => {
                let text = event["delta"].as_str().unwrap_or_default();
                reply.text.push_str(text);
                on_delta(Delta::Text(text));
            }
            Some("response.reasoning_summary_text.delta") => {
                let text = event["delta"].as_str().unwrap_or_default();
                reply.reasoning.push_str(text);
                on_delta(Delta::Reasoning(text));
            }
            Some("response.output_item.added") if event["item"]["type"] == "function_call" => {
                let item = &event["item"];
                let name = item["name"].as_str().unwrap_or_default();
                let ours = self
                    .names
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| name.to_string());
                reply.tool_calls.push(ToolCall {
                    id: item["call_id"].as_str().unwrap_or_default().to_string(),
                    name: ours,
                    arguments: String::new(),
                });
                self.calls.insert(index, reply.tool_calls.len() - 1);
            }
            Some("response.function_call_arguments.delta") => {
                if let Some(&i) = self.calls.get(&index) {
                    let text = event["delta"].as_str().unwrap_or_default();
                    reply.tool_calls[i].arguments.push_str(text);
                    on_delta(Delta::Tool(text));
                }
            }
            Some("response.output_item.done") => {
                let item = &event["item"];
                match item["type"].as_str() {
                    Some("reasoning") => {
                        let mut item = item.clone();
                        item["killi_origin"] = self.origin.clone().into();
                        reply.reasoning_items.push(item);
                    }
                    Some("function_call") => {
                        if let (Some(&i), Some(arguments)) =
                            (self.calls.get(&index), item["arguments"].as_str())
                        {
                            reply.tool_calls[i].arguments = arguments.to_string();
                        }
                    }
                    _ => {}
                }
            }
            Some("response.completed" | "response.incomplete") => {
                self.terminal = true;
                let response = &event["response"];
                let usage = &response["usage"];
                let num =
                    |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
                reply.usage.prompt = num("/input_tokens");
                reply.usage.cached = num("/input_tokens_details/cached_tokens");
                reply.usage.completion = num("/output_tokens");
                reply.usage.reasoning = num("/output_tokens_details/reasoning_tokens");
                let incomplete = event["type"] == "response.incomplete";
                let reason = response["incomplete_details"]["reason"].as_str();
                if incomplete && reason != Some("max_output_tokens") {
                    let message = format!(
                        "response incomplete: {}",
                        reason.unwrap_or("unknown reason")
                    );
                    return Err(if reason == Some("content_filter") {
                        Failure::fatal(message)
                    } else {
                        Failure::transient(message)
                    });
                }
                reply.finish_reason = Some(
                    if incomplete {
                        "length"
                    } else if reply.tool_calls.is_empty() {
                        "stop"
                    } else {
                        "tool_calls"
                    }
                    .to_string(),
                );
            }
            Some("response.failed" | "error") => {
                let error = failed_error(event);
                let text = error.to_string();
                let message = format!("provider error mid-stream: {}", truncate(&text, 2000));
                let code = error["code"]
                    .as_str()
                    .or_else(|| event["code"].as_str())
                    .unwrap_or_default()
                    .to_lowercase();
                let lower = text.to_lowercase();
                // A code names the failure; without one the text is all there is, and it
                // says the same things a 429 body does.
                let kind = if code.contains("rate_limit")
                    || (code.is_empty() && lower.contains("rate limit"))
                {
                    ErrorKind::RateLimited
                } else if code.contains("usage_limit")
                    || spent_quota(&code)
                    || (code.is_empty() && spent_quota(&lower))
                {
                    ErrorKind::Quota
                } else {
                    ErrorKind::Other
                };
                let retry = match kind {
                    ErrorKind::Quota => false,
                    ErrorKind::RateLimited => true,
                    _ => {
                        code.contains("server_error")
                            || code.ends_with("_unavailable")
                            || code.is_empty()
                    }
                };
                return Err(Failure::of(kind, None, retry, message));
            }
            _ => {}
        }
        Ok(())
    }

fn finish(&mut self, reply: &mut Reply) -> Result<(), Failure> {
        if self.terminal {
            reply.repair_call_arguments();
            Ok(())
        } else {
            Err(Failure::of(
                ErrorKind::BadResponse,
                None,
                true,
                "Responses stream ended without a terminal response event".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests;
