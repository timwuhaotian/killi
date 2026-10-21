//! OpenAI Chat Completions: the history is already in this format, so it is sent as is.

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use super::{Delta, Failure, Prepared, Reply, StreamParser, ToolCall, Usage, truncate};
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

pub(super) fn prepare(model: &ResolvedModel, messages: &[Value], tools: &[Value], cache_key: &str) -> Result<Prepared> {
    let max_output = model.max_output.map(|n| n.clamp(1, 131_072));
    let completion_style = model.model.starts_with("gpt-5")
        || (model.model.starts_with('o') && model.model[1..].starts_with(|c: char| c.is_ascii_digit()));
    // Some gateways reject reasoning in history unless reasoning is switched on.
    let has_reasoning = || messages.iter().any(|m| REASONING_KEYS.iter().any(|k| m.get(*k).is_some()));
    let reasoning_effort = match &model.thinking {
        Thinking::Effort(effort) => Some(effort.as_str()),
        Thinking::On if has_reasoning() => Some("medium"),
        Thinking::On | Thinking::Off => None,
    };
    let request = Request {
        model: &model.model,
        messages,
        tools,
        stream: true,
        stream_options: StreamOptions { include_usage: true },
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

pub(super) struct Parser;

impl StreamParser for Parser {
    fn event(&mut self, event: &Value, reply: &mut Reply, on_delta: &mut dyn FnMut(Delta)) -> Result<(), Failure> {
        if let Some(error) = event.get("error").filter(|e| !e.is_null()) {
            let code = error.get("code").and_then(Value::as_u64).map(|c| c as u16);
            return Err(Failure::from_status(code, format!("provider error mid-stream: {}", truncate(&error.to_string(), 2000))));
        }
        if let Some(usage) = event.get("usage").filter(|u| u.is_object()) {
            reply.usage = parse_usage(usage);
        }
        let Some(choice) = event.get("choices").and_then(|c| c.get(0)) else { return Ok(()) };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            reply.finish_reason = Some(reason.to_string());
        }
        let Some(delta) = choice.get("delta") else { return Ok(()) };
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
        for call in delta.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
            let index = call.get("index").and_then(Value::as_u64).unwrap_or(reply.tool_calls.len() as u64) as usize;
            if reply.tool_calls.len() <= index {
                reply.tool_calls.resize_with(index + 1, ToolCall::default);
            }
            let slot = &mut reply.tool_calls[index];
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
mod tests {
    use super::*;

    #[test]
    fn stream_events_assemble_a_reply() {
        let mut reply = Reply::default();
        let mut sink = |_: Delta| {};
        let events = [
            r#"{"choices":[{"delta":{"reasoning_content":"think"}}]}"#,
            r#"{"choices":[{"delta":{"content":"Hi"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"pa"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":8}}}"#,
        ];
        let mut parser = Parser;
        for event in events {
            parser.event(&serde_json::from_str(event).unwrap(), &mut reply, &mut sink).ok().unwrap();
        }
        assert_eq!(reply.text, "Hi");
        assert_eq!(reply.reasoning, "think");
        assert_eq!(reply.reasoning_key, Some("reasoning_content"));
        assert_eq!(reply.tool_calls[0].name, "read");
        assert_eq!(reply.tool_calls[0].arguments, r#"{"path":"a"}"#);
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!((reply.usage.prompt, reply.usage.cached), (10, 8));
    }
}
