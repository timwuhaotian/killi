//! OpenAI Responses, stateless: `store: false`, with encrypted reasoning replayed each turn.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::{Map, Value, json};

use super::{Delta, ErrorKind, Failure, Prepared, Reply, StreamParser, ToolCall, IMAGE_OMITTED, cap_output, content_text, system_text, truncate};
use crate::config::{ResolvedModel, Thinking};

pub(super) fn prepare(model: &ResolvedModel, messages: &[Value], tools: &[Value], cache_key: &str) -> Result<Prepared> {
    let (system, history) = system_text(messages);
    let mut body = Map::new();
    body.insert("model".into(), model.model.clone().into());
    body.insert("stream".into(), true.into());
    body.insert("store".into(), false.into());
    if !system.is_empty() {
        body.insert("instructions".into(), system.into());
    }
    body.insert("input".into(), convert_input(history, model.images).into());
    let tools: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let f = &tool["function"];
            json!({"type": "function", "name": f["name"], "description": f["description"], "parameters": f["parameters"], "strict": false})
        })
        .collect();
    body.insert("prompt_cache_key".into(), cache_key.into());
    // A ChatGPT plan turns `max_output_tokens` away.
    if let Some(max) = model.max_output.filter(|_| !model.chatgpt) {
        body.insert("max_output_tokens".into(), cap_output(model, messages, &tools, max).clamp(1, 131_072).into());
    }
    body.insert("tools".into(), tools.into());
    match &model.thinking {
        Thinking::Off => {}
        Thinking::On => {
            body.insert("reasoning".into(), json!({"summary": "auto"}));
        }
        Thinking::Effort(effort) => {
            body.insert("reasoning".into(), json!({"effort": effort, "summary": "auto"}));
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
    Ok(Prepared { url: format!("{}/responses", model.base_url), body: serde_json::to_vec(&body)?, headers })
}

fn convert_input(history: &[Value], images: bool) -> Vec<Value> {
    let mut input = Vec::new();
    for message in history {
        match message["role"].as_str() {
            Some("assistant") => {
                for item in message["reasoning_items"].as_array().into_iter().flatten() {
                    if item["type"] == "reasoning" && replayable_reasoning(item) {
                        input.push(item.clone());
                    }
                }
                if let Some(text) = message["content"].as_str().filter(|t| !t.is_empty()) {
                    input.push(json!({"role": "assistant", "content": text}));
                }
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call["id"],
                        "name": call["function"]["name"],
                        "arguments": call["function"]["arguments"],
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
                other => input.push(json!({"role": role, "content": other.as_str().unwrap_or_default()})),
            },
            None => {}
        }
    }
    input
}

/// A reasoning item can only be replayed under `store: false` when it carries its encrypted
/// content; summary text alone is kept for gateways that accept it.
fn replayable_reasoning(item: &Value) -> bool {
    item.get("encrypted_content").is_some()
        || item["summary"].as_array().is_some_and(|summary| {
            summary.iter().any(|part| part["text"].as_str().is_some_and(|text| !text.is_empty()))
        })
}

#[derive(Default)]
pub(super) struct Parser {
    /// Output index of each function call, to route argument deltas.
    calls: HashMap<u64, usize>,
    terminal: bool,
}

impl StreamParser for Parser {
    fn event(&mut self, event: &Value, reply: &mut Reply, on_delta: &mut dyn FnMut(Delta)) -> Result<(), Failure> {
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
                reply.tool_calls.push(ToolCall {
                    id: item["call_id"].as_str().unwrap_or_default().to_string(),
                    name: item["name"].as_str().unwrap_or_default().to_string(),
                    arguments: String::new(),
                });
                self.calls.insert(index, reply.tool_calls.len() - 1);
            }
            Some("response.function_call_arguments.delta") => {
                if let Some(&i) = self.calls.get(&index) {
                    reply.tool_calls[i].arguments.push_str(event["delta"].as_str().unwrap_or_default());
                }
            }
            Some("response.output_item.done") => {
                let item = &event["item"];
                match item["type"].as_str() {
                    Some("reasoning") => reply.reasoning_items.push(item.clone()),
                    Some("function_call") => {
                        if let (Some(&i), Some(arguments)) = (self.calls.get(&index), item["arguments"].as_str()) {
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
                let num = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
                reply.usage.prompt = num("/input_tokens");
                reply.usage.cached = num("/input_tokens_details/cached_tokens");
                reply.usage.completion = num("/output_tokens");
                reply.usage.reasoning = num("/output_tokens_details/reasoning_tokens");
                let incomplete = event["type"] == "response.incomplete";
                let reason = response["incomplete_details"]["reason"].as_str();
                if incomplete && reason != Some("max_output_tokens") {
                    let message = format!("response incomplete: {}", reason.unwrap_or("unknown reason"));
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
                let error = if event["type"] == "error" { event.clone() } else { event["response"]["error"].clone() };
                let message = format!("provider error mid-stream: {}", truncate(&error.to_string(), 2000));
                let code = error["code"].as_str().unwrap_or_default();
                let kind = if code.contains("rate_limit") {
                    ErrorKind::RateLimited
                } else if code.contains("usage_limit") || code.contains("quota") {
                    ErrorKind::Quota
                } else {
                    ErrorKind::Other
                };
                let retry = code.contains("rate_limit") || code.contains("server_error") || code.ends_with("_unavailable") || code.is_empty();
                return Err(if retry {
                    Failure::of(kind, None, true, message)
                } else {
                    Failure::of(kind, None, false, message)
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(&mut self, _reply: &mut Reply) -> Result<(), Failure> {
        if self.terminal {
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
mod tests {
    use super::*;

    #[test]
    fn history_becomes_input_items() {
        let history = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "reasoning_items": [{"type": "reasoning", "encrypted_content": "e"}],
                   "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{}"}}]}),
            json!({"role": "tool", "tool_call_id": "c1", "content": "data"}),
        ];
        let input = convert_input(&history, true);
        assert_eq!(input[0], json!({"role": "user", "content": "hi"}));
        assert_eq!(input[1]["encrypted_content"], "e");
        assert_eq!(input[2], json!({"type": "function_call", "call_id": "c1", "name": "read", "arguments": "{}"}));
        assert_eq!(input[3], json!({"type": "function_call_output", "call_id": "c1", "output": "data"}));
    }

    #[test]
    fn images_become_text_when_the_model_cannot_see_them() {
        let history = vec![json!({"role": "user", "content": [
            {"type": "text", "text": "look"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
        ]})];
        let input = convert_input(&history, false);
        let content = input[0]["content"].clone();
        assert!(content.as_array().unwrap().iter().all(|part| part["type"] != "input_image"), "{content}");
        assert!(content.as_array().unwrap().iter().any(|part| part["text"] == super::IMAGE_OMITTED), "{content}");
        assert_eq!(history[0]["content"][1]["type"], "image_url", "history itself stays as sent");
    }

    #[test]
    fn stream_assembles_calls_and_usage() {
        let mut parser = Parser::default();
        let mut reply = Reply::default();
        let mut sink = |_: Delta| {};
        let events = [
            json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "function_call", "call_id": "c1", "name": "grep"}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 1, "delta": "{\"pattern\":"}),
            json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "function_call", "arguments": "{\"pattern\":\"x\"}"}}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "reasoning", "encrypted_content": "e"}}),
            json!({"type": "response.completed", "response": {"usage": {"input_tokens": 50, "input_tokens_details": {"cached_tokens": 40}, "output_tokens": 7}}}),
        ];
        for event in &events {
            parser.event(event, &mut reply, &mut sink).ok().unwrap();
        }
        assert_eq!(reply.tool_calls[0].arguments, "{\"pattern\":\"x\"}");
        assert_eq!(reply.reasoning_items.len(), 1);
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!((reply.usage.prompt, reply.usage.cached), (50, 40));
        parser.finish(&mut reply).unwrap();
    }

    #[test]
    fn terminal_event_is_required_and_incomplete_reasons_are_classified() {
        let mut parser = Parser::default();
        let mut reply = Reply::default();
        let mut sink = |_: Delta| {};
        assert!(parser.finish(&mut reply).unwrap_err().retryable);

        let incomplete = json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "content_filter"}}});
        assert!(!parser.event(&incomplete, &mut reply, &mut sink).unwrap_err().retryable);
        let incomplete = json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "internal_error"}}});
        assert!(parser.event(&incomplete, &mut reply, &mut sink).unwrap_err().retryable);
        let truncated = json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "max_output_tokens"}}});
        parser.event(&truncated, &mut reply, &mut sink).unwrap();
        assert_eq!(reply.finish_reason.as_deref(), Some("length"));
        parser.finish(&mut reply).unwrap();
    }

    #[test]
    fn reasoning_without_encrypted_content_is_not_replayed() {
        let model = ResolvedModel {
            alias: "m".into(),
            model: "gpt-5".into(),
            protocol: crate::config::Protocol::Responses,
            base_url: "http://localhost".into(),
            api_key: String::new(),
            headers: Default::default(),
            session_header: None,
            max_output: None,
            context_window: None,
            images: false,
            thinking: Thinking::Off,
            cache_control: false,
            chatgpt: false,
        };
        let messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "assistant", "content": "ok", "reasoning_items": [
                {"type": "reasoning", "id": "rs_bare"},
                {"type": "reasoning", "id": "rs_summary", "summary": [{"type": "summary_text", "text": "for gateways"}]},
                {"type": "reasoning", "id": "rs_encrypted", "encrypted_content": "e"},
            ]}),
        ];
        let prepared = prepare(&model, &messages, &[], "k").unwrap();
        let body: Value = serde_json::from_slice(&prepared.body).unwrap();
        let ids: Vec<&str> = body["input"].as_array().unwrap().iter().filter_map(|item| item["id"].as_str()).collect();
        assert!(!ids.contains(&"rs_bare"), "a reasoning item with nothing to replay must not be sent");
        assert!(ids.contains(&"rs_encrypted"));
        assert!(ids.contains(&"rs_summary"), "summary text is kept for gateways");
        assert!(body.get("include").is_none(), "thinking off does not ask for encrypted reasoning");
    }

    #[test]
    fn prepare_caps_output_from_remaining_context() {
        let model = ResolvedModel {
            alias: "m".into(),
            model: "gpt-5".into(),
            protocol: crate::config::Protocol::Responses,
            base_url: "http://localhost".into(),
            api_key: String::new(),
            headers: Default::default(),
            session_header: None,
            max_output: Some(4_000),
            context_window: Some(2_000),
            images: false,
            thinking: Thinking::Off,
            cache_control: false,
            chatgpt: false,
        };
        let messages = vec![json!({"role": "user", "content": "hello"})];
        let prepared = prepare(&model, &messages, &[], "cache").unwrap();
        let body: Value = serde_json::from_slice(&prepared.body).unwrap();
        let expected = super::cap_output(&model, &messages, &[], 4_000);
        assert_eq!(body["max_output_tokens"], expected);
    }

    #[test]
    fn a_chatgpt_plan_request_has_no_output_cap_and_no_key() {
        let model = ResolvedModel {
            alias: "m".into(),
            model: "gpt-5".into(),
            protocol: crate::config::Protocol::Responses,
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            headers: Default::default(),
            session_header: None,
            max_output: Some(4_000),
            context_window: Some(272_000),
            images: false,
            thinking: Thinking::Effort("high".into()),
            cache_control: false,
            chatgpt: true,
        };
        let prepared = prepare(&model, &[json!({"role": "system", "content": "sys"}), json!({"role": "user", "content": "hi"})], &[], "cache").unwrap();
        let body: Value = serde_json::from_slice(&prepared.body).unwrap();
        assert!(body.get("max_output_tokens").is_none());
        assert_eq!((body["store"].clone(), body["stream"].clone(), body["instructions"].clone()), (json!(false), json!(true), json!("sys")));
        assert!(body["input"].as_array().unwrap().iter().all(|item| item["role"] != "system"), "the plan rejects system items");
        assert!(prepared.headers.iter().all(|(name, _)| name != "authorization"), "the token is added per attempt");
    }

    #[test]
    fn a_refusal_delta_is_fatal() {
        let mut parser = Parser::default();
        let mut reply = Reply::default();
        let mut sink = |_: Delta| {};
        let event = json!({"type": "response.refusal.delta", "delta": "cannot help"});
        assert!(!parser.event(&event, &mut reply, &mut sink).unwrap_err().retryable);
    }

    #[test]
    fn plan_errors_mid_stream_retry_only_when_temporary() {
        let mut sink = |_: Delta| {};
        let failed = |code: &str| json!({"type": "response.failed", "response": {"error": {"code": code}}});
        let mut reply = Reply::default();
        assert!(Parser::default().event(&failed("subscription_sharing_usage_unavailable"), &mut reply, &mut sink).unwrap_err().retryable);
        assert!(!Parser::default().event(&failed("subscription_sharing_usage_limit_exceeded"), &mut reply, &mut sink).unwrap_err().retryable);
    }
}
