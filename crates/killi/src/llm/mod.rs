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

/// How killi names itself to the servers it calls.
pub const USER_AGENT: &str = concat!("killi/", env!("CARGO_PKG_VERSION"));

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
    /// signatures, OpenAI Chat reasoning details, or OpenAI Responses reasoning items.
    pub reasoning_items: Vec<Value>,
    pub tool_calls: Vec<ToolCall>,
    /// Normalized to OpenAI's values: `stop`, `length`, `tool_calls`.
    pub finish_reason: Option<String>,
    pub usage: Usage,
}

impl Reply {
    /// Whether anything came back to keep on either reasoning channel: the scalar text or the
    /// structured items, whichever dialect the provider spoke. The channels must be judged
    /// together — an items-only reply kept its reasoning with no scalar text at all.
    pub fn reasoning_kept(&self) -> bool {
        !self.reasoning.is_empty() || !self.reasoning_items.is_empty()
    }
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
    fn done(&mut self) {}
    fn finish(&mut self, reply: &mut Reply) -> Result<(), Failure>;
}

#[derive(Debug)]
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

impl From<crate::login::chatgpt::TokenError> for Failure {
    fn from(error: crate::login::chatgpt::TokenError) -> Self {
        match error {
            crate::login::chatgpt::TokenError::SignIn(message) => Failure::fatal(message),
            crate::login::chatgpt::TokenError::Transient(message) => Failure::transient(message),
        }
    }
}

impl Client {
    pub fn new(model: ResolvedModel) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
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
        let mut prepared = match self.model.protocol {
            Protocol::OpenAi => openai::prepare(&self.model, messages, tools, cache_key),
            Protocol::Anthropic => anthropic::prepare(&self.model, messages, tools),
            Protocol::Responses => responses::prepare(&self.model, messages, tools, cache_key),
        }?;
        if let Some(name) = &self.model.session_header {
            prepared.headers.push((name.clone(), cache_key.to_string()));
        }
        let mut attempt = 1;
        let mut renewed = false;
        loop {
            let failure = match self.attempt(&prepared, &mut renewed, on_delta).await {
                Ok(reply) => return Ok(reply),
                Err(failure) => failure,
            };
            if !failure.retryable || attempt == MAX_ATTEMPTS {
                let advice = if self.model.chatgpt { crate::login::chatgpt::advice(&failure.message) } else { String::new() };
                return Err(anyhow!("{} (after {attempt} attempt(s)){advice}", failure.message));
            }
            let wait = failure.retry_after.unwrap_or_else(|| backoff(attempt));
            on_delta(Delta::Retry { attempt, wait, reason: &failure.message });
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }

    /// One request. On a ChatGPT plan it carries the current token, and the first 401 of a
    /// `complete` call refreshes that token and is retried at once (`renewed` records it).
    async fn attempt(&self, prepared: &Prepared, renewed: &mut bool, on_delta: &mut dyn FnMut(Delta)) -> Result<Reply, Failure> {
        let mut request = self.http.post(&prepared.url).body(prepared.body.clone());
        let token = match self.model.chatgpt {
            true => Some(crate::login::chatgpt::access_token(&self.http, None).await?),
            false => None,
        };
        if let Some(token) = &token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
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
            if let Some(token) = token.as_deref().filter(|_| status == StatusCode::UNAUTHORIZED && !*renewed) {
                crate::login::chatgpt::access_token(&self.http, Some(token)).await?;
                *renewed = true;
                let message = format!("HTTP {status}: the ChatGPT token was turned away, so it was refreshed");
                return Err(Failure { retryable: true, retry_after: Some(Duration::ZERO), message });
            }
            return Err(classify(status, retry_after, &text));
        }

        let mut parser: Box<dyn StreamParser> = match self.model.protocol {
            Protocol::OpenAi => Box::<openai::Parser>::default(),
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
                let Some(event) = parse_sse_data(data)? else {
                    parser.done();
                    break 'stream;
                };
                parser.event(&event, &mut reply, on_delta)?;
            }
        }
        ensure_stream_consumed(&buffer)?;
        parser.finish(&mut reply)?;
        validate_reply(reply)
    }
}

fn parse_sse_data(data: &str) -> Result<Option<Value>, Failure> {
    if data == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(data)
        .map(Some)
        .map_err(|e| Failure::transient(format!("malformed SSE JSON: {e}")))
}

fn ensure_stream_consumed(buffer: &[u8]) -> Result<(), Failure> {
    if buffer.iter().all(|b| b.is_ascii_whitespace()) {
        Ok(())
    } else {
        Err(Failure::transient("stream ended with unconsumed data".into()))
    }
}

/// Only normal termination and an output-limit reply are usable; in particular, a content
/// filter must not turn a partial or reasoning-only attempt into a committed assistant turn.
fn validate_reply(reply: Reply) -> Result<Reply, Failure> {
    if let Some(reason) = reply.finish_reason.as_deref()
        && !matches!(reason, "stop" | "length" | "tool_calls")
    {
        return Err(Failure::fatal(format!("response terminated abnormally: {reason}")));
    }
    if reply.text.trim().is_empty() && reply.tool_calls.is_empty() && reply.finish_reason.is_none() {
        return Err(Failure::transient(match reply.reasoning.is_empty() {
            true => "empty response without a termination reason".into(),
            false => "reasoning-only response without a termination reason".into(),
        }));
    }
    Ok(reply)
}

/// Tokens reserved per inline image: its base64 is about a hundred times what it bills.
pub(crate) const IMAGE_TOKEN_RESERVE: u64 = 2_048;

/// Estimated serialized input size, with inline image data replaced by a fixed per-image
/// reserve instead of treating base64 as text. This only caps output; compaction has its own
/// estimate in `agent.rs`, which reserves the same per image.
fn estimated_value_bytes(value: &Value) -> u64 {
    match value {
        Value::Null => 4,
        Value::Bool(value) => 4 + u64::from(!*value),
        Value::Number(value) => value.to_string().len() as u64,
        Value::String(value) if value.starts_with("data:") && value.contains(";base64,") => IMAGE_TOKEN_RESERVE * 4,
        Value::String(value) => value.len() as u64 + 2,
        Value::Array(values) => values
            .iter()
            .fold(2, |total, value| total.saturating_add(estimated_value_bytes(value)).saturating_add(1)),
        Value::Object(values) => values.iter().fold(2, |total, (key, value)| {
            let value_bytes = if key == "data" && values.get("type").and_then(Value::as_str) == Some("base64") {
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
fn estimated_input_tokens(messages: &[Value], tools: &[Value]) -> u64 {
    let bytes = messages.iter().chain(tools).fold(0u64, |total, value| {
        total.saturating_add(estimated_value_bytes(value))
    });
    bytes.saturating_add(3) / 4
}

const OUTPUT_SAFETY_TOKENS: u64 = 256;

fn cap_output(model: &ResolvedModel, messages: &[Value], tools: &[Value], max: u64) -> u64 {
    let Some(window) = model.context_window else { return max.max(1) };
    let reserved = estimated_input_tokens(messages, tools).saturating_add(OUTPUT_SAFETY_TOKENS);
    max.min(window.saturating_sub(reserved)).max(1)
}

fn retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 425 | 429 | 500..=599)
}

/// A server asking for a longer wait than this isn't worth sitting through.
const LONGEST_WAIT: Duration = Duration::from_secs(60);

/// Words in a 429 body that say a usage quota ran out, not that requests came too fast.
const QUOTA_WORDS: [&str; 5] = ["usage limit", "quota", "reset at", "exceeded your current", "usage_limit_exceeded"];

/// What a failed HTTP response means: retried, unless waiting won't help. A spent quota
/// (a 5-hour or monthly limit) resets in hours, so it fails at once with the server's message.
fn classify(status: StatusCode, retry_after: Option<Duration>, body: &str) -> Failure {
    let lower = body.to_lowercase();
    let quota = status == StatusCode::TOO_MANY_REQUESTS && QUOTA_WORDS.iter().any(|w| lower.contains(w));
    let too_long = retry_after.is_some_and(|wait| wait > LONGEST_WAIT);
    Failure {
        retryable: retryable_status(status) && !quota && !too_long,
        retry_after,
        message: format!("HTTP {status}: {}", truncate(body, 2000)),
    }
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
    fn a_spent_quota_is_not_retried() {
        let body = r#"{"error":{"code":"1308","message":"Usage limit reached for 5 hour. Your limit will reset at 2026-10-22 16:06:02"}}"#;
        let failure = classify(StatusCode::TOO_MANY_REQUESTS, None, body);
        assert!(!failure.retryable);
        assert!(failure.message.contains("reset at 2026-10-22 16:06:02"));
    }

    #[test]
    fn a_spent_chatgpt_plan_is_not_retried() {
        let body = r#"{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"Try again later"}}"#;
        assert!(!classify(StatusCode::TOO_MANY_REQUESTS, None, body).retryable);
    }

    #[test]
    fn rate_limits_and_server_errors_are_retried() {
        assert!(classify(StatusCode::TOO_MANY_REQUESTS, Some(Duration::from_secs(5)), "slow down").retryable);
        assert!(classify(StatusCode::BAD_GATEWAY, None, "bad gateway").retryable);
        assert!(!classify(StatusCode::BAD_REQUEST, None, "bad request").retryable);
    }

    #[test]
    fn a_long_retry_after_is_not_waited_out() {
        assert!(!classify(StatusCode::SERVICE_UNAVAILABLE, Some(Duration::from_secs(3600)), "down").retryable);
        assert!(classify(StatusCode::SERVICE_UNAVAILABLE, Some(Duration::from_secs(60)), "down").retryable);
    }

    #[test]
    fn backoff_doubles_to_a_cap() {
        assert!(backoff(1) >= Duration::from_millis(500) && backoff(1) <= Duration::from_millis(625));
        assert!(backoff(7) >= Duration::from_secs(32) && backoff(7) <= Duration::from_secs(40));
        assert!(backoff(10) <= Duration::from_secs(40));
    }

    #[test]
    fn malformed_json_and_unconsumed_stream_tails_are_transient() {
        assert!(parse_sse_data("{not json}").unwrap_err().retryable);
        assert!(parse_sse_data("[DONE]").unwrap().is_none());
        assert!(ensure_stream_consumed(b" \r\n").is_ok());
        assert!(ensure_stream_consumed(br#"data: {"unfinished":"#).unwrap_err().retryable);
    }

    #[test]
    fn empty_replies_follow_their_termination_reason() {
        let empty = |reason: Option<&str>| Reply {
            finish_reason: reason.map(str::to_string),
            ..Reply::default()
        };
        assert!(validate_reply(empty(Some("stop"))).is_ok());
        let reasoning_only = Reply { reasoning: "plan".into(), finish_reason: Some("length".into()), ..Reply::default() };
        assert!(validate_reply(reasoning_only).is_ok());
        assert_eq!(validate_reply(empty(Some("length"))).unwrap().finish_reason.as_deref(), Some("length"));
        assert!(validate_reply(empty(None)).unwrap_err().retryable);
        assert!(!validate_reply(empty(Some("content_filter"))).unwrap_err().retryable);
        assert!(!validate_reply(empty(Some("refusal"))).unwrap_err().retryable);
    }

    #[test]
    fn input_estimate_reserves_an_image_instead_of_counting_base64_as_text() {
        let image = serde_json::json!({
            "role": "user",
            "content": [{"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{}", "A".repeat(1_000_000))}}]
        });
        let estimate = estimated_input_tokens(&[image], &[]);
        assert!((IMAGE_TOKEN_RESERVE..IMAGE_TOKEN_RESERVE + 100).contains(&estimate), "{estimate}");
    }

    #[test]
    fn output_cap_reserves_estimated_input_and_never_reaches_zero() {
        assert_eq!(estimated_input_tokens(&[serde_json::json!(1234)], &[]), 1);
        assert_eq!(estimated_input_tokens(&[serde_json::json!(12345)], &[]), 2);
        let model = ResolvedModel {
            alias: String::new(),
            model: String::new(),
            protocol: Protocol::OpenAi,
            base_url: String::new(),
            api_key: String::new(),
            headers: Default::default(),
            session_header: None,
            max_output: Some(8_000),
            context_window: Some(2_000),
            images: false,
            thinking: crate::config::Thinking::Off,
            cache_control: false,
            chatgpt: false,
        };
        let messages = vec![serde_json::json!({"role": "user", "content": "x".repeat(4_000)})];
        let input = estimated_input_tokens(&messages, &[]);
        assert_eq!(cap_output(&model, &messages, &[], 8_000), 2_000 - input - OUTPUT_SAFETY_TOKENS);
        assert_eq!(cap_output(&model, &messages, &[], 100), 100);
        assert_eq!(cap_output(&model, &messages, &[], 0), 1);

        let mut unknown = model;
        unknown.context_window = None;
        assert_eq!(cap_output(&unknown, &messages, &[], 8_000), 8_000);
        unknown.context_window = Some(1);
        assert_eq!(cap_output(&unknown, &messages, &[], 8_000), 1);
    }
}
