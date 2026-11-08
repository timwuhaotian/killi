//! Command Code: the subscription behind the commandcode CLI's own gateway. Requests go
//! to `{base}/alpha/generate` in the CLI's wire format and come back as NDJSON — one JSON
//! object per line, not SSE — with `text-delta`, `reasoning-delta`, `tool-input-*`/`tool-call`
//! and `finish` events. History is kept in OpenAI chat format and converted the same way on
//! every request, so the bytes sent for earlier messages never change.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use ring::rand::SecureRandom;
use serde_json::{Map, Value, json};

use super::{
    Delta, ErrorKind, Failure, IMAGE_OMITTED, Prepared, Reply, StreamParser, ToolCall,
    arguments_object, cap_output, content_text, system_text, truncate,
};
use crate::config::{ResolvedModel, Thinking};

/// The CLI version the gateway sees. killi speaks the CLI's protocol, so it says the CLI's
/// version the way the CLI does; the login flow says it on its requests too.
pub(crate) const CLI_VERSION: &str = "1.88.5";

pub(super) fn prepare(
    model: &ResolvedModel,
    messages: &[Value],
    tools: &[Value],
    cache_key: &str,
) -> Result<Prepared> {
    let (system, history) = system_text(messages);
    let mut wire = Vec::new();
    // Every tool call's wire name by its id, so the tool results below name it the same way.
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for message in history {
        match message["role"].as_str() {
            Some("assistant") => {
                let mut content = Vec::new();
                if let Some(text) = message["reasoning_content"]
                    .as_str()
                    .filter(|text| !text.is_empty())
                {
                    content.push(json!({"type": "reasoning", "text": text}));
                }
                for item in message["reasoning_details"].as_array().into_iter().flatten() {
                    let text = detail_text(item);
                    if !text.is_empty() {
                        content.push(json!({"type": "reasoning", "text": text}));
                    }
                }
                let text = content_text(message);
                if !text.is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    let id = call["id"].as_str().unwrap_or_default();
                    let name = call["function"]["name"].as_str().unwrap_or("unknown");
                    let wire_name = model
                        .tool_names
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| name.to_string());
                    names.insert(id.to_string(), wire_name.clone());
                    content.push(json!({
                        "type": "tool-call",
                        "toolCallId": id,
                        "toolName": wire_name,
                        "input": arguments_object(call["function"]["arguments"].as_str().unwrap_or_default()),
                    }));
                }
                wire.push(json!({"role": "assistant", "content": content}));
            }
            Some("tool") => {
                let id = message["tool_call_id"].as_str().unwrap_or_default();
                let text = content_text(message);
                wire.push(json!({
                    "role": "tool",
                    "content": [{
                        "type": "tool-result",
                        "toolCallId": id,
                        "toolName": names.get(id).cloned().unwrap_or_else(|| "unknown".to_string()),
                        "output": {"type": "text", "value": if text.is_empty() { "(empty)".to_string() } else { text }},
                    }],
                }));
            }
            _ => {
                let mut content = Vec::new();
                match &message["content"] {
                    Value::String(text) if !text.is_empty() => {
                        content.push(json!({"type": "text", "text": text}));
                    }
                    Value::Array(parts) => {
                        for part in parts {
                            match part["type"].as_str() {
                                Some("text")
                                    if part["text"]
                                        .as_str()
                                        .is_some_and(|text| !text.is_empty()) =>
                                {
                                    content.push(json!({"type": "text", "text": part["text"]}));
                                }
                                Some("image_url") => content.push(match model.images {
                                    true => part["image_url"]["url"]
                                        .as_str()
                                        .map(image_block)
                                        .unwrap_or_else(|| {
                                            json!({"type": "text", "text": IMAGE_OMITTED})
                                        }),
                                    false => json!({"type": "text", "text": IMAGE_OMITTED}),
                                }),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                if !content.is_empty() {
                    wire.push(json!({"role": "user", "content": content}));
                }
            }
        }
    }
    let tools: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let function = &tool["function"];
            let name = function["name"].as_str().unwrap_or("unknown");
            json!({
                "name": model.tool_names.get(name).cloned().unwrap_or_else(|| name.to_string()),
                "description": function.get("description").cloned().unwrap_or(Value::String(String::new())),
                "input_schema": function.get("parameters").cloned().unwrap_or(json!({})),
            })
        })
        .collect();
    let system: Vec<Value> = (!system.is_empty())
        .then(|| json!({"type": "text", "text": system}))
        .into_iter()
        .collect();
    let max_tokens = model
        .max_output
        .map(|n| cap_output(model, messages, &tools, n).clamp(1, 131_072))
        .unwrap_or(64_000);
    let mut params = Map::new();
    params.insert("model".into(), model.model.clone().into());
    params.insert("messages".into(), wire.into());
    params.insert("tools".into(), tools.into());
    params.insert("system".into(), system.into());
    params.insert("max_tokens".into(), max_tokens.into());
    params.insert("stream".into(), true.into());
    if let Thinking::Effort(effort) = &model.thinking {
        params.insert("reasoning".into(), json!({"effort": effort}));
        params.insert("reasoning_effort".into(), effort.clone().into());
    }
    let thread = thread_id(cache_key);
    let body = json!({
        "config": {
            "workingDir": working_dir(),
            "date": today(),
            "environment": environment(),
            "structure": [],
            "isGitRepo": false,
            "currentBranch": "",
            "mainBranch": "",
            "gitStatus": "",
            "recentCommits": [],
        },
        "memory": Value::Null,
        "taste": Value::Null,
        "skills": Value::Null,
        "permissionMode": "auto-accept",
        "threadId": thread,
        "params": params,
    });
    Ok(Prepared {
        url: format!("{}/alpha/generate", model.base_url),
        body: serde_json::to_vec(&body)?,
        headers: vec![
            ("authorization".into(), format!("Bearer {}", model.api_key)),
            ("content-type".into(), "application/json".into()),
            // The CLI's own headers, which the gateway expects on its own API.
            ("user-agent".into(), "cli".into()),
            ("x-command-code-version".into(), CLI_VERSION.into()),
            ("x-cli-environment".into(), "production".into()),
            ("x-project-slug".into(), "private-killi".into()),
            ("x-taste-learning".into(), "false".into()),
            ("x-session-id".into(), thread),
            ("traceparent".into(), traceparent()?),
        ],
    })
}

/// An image part in the gateway's shape. Remote images have no bytes to send, so they arrive
/// as their URL in text instead.
fn image_block(url: &str) -> Value {
    match url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
    {
        Some((media_type, _)) => json!({"type": "image", "image": url, "mimeType": media_type}),
        None => json!({"type": "text", "text": format!("[image: {}]", truncate(url, 200))}),
    }
}

/// The replayable text of one structured reasoning item: scalar fields first, then summaries.
fn detail_text(item: &Value) -> String {
    let mut parts = Vec::new();
    for key in ["text", "thinking"] {
        if let Some(text) = item[key].as_str().filter(|text| !text.is_empty()) {
            parts.push(text);
        }
    }
    for part in item["summary"].as_array().into_iter().flatten() {
        if let Some(text) = part["text"].as_str().filter(|text| !text.is_empty()) {
            parts.push(text);
        }
    }
    parts.join("\n")
}

/// The thread and session id: the cache key hashed into a UUID, so a session's requests share
/// the gateway's thread the way the CLI's do.
fn thread_id(cache_key: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, cache_key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest.as_ref()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

fn traceparent() -> Result<String> {
    let mut bytes = [0u8; 24];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("the system's random source failed"))?;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("00-{}-{}-01", &hex[..32], &hex[32..]))
}

fn working_dir() -> String {
    std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// Today as `YYYY-MM-DD`, from days since the epoch (Howard Hinnant's civil date).
fn today() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() / 86400);
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    format!(
        "{:04}-{:02}-{:02}",
        if month <= 2 { year + 1 } else { year },
        month,
        day
    )
}

/// The OS name the gateway knows: what the CLI sends on each platform.
fn environment() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        _ => "linux",
    }
}

/// The most streamed tool calls one reply keeps, so a provider that floods or replays
/// indices can't grow it without bound.
const MAX_STREAMED_CALLS: usize = 128;

/// Error markers that end the subscription's allowance: retrying can't heal them.
const TERMINAL_MARKERS: [&str; 3] = [
    "premium_credits_exhausted",
    "model_not_in_plan",
    "insufficient credits",
];

#[derive(Default)]
pub(super) struct Parser {
    /// Each tool call's id to the reply slot it was first given.
    calls: BTreeMap<String, usize>,
    /// The provider's `tool_names` inverted: the name the API sends back to the session's.
    names: BTreeMap<String, String>,
    terminal: bool,
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

    /// The slot of the call `id` names, opened under the session's name when new.
    fn slot(&mut self, reply: &mut Reply, id: &str, wire: &str) -> Option<usize> {
        if let Some(slot) = self.calls.get(id) {
            return Some(*slot);
        }
        if id.is_empty() || reply.tool_calls.len() >= MAX_STREAMED_CALLS {
            return None;
        }
        let name = self
            .names
            .get(wire)
            .cloned()
            .unwrap_or_else(|| wire.to_string());
        let slot = reply.tool_calls.len();
        reply.tool_calls.push(ToolCall {
            id: id.to_string(),
            name,
            arguments: String::new(),
        });
        self.calls.insert(id.to_string(), slot);
        Some(slot)
    }
}

impl StreamParser for Parser {
    fn event(
        &mut self,
        event: &Value,
        reply: &mut Reply,
        on_delta: &mut dyn FnMut(Delta),
    ) -> Result<(), Failure> {
        match event["type"].as_str() {
            Some("text-delta") => {
                let text = event["text"].as_str().unwrap_or_default();
                reply.text.push_str(text);
                on_delta(Delta::Text(text));
            }
            // Named by the history field `prepare` replays reasoning from, so what the
            // gateway streamed is stored under it; an opened block that stays empty still
            // records which channel carried no reasoning.
            Some("reasoning-start") => {
                reply.reasoning_key.get_or_insert("reasoning_content");
            }
            Some("reasoning-delta") => {
                let text = event["text"].as_str().unwrap_or_default();
                reply.reasoning.push_str(text);
                reply.reasoning_key.get_or_insert("reasoning_content");
                on_delta(Delta::Reasoning(text));
            }
            Some("tool-input-start") => {
                let id = event["id"].as_str().unwrap_or_default();
                let wire = event["toolName"].as_str().unwrap_or_default();
                self.slot(reply, id, wire);
            }
            Some("tool-input-delta") => {
                let id = event["id"].as_str().unwrap_or_default();
                let delta = event["delta"].as_str().unwrap_or_default();
                if let Some(slot) = self.calls.get(id) {
                    reply.tool_calls[*slot].arguments.push_str(delta);
                    on_delta(Delta::Tool(delta));
                }
            }
            Some("tool-call") => {
                // The call carries its whole input: the deltas streamed it already, so only
                // what they haven't covered is appended, and a call with no deltas lands whole.
                let id = event["toolCallId"].as_str().unwrap_or_default();
                let wire = event["toolName"].as_str().unwrap_or_default();
                let Some(slot) = self.slot(reply, id, wire) else {
                    return Ok(());
                };
                let full = event
                    .get("input")
                    .map(|input| serde_json::to_string(input).unwrap_or_default())
                    .unwrap_or_default();
                let call = &mut reply.tool_calls[slot];
                if call.arguments.is_empty() {
                    if !full.is_empty() {
                        on_delta(Delta::Tool(&full));
                        call.arguments = full;
                    }
                } else if let Some(rest) = full.strip_prefix(call.arguments.as_str())
                    && !rest.is_empty()
                {
                    on_delta(Delta::Tool(rest));
                    call.arguments.push_str(rest);
                }
            }
            Some("finish-step" | "finish") => {
                self.terminal = true;
                let reason = event["finishReason"]
                    .as_str()
                    .or_else(|| event["rawFinishReason"].as_str())
                    .unwrap_or("stop");
                reply.finish_reason = Some(map_finish(reason).to_string());
                let usage = match event["type"].as_str() {
                    Some("finish") => &event["totalUsage"],
                    _ => &event["usage"],
                };
                reply.usage = parse_usage(usage);
            }
            Some("error") => return Err(stream_error(event)),
            Some("abort") => {
                return Err(Failure::transient(
                    "Command Code aborted the request".into(),
                ));
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
                "Command Code stream ended without a finish event".into(),
            ))
        }
    }
}

/// A finish reason in OpenAI's values; anything else passes through for `validate_reply`.
fn map_finish(reason: &str) -> &str {
    if reason.contains("tool") {
        "tool_calls"
    } else if reason.contains("length") || reason.contains("max") {
        "length"
    } else if reason.contains("stop") || reason.contains("end") {
        "stop"
    } else {
        reason
    }
}

fn parse_usage(usage: &Value) -> super::Usage {
    let num = |value: Option<&Value>| value.and_then(Value::as_u64).unwrap_or(0);
    super::Usage {
        prompt: num(usage.get("inputTokens")),
        cached: num(usage.pointer("/inputTokenDetails/cacheReadTokens"))
            .max(num(usage.get("cachedInputTokens"))),
        completion: num(usage.get("outputTokens")),
        reasoning: num(usage.pointer("/outputTokenDetails/reasoningTokens"))
            .max(num(usage.get("reasoningTokens"))),
    }
}

/// An `error` event's failure: the allowance markers and client errors stay fatal, a rate
/// limit and the rest may heal.
fn stream_error(event: &Value) -> Failure {
    let message = event["message"]
        .as_str()
        .or_else(|| event["error"]["message"].as_str())
        .or_else(|| event["error"].as_str())
        .unwrap_or("unknown error");
    let code = event["code"].as_str().unwrap_or_default();
    let status = event["statusCode"]
        .as_u64()
        .and_then(|code| u16::try_from(code).ok());
    let dump = event.to_string();
    if TERMINAL_MARKERS.iter().any(|marker| dump.contains(marker)) {
        return Failure::of(
            ErrorKind::Quota,
            status,
            false,
            format!("Command Code usage limit: {message}"),
        );
    }
    if status == Some(429) || code.contains("rate_limit") {
        return Failure::of(
            ErrorKind::RateLimited,
            status,
            true,
            format!("Command Code rate limit: {message}"),
        );
    }
    if let Some(status) = status {
        return Failure::from_status(Some(status), format!("Command Code error: {message}"));
    }
    match code {
        "invalid_request_error" | "authentication_error" | "permission_error"
        | "not_found_error" => Failure::fatal(format!("Command Code error: {message}")),
        _ => Failure::transient(format!("Command Code error: {message}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> ResolvedModel {
        ResolvedModel {
            alias: "commandcode/spark".into(),
            model: "meta/muse-spark-1.3-contributor".into(),
            protocol: crate::config::Protocol::CommandCode,
            base_url: "https://api.commandcode.ai".into(),
            api_key: "user_key".into(),
            headers: Default::default(),
            tool_names: Default::default(),
            session_header: None,
            max_output: None,
            context_window: None,
            images: true,
            thinking: Thinking::Effort("xhigh".into()),
            cache_control: false,
            chatgpt: false,
        }
    }

    fn prepared(messages: &[Value], tools: &[Value]) -> (String, Value, Vec<(String, String)>) {
        let prepared = prepare(&model(), messages, tools, "cache-key").unwrap();
        let body: Value = serde_json::from_slice(&prepared.body).unwrap();
        (prepared.url, body, prepared.headers)
    }

    #[test]
    fn the_request_carries_the_cli_wire_format_and_headers() {
        let (url, body, headers) = prepared(
            &[
                json!({"role": "system", "content": "sys"}),
                json!({"role": "user", "content": "hi"}),
            ],
            &[],
        );
        assert_eq!(url, "https://api.commandcode.ai/alpha/generate");
        assert_eq!(body["params"]["model"], "meta/muse-spark-1.3-contributor");
        assert_eq!(body["params"]["stream"], true);
        assert_eq!(body["params"]["max_tokens"], 64000);
        assert_eq!(body["params"]["reasoning_effort"], "xhigh");
        assert_eq!(body["params"]["system"], json!([{"type": "text", "text": "sys"}]));
        assert_eq!(
            body["params"]["messages"],
            json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}])
        );
        assert_eq!(body["permissionMode"], "auto-accept");
        assert_eq!(body["threadId"], body["threadId"]);
        let header = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        assert_eq!(header("authorization"), "Bearer user_key");
        assert_eq!(header("user-agent"), "cli");
        assert_eq!(header("x-command-code-version"), CLI_VERSION);
        assert_eq!(header("x-session-id"), body["threadId"].as_str().unwrap());
        assert!(header("traceparent").starts_with("00-"), "{headers:?}");
    }

    #[test]
    fn history_becomes_tool_calls_and_results() {
        let (_, body, _) = prepared(
            &[
                json!({"role": "system", "content": "sys"}),
                json!({"role": "user", "content": "read it"}),
                json!({"role": "assistant", "content": "on it", "reasoning_content": "plan",
                    "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a\"}"}}]}),
                json!({"role": "tool", "tool_call_id": "c1", "content": "data"}),
            ],
            &[json!({"type": "function", "function": {"name": "read", "description": "Read", "parameters": {"type": "object"}}})],
        );
        let messages = body["params"]["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages[1]["content"],
            json!([
                {"type": "reasoning", "text": "plan"},
                {"type": "text", "text": "on it"},
                {"type": "tool-call", "toolCallId": "c1", "toolName": "read", "input": {"path": "a"}},
            ])
        );
        assert_eq!(
            messages[2],
            json!({"role": "tool", "content": [
                {"type": "tool-result", "toolCallId": "c1", "toolName": "read",
                 "output": {"type": "text", "value": "data"}},
            ]})
        );
        assert_eq!(
            body["params"]["tools"],
            json!([{"name": "read", "description": "Read", "input_schema": {"type": "object"}}])
        );
    }

    #[test]
    fn images_ride_along_and_remote_ones_become_text() {
        let (_, body, _) = prepared(
            &[
                json!({"role": "user", "content": [
                    {"type": "text", "text": "look"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
                    {"type": "image_url", "image_url": {"url": "https://example/x.png"}},
                ]}),
            ],
            &[],
        );
        assert_eq!(
            body["params"]["messages"][0]["content"],
            json!([
                {"type": "text", "text": "look"},
                {"type": "image", "image": "data:image/png;base64,AAAA", "mimeType": "image/png"},
                {"type": "text", "text": "[image: https://example/x.png]"},
            ])
        );
    }

    #[test]
    fn the_stream_assembles_text_reasoning_calls_and_usage() {
        let mut parser = Parser::default();
        let mut reply = Reply::default();
        let mut sink = |_: Delta| {};
        let events = [
            json!({"type": "start"}),
            json!({"type": "reasoning-start", "id": "reasoning-0"}),
            json!({"type": "reasoning-delta", "id": "reasoning-0", "text": "hmm"}),
            json!({"type": "reasoning-end", "id": "reasoning-0"}),
            json!({"type": "text-start", "id": "txt-0"}),
            json!({"type": "text-delta", "id": "txt-0", "text": "reading "}),
            json!({"type": "tool-input-start", "id": "c1", "toolName": "read"}),
            json!({"type": "tool-input-delta", "id": "c1", "delta": "{\"path\":"}),
            json!({"type": "tool-input-end", "id": "c1"}),
            json!({"type": "tool-call", "toolCallId": "c1", "toolName": "read", "input": {"path": "a"}}),
            json!({"type": "text-end", "id": "txt-0"}),
            json!({"type": "finish", "finishReason": "tool-calls",
                "totalUsage": {"inputTokens": 50, "inputTokenDetails": {"cacheReadTokens": 5},
                "outputTokens": 7, "outputTokenDetails": {"reasoningTokens": 2}}}),
        ];
        for event in &events {
            parser.event(event, &mut reply, &mut sink).unwrap();
        }
        assert_eq!(reply.text, "reading ");
        assert_eq!(reply.reasoning, "hmm");
        assert_eq!(
            reply.reasoning_key,
            Some("reasoning_content"),
            "the history field `prepare` replays it from"
        );
        assert_eq!(reply.tool_calls.len(), 1);
        assert_eq!(reply.tool_calls[0].arguments, "{\"path\":\"a\"}");
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(
            (reply.usage.prompt, reply.usage.cached, reply.usage.completion, reply.usage.reasoning),
            (50, 5, 7, 2)
        );
        parser.finish(&mut reply).unwrap();
    }

    #[test]
    fn an_error_event_is_fatal_only_when_the_allowance_ends() {
        let mut sink = |_: Delta| {};
        let mut error = |event: Value| {
            let mut parser = Parser::default();
            parser
                .event(&event, &mut Reply::default(), &mut sink)
                .unwrap_err()
        };
        let fatal = error(json!({"type": "error", "message": "premium_credits_exhausted"}));
        assert!(!fatal.retryable && fatal.kind == ErrorKind::Quota, "{fatal:?}");
        let fatal = error(json!({"type": "error", "code": "invalid_request_error", "message": "bad"}));
        assert!(!fatal.retryable, "{fatal:?}");
        let paced = error(json!({"type": "error", "statusCode": 429, "message": "slow"}));
        assert!(paced.retryable && paced.kind == ErrorKind::RateLimited, "{paced:?}");
        assert!(error(json!({"type": "error", "message": "boom"})).retryable);
        assert!(Parser::default().finish(&mut Reply::default()).unwrap_err().retryable);
    }

    #[test]
    fn mapped_tool_names_go_out_and_come_back() {
        let mut with_names = model();
        with_names.tool_names = BTreeMap::from([("read".to_string(), "read_file".to_string())]);
        let prepared = prepare(
            &with_names,
            &[json!({"role": "user", "content": "go"})],
            &[json!({"function": {"name": "read", "description": "R", "parameters": {}}})],
            "k",
        )
        .unwrap();
        let body: Value = serde_json::from_slice(&prepared.body).unwrap();
        assert_eq!(body["params"]["tools"][0]["name"], "read_file");

        let mut parser = Parser::new(&with_names.tool_names);
        let mut reply = Reply::default();
        let mut sink = |_: Delta| {};
        parser
            .event(
                &json!({"type": "tool-call", "toolCallId": "c9", "toolName": "read_file", "input": {}}),
                &mut reply,
                &mut sink,
            )
            .unwrap();
        assert_eq!(reply.tool_calls[0].name, "read");
    }

    #[test]
    fn the_thread_is_stable_per_session_and_today_is_a_date() {
        assert_eq!(thread_id("k"), thread_id("k"));
        assert_ne!(thread_id("k"), thread_id("j"));
        let id = thread_id("k");
        assert_eq!(id.len(), 36, "{id}");
        let date = today();
        assert_eq!(date.len(), 10, "{date}");
        assert!(date.starts_with("202"), "{date}");
    }
}
