//! OpenAI Chat Completions: history keeps its stored form; structured reasoning is converted on a copy.

use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};

use super::{Delta, Failure, Prepared, Reply, StreamParser, ToolCall, Usage, cap_output, truncate};
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
    let max_output = model.max_output.map(|n| cap_output(model, messages, tools, n).clamp(1, 131_072));
    let mut messages = replay_history(messages);
    if model.cache_control {
        mark_cache_breakpoints(&mut messages);
    }
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
        messages: &messages,
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
            message["content"] = json!([{"type": "text", "text": text.clone(), "cache_control": mark}]);
        }
        Value::Array(blocks) => {
            if let Some(block) = blocks.iter_mut().rev().find(|b| b["type"] == "text") {
                block["cache_control"] = mark;
            }
        }
        _ => {}
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
                    .filter(|item| !matches!(item["type"].as_str(), Some("reasoning" | "thinking" | "redacted_thinking")))
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

#[derive(Default)]
pub(super) struct Parser {
    done: bool,
}

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
        if delta.get("refusal").and_then(Value::as_str).is_some_and(|text| !text.is_empty()) {
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
        for detail in delta.get("reasoning_details").and_then(Value::as_array).into_iter().flatten() {
            merge_reasoning_detail(reply, detail);
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

    fn done(&mut self) {
        self.done = true;
    }

    fn finish(&mut self, reply: &mut Reply) -> Result<(), Failure> {
        if self.done || reply.finish_reason.is_some() {
            Ok(())
        } else {
            Err(Failure::transient("OpenAI stream ended without finish_reason or [DONE]".into()))
        }
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
                reply.reasoning_items.iter().position(|item| same_type(item) && item["id"].as_str() == Some(id))
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
        let append = matches!(key.as_str(), "text" | "summary" | "data" | "encrypted_content" | "signature");
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
mod tests {
    use super::*;
    use serde_json::json;

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
        let mut parser = Parser::default();
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
        parser.finish(&mut reply).unwrap();
    }

    #[test]
    fn done_is_a_terminator_but_an_unterminated_stream_is_not() {
        let mut parser = Parser::default();
        let mut reply = Reply::default();
        assert!(parser.finish(&mut reply).unwrap_err().retryable);
        parser.done();
        parser.finish(&mut reply).unwrap();
    }

    #[test]
    fn a_refusal_delta_is_fatal() {
        let mut parser = Parser::default();
        let mut reply = Reply::default();
        let mut sink = |_: Delta| {};
        let event = json!({"choices":[{"delta":{"refusal":"cannot help"}}]});
        assert!(!parser.event(&event, &mut reply, &mut sink).unwrap_err().retryable);
    }

    #[test]
    fn reasoning_detail_deltas_accumulate_by_item() {
        let mut parser = Parser::default();
        let mut reply = Reply::default();
        let mut sink = |_: Delta| {};
        let events = [
            json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","index":0,"format":"x","text":"think "}]}}]}),
            json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","index":0,"format":"x","text":"more"}]}}]}),
            json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","index":1,"text":"other"}]}}]}),
        ];
        for event in &events {
            parser.event(event, &mut reply, &mut sink).unwrap();
        }
        assert_eq!(reply.reasoning_items.len(), 2);
        assert_eq!(reply.reasoning_items[0]["text"], "think more");
        assert_eq!(reply.reasoning_items[0]["format"], "x");
        assert_eq!(reply.reasoning_items[1]["text"], "other");
        assert!(reply.reasoning.is_empty());
    }

    #[test]
    fn reasoning_detail_types_with_one_index_stay_separate_and_null_ids_continue_a_block() {
        let mut reply = Reply::default();
        merge_reasoning_detail(&mut reply, &json!({"type":"reasoning.summary","index":0,"summary":"think "}));
        merge_reasoning_detail(&mut reply, &json!({"type":"reasoning.encrypted","index":0,"data":"cipher"}));
        merge_reasoning_detail(&mut reply, &json!({"type":"reasoning.summary","index":0,"summary":"more"}));
        assert_eq!(reply.reasoning_items.len(), 2);
        assert_eq!(reply.reasoning_items[0]["summary"], "think more");
        assert!(reply.reasoning_items[0].get("data").is_none());
        assert_eq!(reply.reasoning_items[1]["data"], "cipher");

        merge_reasoning_detail(&mut reply, &json!({"type":"summary","id":null,"summary":"part "}));
        merge_reasoning_detail(&mut reply, &json!({"type":"summary","id":null,"summary":"two"}));
        assert_eq!(reply.reasoning_items[2]["summary"], "part two");
    }

    #[test]
    fn kimi_reasoning_items_drop_stream_indexes_when_replayed() {
        let messages = vec![json!({
            "role": "assistant",
            "reasoning_items": [
                {"type": "summary", "index": 0, "summary": "plan"},
                {"type": "encrypted", "index": 0, "data": "cipher"},
                {"type": "reasoning.text", "index": 0, "text": "keep"}
            ]
        })];
        let details = replay_history(&messages)[0]["reasoning_details"].clone();
        assert!(details[0].get("index").is_none());
        assert!(details[1].get("index").is_none());
        assert_eq!(details[2]["index"], 0);
    }

    #[test]
    fn reasoning_items_replay_as_details_without_mutating_history() {
        let messages = vec![json!({
            "role": "assistant",
            "reasoning_items": [
                {"type": "reasoning.text", "index": 0, "text": "plan"},
                {"type": "reasoning", "encrypted_content": "responses-only"}
            ],
            "content": "ok"
        })];
        let original = messages.clone();
        let replayed = replay_history(&messages);
        assert_eq!(messages, original);
        assert_eq!(replayed[0]["reasoning_details"], json!([{"type": "reasoning.text", "index": 0, "text": "plan"}]));
        assert!(replayed[0].get("reasoning_items").is_none());
    }

    #[test]
    fn cache_breakpoints_mark_the_system_and_newest_message_on_the_copy_only() {
        let history = vec![
            json!({"role": "system", "content": "rules"}),
            json!({"role": "user", "content": "run it"}),
            json!({"role": "assistant", "content": null, "tool_calls": [{"id": "c", "type": "function", "function": {"name": "bash", "arguments": "{}"}}]}),
            json!({"role": "tool", "tool_call_id": "c", "content": "done"}),
        ];
        let original = history.clone();
        let mut sent = history.clone();
        mark_cache_breakpoints(&mut sent);
        let mark = json!({"type": "ephemeral"});
        assert_eq!(sent[0]["content"], json!([{"type": "text", "text": "rules", "cache_control": mark}]));
        assert_eq!(sent[1]["content"], "run it");
        assert!(sent[2]["content"].is_null());
        assert_eq!(sent[3]["content"], json!([{"type": "text", "text": "done", "cache_control": mark}]));
        assert_eq!(history, original);
        assert_eq!(sent.iter().filter(|m| m["content"].is_array()).count(), 2);
    }

    #[test]
    fn cache_breakpoints_go_on_the_last_text_block_and_skip_images() {
        let mut sent = vec![json!({"role": "user", "content": [
            {"type": "text", "text": "look"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA"}},
        ]})];
        mark_cache_breakpoints(&mut sent);
        assert_eq!(sent[0]["content"][0]["cache_control"], json!({"type": "ephemeral"}));
        assert!(sent[0]["content"][1].get("cache_control").is_none());
    }

    #[test]
    fn prepare_marks_breakpoints_only_when_the_model_asks() {
        let mut model = ResolvedModel {
            alias: "m".into(),
            model: "claude".into(),
            protocol: crate::config::Protocol::OpenAi,
            base_url: "http://localhost".into(),
            api_key: String::new(),
            headers: Default::default(),
            max_output: None,
            context_window: None,
            images: false,
            thinking: Thinking::Off,
            cache_control: false,
        };
        let messages = vec![json!({"role": "system", "content": "rules"}), json!({"role": "user", "content": "hi"})];
        let plain: Value = serde_json::from_slice(&prepare(&model, &messages, &[], "k").unwrap().body).unwrap();
        assert_eq!(plain["messages"][1]["content"], "hi");
        model.cache_control = true;
        let marked: Value = serde_json::from_slice(&prepare(&model, &messages, &[], "k").unwrap().body).unwrap();
        assert_eq!(marked["messages"][1]["content"][0]["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(messages[1]["content"], "hi");
    }

    #[test]
    fn prepare_caps_output_from_context_and_preserves_string_reasoning() {
        let model = ResolvedModel {
            alias: "m".into(),
            model: "gpt-4".into(),
            protocol: crate::config::Protocol::OpenAi,
            base_url: "http://localhost".into(),
            api_key: String::new(),
            headers: Default::default(),
            max_output: Some(2_000),
            context_window: Some(2_000),
            images: false,
            thinking: Thinking::Off,
            cache_control: false,
        };
        let messages = vec![
            json!({"role": "user", "content": "hello"}),
            json!({"role": "assistant", "reasoning_content": "plan", "content": "ok"}),
        ];
        let prepared = prepare(&model, &messages, &[], "cache").unwrap();
        let body: Value = serde_json::from_slice(&prepared.body).unwrap();
        let expected = super::cap_output(&model, &messages, &[], 2_000);
        assert_eq!(body["max_tokens"], expected);
        assert_eq!(body["messages"][1]["reasoning_content"], "plan");
        assert!(body.get("max_completion_tokens").is_none());
    }
}
