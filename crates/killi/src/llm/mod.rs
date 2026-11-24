//! Provider clients: one streaming request with retries, in each provider's wire format.
//! History is kept in OpenAI chat format and converted the same way on every request, so
//! the bytes sent for earlier messages never change.

mod anthropic;
mod commandcode;
mod error;
mod history;
mod openai;
mod responses;
mod tokens;

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::Value;

use crate::config::{Plan, Protocol, ResolvedModel};

use error::{
    LONGEST_WAIT, backoff, body_excerpt, classify, classify_stream_error, classify_stream_status,
    excerpt, transport_failure,
};
use history::{arguments_object, arguments_valid, content_text, json_arguments, system_text};
use tokens::cap_output;

pub(crate) use commandcode::CLI_VERSION;
pub(crate) use error::error_chain;
pub use error::{ErrorKind, ProviderError};
pub use openai::REASONING_KEYS;
pub(crate) use tokens::{IMAGE_OMITTED, IMAGE_TOKEN_RESERVE};

#[cfg(test)]
use tokens::{OUTPUT_SAFETY_TOKENS, estimated_input_tokens};

const MAX_ATTEMPTS: u32 = 10;

/// Requests that never reach a response head stop early: a refused port or an unresolvable
/// host won't heal in the time ten attempts take. Once the endpoint has answered this
/// client, the same failure is the network going away (a Mac waking from sleep resolves
/// nothing for a few seconds), so it gets the full `MAX_ATTEMPTS`, about two minutes of backoff.
const TRANSPORT_ATTEMPTS: u32 = 3;

/// Requests one `complete` may send in all, however many of its waits were shared with other
/// members of a swarm, so peers pushing the cooldown back can't keep it alive forever.
const HARD_ATTEMPTS: u32 = 2 * MAX_ATTEMPTS;

/// The longest one request may spend waiting out rate limits in all: its own 429 and
/// rate-limit cooldowns and the ones other members of a swarm set on the shared client. A
/// limit that hasn't cleared by then isn't going to, and a swarm's members share one client,
/// so their waits add up to a session that looks stopped. Other backoff (5xx, timeouts,
/// transport) is bounded by attempt counts instead.
const MAX_WAIT: Duration = Duration::from_secs(180);

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
    /// Whether the endpoint has sent a response head to any clone of this client; from then
    /// on a connection failure is retried like any transient one, not cut at `TRANSPORT_ATTEMPTS`.
    answered: Rc<Cell<bool>>,
    /// How long one request may wait between attempts in all; `MAX_WAIT`, or less in a test.
    wait_budget: Duration,
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

    /// Tool-call arguments a provider never sent become `{}`. Anything else is kept as
    /// streamed, even when it isn't valid JSON: the call then fails with the raw arguments
    /// shown (and, cut off at the output limit, says so), and each adapter repairs the copy
    /// it sends, so the stored history stays replayable. Each adapter calls this as its
    /// stream finishes.
    pub fn repair_call_arguments(&mut self) {
        for call in &mut self.tool_calls {
            if call.arguments.trim().is_empty() {
                call.arguments = "{}".into();
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Exactly as the model streamed it; never re-serialized.
    pub arguments: String,
    /// The provider's own data on the call, stored on it and sent back verbatim: Gemini's
    /// `{"google": {"thought_signature": …}}`, which it requires on the next request.
    pub extra_content: Option<Value>,
}

pub enum Delta<'a> {
    Text(&'a str),
    Reasoning(&'a str),
    /// A fragment of a tool call's arguments: counted toward the live output estimate,
    /// never shown.
    Tool(&'a str),
    Retry {
        attempt: u32,
        wait: Duration,
        reason: &'a str,
    },
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
    fn event(
        &mut self,
        event: &Value,
        reply: &mut Reply,
        on_delta: &mut dyn FnMut(Delta),
    ) -> Result<(), Failure>;
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
        Self {
            kind,
            status,
            retryable,
            retry_after: None,
            max_attempts: None,
            message,
        }
    }
    fn fatal(message: String) -> Self {
        Self::of(ErrorKind::Other, None, false, message)
    }
    fn transient(message: String) -> Self {
        Self::of(ErrorKind::Other, None, true, message)
    }
    fn refused(message: String) -> Self {
        Self::of(ErrorKind::Refused, None, false, message)
    }
}

impl From<crate::login::TokenError> for Failure {
    fn from(error: crate::login::TokenError) -> Self {
        match error {
            crate::login::TokenError::SignIn(message) => {
                Failure::of(ErrorKind::Auth, None, false, message)
            }
            crate::login::TokenError::Transient(message) => Failure::transient(message),
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
        Ok(Self {
            http,
            model,
            cooldown: Rc::new(Cell::new(None)),
            answered: Rc::new(Cell::new(false)),
            wait_budget: MAX_WAIT,
        })
    }

    /// Sends one request, retrying transient failures. A failed attempt's partial output is
    /// discarded, so history only ever holds complete replies. Every attempt first waits out
    /// a rate-limit cooldown shared with the other members of a swarm, and the retries stop
    /// once the request has waited out its budget in all, however many of those waits were
    /// shared.
    pub async fn complete(
        &self,
        messages: &[Value],
        tools: &[Value],
        cache_key: &str,
        on_delta: &mut dyn FnMut(Delta),
    ) -> Result<Reply> {
        if self.model.alias.is_empty() {
            bail!("no model is configured yet; type /provider to add one, then /model picks it");
        }
        if self.model.plan.is_none() && self.model.api_key.is_empty() {
            bail!(
                "model `{}` has no API key yet; type /provider to add one, or run `killi login` for a subscription",
                self.model.alias
            );
        }
        let mut prepared = match self.model.protocol {
            Protocol::OpenAi => openai::prepare(&self.model, messages, tools, cache_key),
            Protocol::Anthropic => anthropic::prepare(&self.model, messages, tools),
            Protocol::Responses => responses::prepare(&self.model, messages, tools, cache_key),
            Protocol::CommandCode => commandcode::prepare(&self.model, messages, tools, cache_key),
        }?;
        if let Some(name) = &self.model.session_header {
            prepared.headers.push((name.clone(), cache_key.to_string()));
        }
        let mut attempt = 1;
        let mut renewed = false;
        let mut retried_signature = false;
        let mut burns = 0;
        let mut mine = None;
        let mut waited = Duration::ZERO;
        loop {
            let (slept, waited_peer) = self
                .wait_cooldown(
                    mine,
                    attempt,
                    self.wait_budget.saturating_sub(waited),
                    on_delta,
                )
                .await;
            waited += slept;
            let mut failure = match self.attempt(&prepared, &mut renewed, on_delta).await {
                Ok(reply) => return Ok(reply),
                Err(failure) => failure,
            };
            if !retried_signature
                && attempt < HARD_ATTEMPTS
                && self.model.protocol == Protocol::Anthropic
                && failure.status == Some(400)
                && failure
                    .message
                    .contains("Invalid `signature` in `thinking` block")
                && anthropic::omit_rejected_thinking(&mut prepared.body, &failure.message)
            {
                retried_signature = true;
                on_delta(Delta::Retry {
                    attempt,
                    wait: Duration::ZERO,
                    reason: &failure.message,
                });
                attempt += 1;
                continue;
            }
            if self.answered.get() && failure.kind == ErrorKind::Transport {
                failure.max_attempts = None;
            }
            if !waited_peer {
                burns += 1;
            }
            let cap = failure.max_attempts.unwrap_or(MAX_ATTEMPTS);
            let spent = if failure.max_attempts.is_some() {
                attempt
            } else {
                burns
            };
            if !failure.retryable
                || spent >= cap
                || attempt >= HARD_ATTEMPTS
                || waited >= self.wait_budget
            {
                // Even a final rate limit paces the next call on this client for a wait worth
                // sitting out: the server just named one, and a clone starting now should
                // observe it. A wait too long to sit out sets none, so the next call fails
                // at once instead of sleeping on it.
                if failure.retryable && shares_cooldown(&failure) {
                    let wait = failure.retry_after.unwrap_or_else(|| backoff(attempt));
                    self.cooldown
                        .set(Some(Instant::now() + wait.min(LONGEST_WAIT)));
                }
                let advice = if self.model.plan == Some(Plan::ChatGpt) {
                    crate::login::chatgpt::advice(&failure.message)
                } else if self.model.plan == Some(Plan::Grok) {
                    crate::login::grok::advice(&failure.message)
                } else if self.model.protocol == Protocol::CommandCode
                    && failure.kind == ErrorKind::Auth
                {
                    "run `killi login command-code` to sign in again".to_string()
                } else {
                    String::new()
                };
                return Err(anyhow::Error::new(ProviderError {
                    status: failure.status,
                    kind: failure.kind,
                    message: failure.message,
                    attempts: attempt,
                    advice,
                }));
            }
            let wait = failure.retry_after.unwrap_or_else(|| backoff(attempt));
            on_delta(Delta::Retry {
                attempt,
                wait,
                reason: &failure.message,
            });
            if shares_cooldown(&failure) {
                let deadline = Instant::now() + wait;
                self.cooldown.set(Some(deadline));
                mine = Some(deadline);
            } else {
                tokio::time::sleep(wait).await;
            }
            attempt += 1;
        }
    }

    /// Sleeps until the shared rate-limit cooldown passes, or for `budget` at most. Returns
    /// how long it waited, and whether this call actually waited out a deadline another
    /// member (or an earlier request) set; such an attempt doesn't burn this call's budget,
    /// and it is announced as a retry, since nothing else says why the request is waiting.
    async fn wait_cooldown(
        &self,
        mine: Option<Instant>,
        attempt: u32,
        budget: Duration,
        on_delta: &mut dyn FnMut(Delta),
    ) -> (Duration, bool) {
        let Some(shared) = self.cooldown.get() else {
            return (Duration::ZERO, false);
        };
        let deadline = mine.filter(|mine| *mine > shared).unwrap_or(shared);
        let now = Instant::now();
        if deadline <= now {
            return (Duration::ZERO, false);
        }
        let wait = (deadline - now).min(budget);
        let shared = mine.is_none_or(|mine| deadline > mine);
        if shared && !wait.is_zero() {
            on_delta(Delta::Retry {
                attempt,
                wait,
                reason: "waiting out the provider's rate limit",
            });
        }
        tokio::time::sleep(wait).await;
        (wait, shared)
    }

    /// One request. On a plan it carries the plan's current token, and the first 401 of a
    /// `complete` call refreshes that token and is retried at once (`renewed` records it).
    async fn attempt(
        &self,
        prepared: &Prepared,
        renewed: &mut bool,
        on_delta: &mut dyn FnMut(Delta),
    ) -> Result<Reply, Failure> {
        let mut request = self.http.post(&prepared.url).body(prepared.body.clone());
        let token = match self.model.plan {
            Some(plan) => Some(plan_token(&self.http, plan, None).await?),
            None => None,
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
        self.answered.set(true);
        let status = response.status();
        if !status.is_success() {
            let retry_after = retry_after(response.headers());
            let text = body_excerpt(&mut response).await;
            if let Some(token) = token
                .as_deref()
                .filter(|_| status == StatusCode::UNAUTHORIZED && !*renewed)
            {
                let plan = self.model.plan.expect("a token means a plan");
                plan_token(&self.http, plan, Some(token)).await?;
                *renewed = true;
                let message =
                    format!("HTTP {status}: the plan's token was turned away, so it was refreshed");
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
            .map(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or(value)
                    .trim()
                    .to_ascii_lowercase()
            });
        // Command Code answers NDJSON with its own content type, not an event stream.
        let ndjson = self.model.protocol == Protocol::CommandCode;
        if !ndjson
            && content_type
                .as_deref()
                .is_some_and(|content_type| content_type != "text/event-stream")
        {
            let body = body_excerpt(&mut response).await;
            let message = format!(
                "HTTP {status}: content-type is {ct}, not an event stream: {}",
                excerpt(&body),
                ct = content_type.as_deref().unwrap_or_default()
            );
            return Err(Failure::of(
                ErrorKind::BadResponse,
                Some(status.as_u16()),
                false,
                message,
            ));
        }

        let mut parser: Box<dyn StreamParser> = match self.model.protocol {
            Protocol::OpenAi => Box::new(openai::Parser::new(&self.model.tool_names)),
            Protocol::Anthropic => Box::new(anthropic::Parser::new(&self.model.tool_names)),
            Protocol::Responses => Box::new(responses::Parser::new(&self.model)),
            Protocol::CommandCode => Box::new(commandcode::Parser::new(&self.model.tool_names)),
        };
        let mut reply = Reply::default();
        let mut stream = response.bytes_stream();
        let mut lines = SseLines::default();
        let mut head: Vec<u8> = Vec::new();
        let mut saw_data = false;
        'stream: while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                Failure::of(
                    ErrorKind::Transport,
                    None,
                    true,
                    format!("stream interrupted: {}", error_chain(&e)),
                )
            })?;
            if head.len() < EXCERPT_BYTES {
                let take = (EXCERPT_BYTES - head.len()).min(chunk.len());
                head.extend_from_slice(&chunk[..take]);
            }
            lines.push(&chunk);
            while let Some(line) = lines.next_line() {
                let line = String::from_utf8_lossy(&line);
                if ndjson {
                    let data = line.trim();
                    if data.is_empty() {
                        continue;
                    }
                    saw_data = true;
                    let event: Value = serde_json::from_str(data).map_err(|e| {
                        Failure::of(
                            ErrorKind::BadResponse,
                            None,
                            true,
                            format!("malformed NDJSON: {e}"),
                        )
                    })?;
                    parser.event(&event, &mut reply, on_delta)?;
                    continue;
                }
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
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
            let noted = content_type.map_or(String::new(), |content_type| {
                format!(" (content-type {content_type})")
            });
            let message = format!(
                "HTTP {status}: the stream carried no events{noted}: {}",
                excerpt(&String::from_utf8_lossy(&head))
            );
            return Err(Failure::of(
                ErrorKind::BadResponse,
                Some(status.as_u16()),
                false,
                message,
            ));
        }
        ensure_stream_consumed(lines.rest())?;
        parser.finish(&mut reply)?;
        validate_reply(reply)
    }
}

/// Whether a failure paces every clone of the client: a 429, or a rate limit a stream named.
fn shares_cooldown(failure: &Failure) -> bool {
    failure.status == Some(429) || failure.kind == ErrorKind::RateLimited
}

/// The wait a response asks for: `retry-after-ms` when it names one, else `retry-after` in
/// seconds. A date or anything unreadable names none.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let header = |name: &str, unit: f64| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<f64>().ok())
            .and_then(|v| Duration::try_from_secs_f64(v / unit).ok())
    };
    header("retry-after-ms", 1000.0).or_else(|| header("retry-after", 1.0))
}

/// The token a request on `plan` carries: the saved one while it has time left, else a
/// refreshed one. `rejected` is a token the API just turned away.
async fn plan_token(
    http: &reqwest::Client,
    plan: Plan,
    rejected: Option<&str>,
) -> Result<String, crate::login::TokenError> {
    match plan {
        Plan::ChatGpt => crate::login::chatgpt::access_token(http, rejected).await,
        Plan::Grok => crate::login::grok::access_token(http, rejected).await,
    }
}

fn parse_sse_data(data: &str) -> Result<Option<Value>, Failure> {
    if data == "[DONE]" {
        return Ok(None);
    }
    serde_json::from_str(data).map(Some).map_err(|e| {
        Failure::of(
            ErrorKind::BadResponse,
            None,
            true,
            format!("malformed SSE JSON: {e}"),
        )
    })
}

fn ensure_stream_consumed(buffer: &[u8]) -> Result<(), Failure> {
    if buffer.iter().all(|b| b.is_ascii_whitespace()) {
        Ok(())
    } else {
        Err(Failure::of(
            ErrorKind::BadResponse,
            None,
            true,
            "stream ended with unconsumed data".into(),
        ))
    }
}

/// Only normal termination and an output-limit reply are usable; in particular, a content
/// filter must not turn a partial or reasoning-only attempt into a committed assistant turn.
/// A reply cut off at the context window is an output-limit reply, or an overflow when
/// nothing came back; a provider short of capacity is asked again.
fn validate_reply(mut reply: Reply) -> Result<Reply, Failure> {
    if let Some(reason) = reply.finish_reason.as_deref()
        && !matches!(reason, "stop" | "length" | "tool_calls")
    {
        let message = format!("response terminated abnormally: {reason}");
        match reason {
            "model_length" | "model_context_window_exceeded" => {
                if reply.text.trim().is_empty() && reply.tool_calls.is_empty() {
                    return Err(Failure::of(ErrorKind::Overflow, None, false, message));
                }
                reply.finish_reason = Some("length".into());
            }
            "insufficient_system_resource" | "error" | "overloaded" => {
                return Err(Failure::transient(message));
            }
            "refusal" | "content_filter" | "content-filter" | "safety" | "sensitive" => {
                return Err(Failure::refused(message));
            }
            _ => return Err(Failure::fatal(message)),
        }
    }
    if reply.text.trim().is_empty() && reply.tool_calls.is_empty() && reply.finish_reason.is_none()
    {
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
