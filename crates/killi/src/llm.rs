//! OpenAI Chat Completions client: streaming, reasoning round-trip, retries.

use std::time::Duration;

use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::Value;

use crate::config::ResolvedModel;

const MAX_ATTEMPTS: u32 = 10;
pub const REASONING_KEYS: [&str; 3] = ["reasoning_content", "reasoning_details", "reasoning"];

pub struct Client {
    http: reqwest::Client,
    pub model: ResolvedModel,
}

#[derive(Debug, Default, Clone)]
pub struct Usage {
    pub prompt: u64,
    pub cached: u64,
    pub completion: u64,
    pub reasoning: u64,
}

#[derive(Debug, Default)]
pub struct Reply {
    pub text: String,
    pub reasoning: String,
    /// The field the provider used for reasoning, so it is sent back the same way.
    pub reasoning_key: Option<&'static str>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub usage: Usage,
}

#[derive(Debug, Default, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Exactly as the model streamed it; never re-serialized.
    pub arguments: String,
}

pub enum Delta<'a> {
    Text(&'a str),
    Retry { attempt: u32, wait: Duration, reason: &'a str },
}

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

struct Failure {
    retryable: bool,
    retry_after: Option<Duration>,
    message: String,
}

impl Failure {
    fn fatal(message: String) -> Self {
        Self { retryable: false, retry_after: None, message }
    }
    fn transient(message: String) -> Self {
        Self { retryable: true, retry_after: None, message }
    }
}

impl Client {
    pub fn new(model: ResolvedModel) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(300))
            .build()?;
        Ok(Self { http, model })
    }

    /// Sends one request, retrying transient failures. A failed attempt's partial output is
    /// discarded, so history only ever holds complete replies.
    pub async fn complete(
        &self,
        messages: &[Value],
        tools: &[Value],
        cache_key: &str,
        on_delta: &mut dyn FnMut(Delta),
    ) -> Result<Reply> {
        let body = serde_json::to_vec(&self.request(messages, tools, cache_key))?;
        let mut attempt = 1;
        loop {
            let failure = match self.attempt(&body, on_delta).await {
                Ok(reply) => return Ok(reply),
                Err(failure) => failure,
            };
            if !failure.retryable || attempt == MAX_ATTEMPTS {
                return Err(anyhow!("{} (after {attempt} attempt(s))", failure.message));
            }
            let wait = failure.retry_after.unwrap_or_else(|| backoff(attempt));
            on_delta(Delta::Retry { attempt, wait, reason: &failure.message });
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }

    fn request<'a>(&'a self, messages: &'a [Value], tools: &'a [Value], cache_key: &'a str) -> Request<'a> {
        let max_output = self.model.max_output.map(|n| n.clamp(1, 131_072));
        let completion_style = self.model.model.starts_with("gpt-5")
            || (self.model.model.starts_with('o') && self.model.model[1..].starts_with(|c: char| c.is_ascii_digit()));
        // Some gateways reject reasoning in history unless reasoning is switched on.
        let has_reasoning = || messages.iter().any(|m| REASONING_KEYS.iter().any(|k| m.get(*k).is_some()));
        let reasoning_effort = match &self.model.effort {
            Some(effort) => Some(effort.as_str()),
            None if has_reasoning() => Some("medium"),
            None => None,
        };
        Request {
            model: &self.model.model,
            messages,
            tools,
            stream: true,
            stream_options: StreamOptions { include_usage: true },
            prompt_cache_key: cache_key,
            max_tokens: max_output.filter(|_| !completion_style),
            max_completion_tokens: max_output.filter(|_| completion_style),
            reasoning_effort,
        }
    }

    async fn attempt(&self, body: &[u8], on_delta: &mut dyn FnMut(Delta)) -> Result<Reply, Failure> {
        let mut request = self
            .http
            .post(format!("{}/chat/completions", self.model.base_url))
            .bearer_auth(&self.model.api_key)
            .header("content-type", "application/json")
            .body(body.to_vec());
        for (name, value) in &self.model.headers {
            request = request.header(name, value);
        }
        let response = request.send().await.map_err(|e| Failure::transient(format!("request failed: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let text = response.text().await.unwrap_or_default();
            return Err(Failure {
                retryable: retryable_status(status),
                retry_after,
                message: format!("HTTP {status}: {}", truncate(&text, 2000)),
            });
        }

        let mut reply = Reply::default();
        let mut stream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        'stream: while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Failure::transient(format!("stream interrupted: {e}")))?;
            buffer.extend_from_slice(&chunk);
            while let Some(end) = buffer.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buffer.drain(..=end).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else { continue };
                let data = data.trim();
                if data == "[DONE]" {
                    break 'stream;
                }
                let Ok(event) = serde_json::from_str::<Value>(data) else { continue };
                apply_event(&event, &mut reply, on_delta)?;
            }
        }

        if reply.text.trim().is_empty() && reply.tool_calls.is_empty() {
            return Err(Failure::transient(match reply.reasoning.is_empty() {
                true => "empty response".into(),
                false => "response contained only reasoning".into(),
            }));
        }
        Ok(reply)
    }
}

fn apply_event(event: &Value, reply: &mut Reply, on_delta: &mut dyn FnMut(Delta)) -> Result<(), Failure> {
    if let Some(error) = event.get("error").filter(|e| !e.is_null()) {
        let code = error.get("code").and_then(Value::as_u64).and_then(|c| StatusCode::from_u16(c as u16).ok());
        let message = format!("provider error mid-stream: {}", truncate(&error.to_string(), 2000));
        return Err(match code {
            Some(status) if !retryable_status(status) => Failure::fatal(message),
            _ => Failure::transient(message),
        });
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

fn retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 425 | 429 | 500..=599)
}

/// 500 ms doubling to a 32 s cap, plus up to 25% jitter.
fn backoff(attempt: u32) -> Duration {
    let base = (500u64 << (attempt - 1).min(6)).min(32_000);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_nanos();
    let jitter = base * u64::from(nanos % 26) / 100;
    Duration::from_millis(base + jitter)
}

pub fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_a_cap() {
        assert!(backoff(1) >= Duration::from_millis(500) && backoff(1) <= Duration::from_millis(625));
        assert!(backoff(7) >= Duration::from_secs(32) && backoff(7) <= Duration::from_secs(40));
        assert!(backoff(10) <= Duration::from_secs(40));
    }

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
        for event in events {
            apply_event(&serde_json::from_str(event).unwrap(), &mut reply, &mut sink).ok().unwrap();
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
