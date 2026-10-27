//! Provider clients: one streaming request with retries, in each provider's wire format.
//! History is kept in OpenAI chat format and converted the same way on every request, so
//! the bytes sent for earlier messages never change.

mod anthropic;
mod openai;
mod responses;

use std::cell::Cell;
use std::fmt;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::Value;

use crate::config::{Protocol, ResolvedModel};

pub use openai::REASONING_KEYS;

const MAX_ATTEMPTS: u32 = 10;

/// Requests that never reach a response head stop early: a refused port or an unresolvable
/// host won't heal in the time ten attempts take.
const TRANSPORT_ATTEMPTS: u32 = 3;

/// Requests one `complete` may send in all, however many of its waits were shared with other
/// members of a swarm, so peers pushing the cooldown back can't keep it alive forever.
const HARD_ATTEMPTS: u32 = 2 * MAX_ATTEMPTS;

/// Body bytes kept from a response, enough for an error message's excerpt.
const EXCERPT_BYTES: usize = 8_192;

/// How killi names itself to the servers it calls.
pub const USER_AGENT: &str = concat!("killi/", env!("CARGO_PKG_VERSION"));

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    pub model: ResolvedModel,
    /// Set by a 429 any clone of this client hit: subagents share one client, so a rate
    /// limit paces them together instead of each burning its own retries on the same cap.
    cooldown: Rc<Cell<Option<Instant>>>,
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

/// The bytes of a chunked event stream, drained one `\n`-terminated line at a time. Bytes
/// already scanned with no newline in them are not scanned again, so a multi-megabyte line
/// arriving in many chunks costs one pass, not one per chunk.
#[derive(Default)]
pub(crate) struct SseLines {
    buffer: Vec<u8>,
    scan: usize,
}

impl SseLines {
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// The next complete line, without its newline, or `None` when no newline arrived yet.
    pub(crate) fn next_line(&mut self) -> Option<Vec<u8>> {
        let Some(rel) = self.buffer[self.scan..].iter().position(|&b| b == b'\n') else {
            self.scan = self.buffer.len();
            return None;
        };
        let end = self.scan + rel;
        let line: Vec<u8> = self.buffer.drain(..=end).collect();
        self.scan = 0;
        Some(line)
    }

    /// What arrived after the last newline: checked for a torn tail once the stream ends.
    pub(crate) fn rest(&mut self) -> &[u8] {
        self.scan = self.buffer.len();
        &self.buffer
    }
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

/// Why a provider request failed, so callers never have to re-classify message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The conversation no longer fits the context window.
    Overflow,
    /// The credentials were rejected.
    Auth,
    /// A usage allowance ran out and resets on its own schedule.
    Quota,
    /// Requests came too fast.
    RateLimited,
    /// The provider rejected an image or other media.
    Media,
    /// The request never reached a usable response: DNS, connect, TLS.
    Transport,
    /// The response wasn't the event stream the protocol speaks.
    BadResponse,
    /// Anything else.
    Other,
}

/// A provider request that failed after every retry, classified once where the response was
/// read. Its `Display` is the message the user has always seen.
#[allow(dead_code)]
#[derive(Debug)]
pub struct ProviderError {
    /// The HTTP status, when the failure had one.
    pub status: Option<u16>,
    /// What kind of failure this was.
    pub kind: ErrorKind,
    /// The server's or transport's message, as shown to the user.
    pub message: String,
    /// Requests made, including the failed one.
    pub attempts: u32,
    /// Guidance appended after the attempt count, such as the ChatGPT plan's sign-in hint.
    pub(crate) advice: String,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (after {} attempt(s)){}", self.message, self.attempts, self.advice)
    }
}

impl std::error::Error for ProviderError {}

#[derive(Debug)]
struct Failure {
    kind: ErrorKind,
    status: Option<u16>,
    retryable: bool,
    retry_after: Option<Duration>,
    /// A retry budget smaller than `MAX_ATTEMPTS`, for failures that won't heal in time.
    max_attempts: Option<u32>,
    message: String,
}

impl Failure {
    fn of(kind: ErrorKind, status: Option<u16>, retryable: bool, message: String) -> Self {
        Self { kind, status, retryable, retry_after: None, max_attempts: None, message }
    }
    fn fatal(message: String) -> Self {
        Self::of(ErrorKind::Other, None, false, message)
    }
    fn transient(message: String) -> Self {
        Self::of(ErrorKind::Other, None, true, message)
    }
    fn from_status(status: Option<u16>, message: String) -> Self {
        let retryable = status
            .and_then(|status| StatusCode::from_u16(status).ok())
            .is_none_or(retryable_status);
        Self::of(kind_for_status(status), status, retryable, message)
    }
}

impl From<crate::login::chatgpt::TokenError> for Failure {
    fn from(error: crate::login::chatgpt::TokenError) -> Self {
        match error {
            crate::login::chatgpt::TokenError::SignIn(message) => Failure::of(ErrorKind::Auth, None, false, message),
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
        Ok(Self { http, model, cooldown: Rc::new(Cell::new(None)) })
    }

    /// Sends one request, retrying transient failures. A failed attempt's partial output is
    /// discarded, so history only ever holds complete replies. Every attempt first waits out
    /// a 429 cooldown shared with the other members of a swarm.
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
        let mut burns = 0;
        let mut mine = None;
        loop {
            let waited_peer = self.wait_cooldown(mine).await;
            let failure = match self.attempt(&prepared, &mut renewed, on_delta).await {
                Ok(reply) => return Ok(reply),
                Err(failure) => failure,
            };
            if !waited_peer {
                burns += 1;
            }
            let cap = failure.max_attempts.unwrap_or(MAX_ATTEMPTS);
            let spent = if failure.max_attempts.is_some() { attempt } else { burns };
            if !failure.retryable || spent >= cap || attempt >= HARD_ATTEMPTS {
                // Even a final 429 paces the next call on this client: the server just
                // named a wait, and a clone starting now should observe it.
                if failure.status == Some(429) {
                    let deadline = Instant::now() + failure.retry_after.unwrap_or_else(|| backoff(attempt));
                    self.cooldown.set(Some(deadline));
                }
                let advice = if self.model.chatgpt { crate::login::chatgpt::advice(&failure.message) } else { String::new() };
                return Err(anyhow::Error::new(ProviderError {
                    status: failure.status,
                    kind: failure.kind,
                    message: failure.message,
                    attempts: attempt,
                    advice,
                }));
            }
            let wait = failure.retry_after.unwrap_or_else(|| backoff(attempt));
            on_delta(Delta::Retry { attempt, wait, reason: &failure.message });
            if failure.status == Some(429) {
                let deadline = Instant::now() + wait;
                self.cooldown.set(Some(deadline));
                mine = Some(deadline);
            } else {
                tokio::time::sleep(wait).await;
            }
            attempt += 1;
        }
    }

    /// Sleeps until the shared 429 cooldown passes. Returns whether this call actually waited
    /// out a deadline another member set; such an attempt doesn't burn this call's budget.
    async fn wait_cooldown(&self, mine: Option<Instant>) -> bool {
        let Some(shared) = self.cooldown.get() else { return false };
        let deadline = mine.filter(|mine| *mine > shared).unwrap_or(shared);
        let now = Instant::now();
        if deadline <= now {
            return false;
        }
        tokio::time::sleep(deadline - now).await;
        mine.is_none_or(|mine| deadline > mine)
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
        let mut response = request.send().await.map_err(transport_failure)?;
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
                return Err(Failure {
                    kind: ErrorKind::Auth,
                    status: Some(status.as_u16()),
                    retryable: true,
                    retry_after: Some(Duration::ZERO),
                    max_attempts: None,
                    message,
                });
            }
            return Err(classify(status, retry_after, &text));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_ascii_lowercase());
        if content_type.as_deref().is_some_and(|content_type| content_type != "text/event-stream") {
            let body = body_excerpt(&mut response).await;
            let message = format!(
                "HTTP {status}: content-type is {ct}, not an event stream: {}",
                excerpt(&body),
                ct = content_type.as_deref().unwrap_or_default()
            );
            return Err(Failure::of(ErrorKind::BadResponse, Some(status.as_u16()), false, message));
        }

        let mut parser: Box<dyn StreamParser> = match self.model.protocol {
            Protocol::OpenAi => Box::<openai::Parser>::default(),
            Protocol::Anthropic => Box::<anthropic::Parser>::default(),
            Protocol::Responses => Box::<responses::Parser>::default(),
        };
        let mut reply = Reply::default();
        let mut stream = response.bytes_stream();
        let mut lines = SseLines::default();
        let mut head: Vec<u8> = Vec::new();
        let mut saw_data = false;
        'stream: while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                Failure::of(ErrorKind::Transport, None, true, format!("stream interrupted: {}", error_chain(&e)))
            })?;
            if head.len() < EXCERPT_BYTES {
                let take = (EXCERPT_BYTES - head.len()).min(chunk.len());
                head.extend_from_slice(&chunk[..take]);
            }
            lines.push(&chunk);
            while let Some(line) = lines.next_line() {
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else { continue };
                saw_data = true;
                let data = data.trim();
                let Some(event) = parse_sse_data(data)? else {
                    parser.done();
                    break 'stream;
                };
                parser.event(&event, &mut reply, on_delta)?;
            }
        }
        if !saw_data {
            let noted = content_type.map_or(String::new(), |content_type| format!(" (content-type {content_type})"));
            let message = format!(
                "HTTP {status}: the stream carried no events{noted}: {}",
                excerpt(&String::from_utf8_lossy(&head))
            );
            return Err(Failure::of(ErrorKind::BadResponse, Some(status.as_u16()), false, message));
        }
        ensure_stream_consumed(lines.rest())?;
        parser.finish(&mut reply)?;
        validate_reply(reply)
    }
}

fn parse_sse_data(data: &str) -> Result<Option<Value>, Failure> {
    if data == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(data).map(Some).map_err(|e| {
        Failure::of(ErrorKind::BadResponse, None, true, format!("malformed SSE JSON: {e}"))
    })
}

fn ensure_stream_consumed(buffer: &[u8]) -> Result<(), Failure> {
    if buffer.iter().all(|b| b.is_ascii_whitespace()) {
        Ok(())
    } else {
        Err(Failure::of(ErrorKind::BadResponse, None, true, "stream ended with unconsumed data".into()))
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

/// What an image part becomes when the model can't see images or the provider rejected
/// them: plain text, so the turn goes on. The same words everywhere keep the conversion
/// deterministic per model, which keeps the cached prefix hitting.
pub(crate) const IMAGE_OMITTED: &str = "[image omitted: this model can't see images]";

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

/// The kind of a failure that carries an HTTP status and nothing else to go on.
fn kind_for_status(status: Option<u16>) -> ErrorKind {
    match status {
        Some(401 | 403) => ErrorKind::Auth,
        Some(402) => ErrorKind::Quota,
        Some(429) => ErrorKind::RateLimited,
        _ => ErrorKind::Other,
    }
}

/// An error with every error beneath it joined by `: ` — reqwest's own `Display` drops the
/// source, which is the part that says whether it was DNS, TLS or a refused connection.
pub(crate) fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        chain.push_str(": ");
        chain.push_str(&error.to_string());
        source = error.source();
    }
    chain
}

/// A request that never produced a response. Builder and URL mistakes and rejected
/// certificates can't succeed on a retry, so they fail at once; connection-level failures
/// get `TRANSPORT_ATTEMPTS`; timeouts stay as transient as ever.
fn transport_failure(error: reqwest::Error) -> Failure {
    let chain = error_chain(&error);
    let fatal = error.is_builder() || error.is_request() || chain.to_ascii_lowercase().contains("certificate");
    Failure {
        max_attempts: (!fatal && !error.is_timeout()).then_some(TRANSPORT_ATTEMPTS),
        ..Failure::of(ErrorKind::Transport, None, !fatal, format!("request failed: {chain}"))
    }
}

/// The first bytes of a response body, enough to say what a misrouted endpoint sent.
async fn body_excerpt(response: &mut reqwest::Response) -> String {
    let mut bytes = Vec::new();
    while bytes.len() < EXCERPT_BYTES {
        match response.chunk().await {
            Ok(Some(chunk)) => bytes.extend_from_slice(&chunk),
            Ok(None) | Err(_) => break,
        }
    }
    String::from_utf8_lossy(&bytes).trim().to_string()
}

/// A body as one short line for an error message: HTML tags stripped, whitespace folded.
fn excerpt(body: &str) -> String {
    let mut text = String::with_capacity(body.len());
    let mut tag = false;
    for c in body.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            _ if !tag => text.push(c),
            _ => {}
        }
    }
    truncate(&text.split_whitespace().collect::<Vec<_>>().join(" "), 500).to_string()
}

/// A server asking for a longer wait than this isn't worth sitting through.
const LONGEST_WAIT: Duration = Duration::from_secs(60);

/// Words in a 429 body that say a usage quota ran out, not that requests came too fast.
const QUOTA_WORDS: [&str; 5] = ["usage limit", "quota", "reset at", "exceeded your current", "usage_limit_exceeded"];

/// Words in a rejection body that say the conversation no longer fits the context window.
const OVERFLOW_WORDS: [&str; 7] =
    ["context length", "context_length", "maximum context", "context window", "prompt is too long", "too many tokens", "input is too long"];

/// Words in a rejection body that say the provider refused an image or other media.
const MEDIA_WORDS: [&str; 2] = ["image", "media"];

/// What a failed HTTP response means: retried, unless waiting won't help. A spent quota
/// (a 5-hour or monthly limit) resets in hours, so it fails at once with the server's message.
fn classify(status: StatusCode, retry_after: Option<Duration>, body: &str) -> Failure {
    let lower = body.to_lowercase();
    let quota = status == StatusCode::TOO_MANY_REQUESTS && QUOTA_WORDS.iter().any(|w| lower.contains(w));
    let too_long = retry_after.is_some_and(|wait| wait > LONGEST_WAIT);
    let kind = match status.as_u16() {
        429 if quota => ErrorKind::Quota,
        429 => ErrorKind::RateLimited,
        401 | 403 => ErrorKind::Auth,
        402 => ErrorKind::Quota,
        400 | 413 | 422 if OVERFLOW_WORDS.iter().any(|w| lower.contains(w)) => ErrorKind::Overflow,
        400 | 415 if MEDIA_WORDS.iter().any(|w| lower.contains(w)) => ErrorKind::Media,
        _ => ErrorKind::Other,
    };
    Failure {
        kind,
        status: Some(status.as_u16()),
        retryable: retryable_status(status) && !quota && !too_long,
        retry_after,
        max_attempts: None,
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
    fn throttle_text_is_not_overflow() {
        let failure = classify(StatusCode::TOO_MANY_REQUESTS, None, "Too many tokens, please wait and try again");
        assert_eq!(failure.kind, ErrorKind::RateLimited);
        assert!(failure.retryable);
        assert_ne!(failure.kind, ErrorKind::Overflow);
    }

    #[test]
    fn statuses_classify_into_kinds() {
        assert_eq!(classify(StatusCode::UNAUTHORIZED, None, "bad key").kind, ErrorKind::Auth);
        assert_eq!(classify(StatusCode::FORBIDDEN, None, "no").kind, ErrorKind::Auth);
        assert_eq!(classify(StatusCode::PAYMENT_REQUIRED, None, "pay").kind, ErrorKind::Quota);
        assert_eq!(classify(StatusCode::TOO_MANY_REQUESTS, None, "usage limit reached").kind, ErrorKind::Quota);
        assert_eq!(classify(StatusCode::BAD_REQUEST, None, "This model's maximum context length is 128000 tokens").kind, ErrorKind::Overflow);
        assert_eq!(classify(StatusCode::BAD_REQUEST, None, "the image is too large").kind, ErrorKind::Media);
        assert_eq!(classify(StatusCode::INTERNAL_SERVER_ERROR, None, "prompt is too long").kind, ErrorKind::Other);
        assert_eq!(Failure::from_status(Some(429), "mid-stream".into()).kind, ErrorKind::RateLimited);
    }

    #[test]
    fn provider_error_display_keeps_the_message_text() {
        let error = ProviderError {
            status: Some(429),
            kind: ErrorKind::RateLimited,
            message: "HTTP 429 Too Many Requests: slow down".into(),
            attempts: 3,
            advice: String::new(),
        };
        assert_eq!(error.to_string(), "HTTP 429 Too Many Requests: slow down (after 3 attempt(s))");
        let with_advice = ProviderError { advice: " Run `killi login chatgpt`.".into(), ..error };
        assert_eq!(with_advice.to_string(), "HTTP 429 Too Many Requests: slow down (after 3 attempt(s)) Run `killi login chatgpt`.");
    }

    #[test]
    fn an_html_body_becomes_a_one_line_excerpt() {
        let text = "<html>\n  <head><title>Gateway</title></head>\n  <body><h1>Down for maintenance</h1></body>\n</html>";
        assert_eq!(excerpt(text), "Gateway Down for maintenance");
        assert_eq!(excerpt(&"x".repeat(600)).len(), 500);
    }

    #[test]
    fn backoff_doubles_to_a_cap() {
        assert!(backoff(1) >= Duration::from_millis(500) && backoff(1) <= Duration::from_millis(625));
        assert!(backoff(7) >= Duration::from_secs(32) && backoff(7) <= Duration::from_secs(40));
        assert!(backoff(10) <= Duration::from_secs(40));
    }

    #[test]
    fn sse_lines_split_chunks_and_keep_unfinished_tails() {
        let mut lines = SseLines::default();
        assert!(lines.next_line().is_none());
        lines.push(b"data: one\ndata: t");
        assert_eq!(lines.next_line().as_deref(), Some(b"data: one\n".as_slice()));
        assert!(lines.next_line().is_none());
        assert_eq!(lines.rest(), b"data: t");
        lines.push(b"wo\n\n");
        assert_eq!(lines.next_line().as_deref(), Some(b"data: two\n".as_slice()));
        assert_eq!(lines.next_line().as_deref(), Some(b"\n".as_slice()));
        assert!(lines.next_line().is_none());
        assert!(lines.rest().is_empty());
    }

    #[test]
    fn sse_lines_remember_how_far_they_scanned() {
        let mut lines = SseLines::default();
        lines.push(&vec![b'x'; 10_000]);
        assert!(lines.next_line().is_none());
        assert_eq!(lines.scan, 10_000);
        lines.push(b"\n");
        let line = lines.next_line().expect("the newline ends the long line");
        assert_eq!(line.len(), 10_001);
        assert_eq!(lines.scan, 0);
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

    /// A local HTTP server: answers connections with `replies` in order and closes, and
    /// beyond those just reads and closes, recording when each request arrived.
    async fn serve(replies: Vec<String>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Instant>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let arrivals: std::sync::Arc<std::sync::Mutex<Vec<Instant>>> = Default::default();
        let seen = arrivals.clone();
        tokio::spawn(async move {
            let mut replies = replies.into_iter();
            while let Ok((mut socket, _)) = listener.accept().await {
                seen.lock().unwrap().push(Instant::now());
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4_096];
                loop {
                    if let Some(head_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buffer[..head_end]).to_ascii_lowercase();
                        let content_length = head.lines().find_map(|line| line.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok());
                        if buffer.len() >= head_end + 4 + content_length.unwrap_or(0) {
                            break;
                        }
                    }
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                    }
                }
                if let Some(reply) = replies.next() {
                    let _ = socket.write_all(reply.as_bytes()).await;
                }
                let _ = socket.shutdown().await;
            }
        });
        (url, arrivals)
    }

    fn test_model(base_url: &str) -> ResolvedModel {
        ResolvedModel {
            alias: "m".into(),
            model: "gpt-test".into(),
            protocol: Protocol::OpenAi,
            base_url: base_url.into(),
            api_key: String::new(),
            headers: Default::default(),
            session_header: None,
            max_output: None,
            context_window: None,
            images: false,
            thinking: crate::config::Thinking::Off,
            cache_control: false,
            chatgpt: false,
        }
    }

    fn sse_ok() -> String {
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
         data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
         data: [DONE]\n\n"
            .into()
    }

    #[tokio::test]
    async fn a_server_that_closes_without_responding_is_not_hammered() {
        let (url, arrivals) = serve(Vec::new()).await;
        let client = Client::new(test_model(&url)).unwrap();
        let error = client
            .complete(&[serde_json::json!({"role": "user", "content": "hi"})], &[], "k", &mut |_| {})
            .await
            .unwrap_err();
        let provider = error.downcast_ref::<ProviderError>().unwrap();
        assert_eq!(provider.kind, ErrorKind::Transport);
        assert!(provider.attempts <= TRANSPORT_ATTEMPTS, "{} attempts", provider.attempts);
        assert!(provider.message.starts_with("request failed: "), "{}", provider.message);
        let cause = &provider.message["request failed: ".len()..];
        assert!(cause.contains(": "), "the cause follows: {}", provider.message);
        assert_eq!(arrivals.lock().unwrap().len() as u32, provider.attempts);
    }

    #[tokio::test]
    async fn a_refused_connection_names_its_cause_and_stops_early() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let client = Client::new(test_model(&url)).unwrap();
        let error = client
            .complete(&[serde_json::json!({"role": "user", "content": "hi"})], &[], "k", &mut |_| {})
            .await
            .unwrap_err();
        let provider = error.downcast_ref::<ProviderError>().unwrap();
        assert_eq!(provider.kind, ErrorKind::Transport);
        assert!(provider.attempts <= TRANSPORT_ATTEMPTS, "{} attempts", provider.attempts);
        assert!(provider.message.to_ascii_lowercase().contains("refused"), "{}", provider.message);
    }

    #[tokio::test]
    async fn an_unusable_url_fails_at_once() {
        let client = Client::new(test_model("not a url")).unwrap();
        let error = client
            .complete(&[serde_json::json!({"role": "user", "content": "hi"})], &[], "k", &mut |_| {})
            .await
            .unwrap_err();
        let provider = error.downcast_ref::<ProviderError>().unwrap();
        assert_eq!((provider.kind, provider.attempts), (ErrorKind::Transport, 1));
    }

    #[tokio::test]
    async fn an_html_success_body_fails_at_once_with_an_excerpt() {
        let html = "<html><head><title>Gateway</title></head><body><h1>Upstream maintenance</h1><p>Try again soon.</p></body></html>";
        let reply = format!("HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\nconnection: close\r\n\r\n{html}");
        let (url, arrivals) = serve(vec![reply]).await;
        let client = Client::new(test_model(&url)).unwrap();
        let error = client
            .complete(&[serde_json::json!({"role": "user", "content": "hi"})], &[], "k", &mut |_| {})
            .await
            .unwrap_err();
        let provider = error.downcast_ref::<ProviderError>().unwrap();
        assert_eq!(provider.kind, ErrorKind::BadResponse);
        assert_eq!(provider.attempts, 1);
        assert_eq!(arrivals.lock().unwrap().len(), 1);
        assert!(provider.message.contains("200"), "{}", provider.message);
        assert!(provider.message.contains("text/html"), "{}", provider.message);
        assert!(provider.message.contains("Upstream maintenance"), "{}", provider.message);
        assert!(!provider.message.contains("<h1>"), "{}", provider.message);
    }

    #[tokio::test]
    async fn a_shared_cooldown_paces_concurrent_completes() {
        let limited = "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\nretry-after: 1\r\nconnection: close\r\n\r\n{\"error\":{\"message\":\"slow down\"}}".to_string();
        let (url, arrivals) = serve(vec![limited, sse_ok(), sse_ok()]).await;
        let first = Client::new(test_model(&url)).unwrap();
        let second = first.clone();
        let message = || serde_json::json!({"role": "user", "content": "hi"});
        let (tx, paced) = tokio::sync::oneshot::channel::<()>();
        let mut sent = Some(tx);
        let mut on_delta = |delta: Delta| {
            if matches!(delta, Delta::Retry { .. })
                && let Some(tx) = sent.take()
            {
                let _ = tx.send(());
            }
        };
        let (a_message, b_message) = ([message()], [message()]);
        let a = first.complete(&a_message, &[], "k", &mut on_delta);
        let b = async {
            let _ = paced.await;
            second.complete(&b_message, &[], "k", &mut |_| {}).await
        };
        let (a, b) = futures_util::join!(a, b);
        a.unwrap();
        b.unwrap();
        let arrivals = arrivals.lock().unwrap().clone();
        assert_eq!(arrivals.len(), 3, "both completes finish on the one rate limit");
        assert!(arrivals[1].duration_since(arrivals[0]) >= Duration::from_millis(900), "the second waits out the cooldown");
    }
}
