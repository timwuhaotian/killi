//! Provider clients: one streaming request with retries, in each provider's wire format.
//! History is kept in OpenAI chat format and converted the same way on every request, so
//! the bytes sent for earlier messages never change.

mod anthropic;
mod error;
mod history;
mod openai;
mod responses;
mod tokens;

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::Value;

use crate::config::{Protocol, ResolvedModel};

use error::{backoff, body_excerpt, classify, excerpt, kind_for_status, retryable_status, transport_failure};
use history::{arguments_object, content_text, system_text};
use tokens::cap_output;

pub use error::{ErrorKind, ProviderError};
pub use openai::REASONING_KEYS;
pub(crate) use error::error_chain;
pub(crate) use tokens::{IMAGE_OMITTED, IMAGE_TOKEN_RESERVE};

#[cfg(test)]
use tokens::{OUTPUT_SAFETY_TOKENS, estimated_input_tokens};

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
    /// Delta field names a provider sent that no adapter reads, sorted, deduped and capped:
    /// what the stream carried that killi doesn't keep, evidence for hidden reasoning.
    pub unread_fields: Vec<String>,
}

impl Reply {
    /// Whether anything came back to keep on either reasoning channel: the scalar text or the
    /// structured items, whichever dialect the provider spoke. The channels must be judged
    /// together — an items-only reply kept its reasoning with no scalar text at all.
    pub fn reasoning_kept(&self) -> bool {
        !self.reasoning.is_empty() || !self.reasoning_items.is_empty()
    }

    /// Notes a delta field name no adapter reads: at most 8, each cut at 40 characters.
    pub fn note_unread(&mut self, key: &str) {
        let name = key.chars().take(40).collect::<String>();
        if !self.unread_fields.contains(&name) {
            self.unread_fields.push(name);
            self.unread_fields.sort();
            self.unread_fields.dedup();
            self.unread_fields.truncate(8);
        }
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
            Protocol::Responses => Box::new(responses::Parser::new(&self.model.tool_names)),
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
mod tests;
