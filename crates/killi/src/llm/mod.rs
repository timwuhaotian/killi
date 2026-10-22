//! Provider clients: one streaming request with retries, in each provider's wire format.
//! History is kept in OpenAI chat format and converted the same way on every request, so
//! the bytes sent for earlier messages never change.

mod anthropic;
mod openai;
mod responses;

use std::time::Duration;

use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::Value;

use crate::config::{Protocol, ResolvedModel};

pub use openai::REASONING_KEYS;

const MAX_ATTEMPTS: u32 = 10;

#[derive(Clone)]
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
    /// The field an OpenAI-compatible provider used for reasoning, so it is sent back the same way.
    pub reasoning_key: Option<&'static str>,
    /// Provider-specific reasoning to replay verbatim: Anthropic thinking blocks with their
    /// signatures, or OpenAI Responses reasoning items.
    pub reasoning_items: Vec<Value>,
    pub tool_calls: Vec<ToolCall>,
    /// Normalized to OpenAI's values: `stop`, `length`, `tool_calls`.
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
    Reasoning(&'a str),
    Retry { attempt: u32, wait: Duration, reason: &'a str },
}

/// A request ready to send: where, what, and with which headers.
struct Prepared {
    url: String,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

/// The state a protocol keeps while it reads one streamed reply.
trait StreamParser {
    fn event(&mut self, event: &Value, reply: &mut Reply, on_delta: &mut dyn FnMut(Delta)) -> Result<(), Failure>;
    fn finish(&mut self, _reply: &mut Reply) {}
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
    fn from_status(status: Option<u16>, message: String) -> Self {
        match status.and_then(|s| StatusCode::from_u16(s).ok()) {
            Some(status) if !retryable_status(status) => Failure::fatal(message),
            _ => Failure::transient(message),
        }
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
        let prepared = match self.model.protocol {
            Protocol::OpenAi => openai::prepare(&self.model, messages, tools, cache_key),
            Protocol::Anthropic => anthropic::prepare(&self.model, messages, tools),
            Protocol::Responses => responses::prepare(&self.model, messages, tools, cache_key),
        }?;
        let mut attempt = 1;
        loop {
            let failure = match self.attempt(&prepared, on_delta).await {
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

    async fn attempt(&self, prepared: &Prepared, on_delta: &mut dyn FnMut(Delta)) -> Result<Reply, Failure> {
        let mut request = self.http.post(&prepared.url).body(prepared.body.clone());
        for (name, value) in &prepared.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        for (name, value) in &self.model.headers {
            request = request.header(name.as_str(), value.as_str());
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

        let mut parser: Box<dyn StreamParser> = match self.model.protocol {
            Protocol::OpenAi => Box::new(openai::Parser),
            Protocol::Anthropic => Box::<anthropic::Parser>::default(),
            Protocol::Responses => Box::<responses::Parser>::default(),
        };
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
                parser.event(&event, &mut reply, on_delta)?;
            }
        }
        parser.finish(&mut reply);

        if reply.text.trim().is_empty() && reply.tool_calls.is_empty() {
            return Err(Failure::transient(match reply.reasoning.is_empty() {
                true => "empty response".into(),
                false => "response contained only reasoning".into(),
            }));
        }
        Ok(reply)
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

/// The system prompt: the first message, which is always the system message.
fn system_text(messages: &[Value]) -> (&str, &[Value]) {
    match messages.first() {
        Some(first) if first["role"] == "system" => (first["content"].as_str().unwrap_or_default(), &messages[1..]),
        _ => ("", messages),
    }
}

/// A message's text, whether `content` is a string or a list of parts.
fn content_text(message: &Value) -> String {
    match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// Tool-call arguments as a JSON object, or an empty object when they aren't one.
fn arguments_object(arguments: &str) -> Value {
    match serde_json::from_str::<Value>(arguments) {
        Ok(value @ Value::Object(_)) => value,
        _ => Value::Object(Default::default()),
    }
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
}
