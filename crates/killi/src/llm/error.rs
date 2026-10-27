//! How failed requests are classified: the kinds callers act on, and whether a retry can heal them.

use std::fmt;

use super::*;

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

pub(super) fn retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 425 | 429 | 500..=599)
}

/// The kind of a failure that carries an HTTP status and nothing else to go on.
pub(super) fn kind_for_status(status: Option<u16>) -> ErrorKind {
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
pub(super) fn transport_failure(error: reqwest::Error) -> Failure {
    let chain = error_chain(&error);
    let fatal = error.is_builder() || error.is_request() || chain.to_ascii_lowercase().contains("certificate");
    Failure {
        max_attempts: (!fatal && !error.is_timeout()).then_some(TRANSPORT_ATTEMPTS),
        ..Failure::of(ErrorKind::Transport, None, !fatal, format!("request failed: {chain}"))
    }
}

/// The first bytes of a response body, enough to say what a misrouted endpoint sent.
pub(super) async fn body_excerpt(response: &mut reqwest::Response) -> String {
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
pub(super) fn excerpt(body: &str) -> String {
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
pub(super) fn classify(status: StatusCode, retry_after: Option<Duration>, body: &str) -> Failure {
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
pub(super) fn backoff(attempt: u32) -> Duration {
    let base = (500u64 << (attempt - 1).min(6)).min(32_000);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().subsec_nanos();
    let jitter = base * u64::from(nanos % 26) / 100;
    Duration::from_millis(base + jitter)
}
