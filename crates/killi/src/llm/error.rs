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
    /// The provider refused the request or filtered its answer on policy grounds.
    Refused,
    /// The provider rejected the request itself as malformed (a 400 or 422 that is neither
    /// overflow nor media), which another model would reject the same way.
    Invalid,
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
        write!(
            f,
            "{} (after {} attempt(s)){}",
            self.message, self.attempts, self.advice
        )
    }
}

impl std::error::Error for ProviderError {}

pub(super) fn retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 425 | 429 | 500..=599)
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
    let fatal = error.is_builder() || chain.to_ascii_lowercase().contains("certificate");
    Failure {
        max_attempts: (!fatal && !error.is_timeout()).then_some(TRANSPORT_ATTEMPTS),
        ..Failure::of(
            ErrorKind::Transport,
            None,
            !fatal,
            format!("request failed: {chain}"),
        )
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
pub(super) const LONGEST_WAIT: Duration = Duration::from_secs(60);

/// Words in a 429 body that say a usage quota ran out, not that requests came too fast.
/// The last two are the Chinese wording some gateways send for a spent plan.
const QUOTA_WORDS: [&str; 9] = [
    "usage limit",
    "quota",
    "budget_exceeded",
    "budget has been exceeded",
    "reset at",
    "exceeded your current",
    "usage_limit_exceeded",
    "用量上限",
    "使用上限",
];

/// Words in a rejection body that say the conversation no longer fits the context window.
const OVERFLOW_WORDS: [&str; 12] = [
    "context length",
    "context_length",
    "maximum context",
    "context window",
    "prompt is too long",
    "too many tokens",
    "input is too long",
    "context limit",
    "token limit",
    "maximum number of tokens",
    "exceeds the context",
    "request_too_large",
];

/// Words that, beside "model", say the requested model doesn't exist here: a rejection
/// another model may well not share, so it still moves the session to its backup.
const UNKNOWN_MODEL_WORDS: [&str; 9] = [
    "not found",
    "not_found",
    "not exist",
    "unknown",
    "no such",
    "not supported",
    "unsupported",
    "not available",
    "invalid model",
];

/// Codes and words that say the provider refused the request or filtered its answer.
const REFUSAL_WORDS: [&str; 4] = [
    "refusal",
    "content_filter",
    "content_policy",
    "content policy",
];

/// Words in a rejection that say the account has no money or credit left: a spent allowance,
/// not a malformed request, so the session moves to its backup.
const BILLING_WORDS: [&str; 7] = [
    "credit balance",
    "insufficient credit",
    "insufficient balance",
    "insufficient_balance",
    "余额不足",
    "billing",
    "payment required",
];

/// Words in an error's text that say requests came too fast.
const RATE_WORDS: [&str; 4] = ["rate limit", "rate_limit", "too many requests", "速率限制"];

/// Codes that say the failure was on the provider's side, so another attempt may well work.
const SERVER_CODES: [&str; 9] = [
    "server_error",
    "overloaded",
    "api_error",
    "timeout",
    "unavailable",
    "internal",
    "upstream",
    "gateway",
    "network",
];

/// Codes that say the credentials or the account's permissions were turned away.
const AUTH_CODES: [&str; 3] = ["authentication", "invalid_api_key", "permission"];

/// Codes that say a usage allowance or the account's money ran out.
const QUOTA_CODES: [&str; 2] = ["usage_limit", "billing"];

/// Words in a rejection body that say the provider refused an image or other media.
const MEDIA_WORDS: [&str; 2] = ["image", "media"];

/// Whether a body or a code says a usage allowance ran out rather than that requests
/// came too fast. The same words for an HTTP body and for a code a stream carried, so
/// the two paths read one allowance the same way.
pub(super) fn spent_quota(text: &str) -> bool {
    let lower = text.to_lowercase();
    QUOTA_WORDS.iter().any(|w| lower.contains(w))
}

/// Whether a rejection is one another model may well not share: a model the provider doesn't
/// have, or a thinking block signed by another provider's model that its one retry without
/// the block didn't cure.
fn model_specific(lower: &str) -> bool {
    lower.contains("模型不存在")
        || lower.contains("invalid `signature` in `thinking` block")
        || (lower.contains("model") && UNKNOWN_MODEL_WORDS.iter().any(|w| lower.contains(w)))
}

/// Whether a rejection says the account has no money or credit left.
fn out_of_credit(lower: &str) -> bool {
    BILLING_WORDS.iter().any(|w| lower.contains(w))
}

/// How long a 429 that reads as a spent quota waits before its one more attempt. Some
/// gateways word a per-minute token limit as a quota ("Allocated quota exceeded"): in the logs
/// the same model answered 0 to 27 s after each of those, so one attempt half a minute later
/// tells the two apart, and a plan that is really spent costs one request more, not ten.
pub(super) const QUOTA_RECHECK: Duration = Duration::from_secs(30);

/// What a failed HTTP response means: retried, unless waiting won't help. A 429 that reads as a
/// spent quota (a 5-hour or monthly limit resets in hours) gets one more attempt after
/// `QUOTA_RECHECK`, in case it was a rate limit worded as one, and then fails with the server's
/// message. A 400, 402 or 403 that says the account is out of credit is a spent quota too.
pub(super) fn classify(status: StatusCode, retry_after: Option<Duration>, body: &str) -> Failure {
    let lower = body.to_lowercase();
    let quota = status == StatusCode::TOO_MANY_REQUESTS && spent_quota(body);
    let too_long = retry_after.is_some_and(|wait| wait > LONGEST_WAIT);
    let kind = match status.as_u16() {
        429 if quota => ErrorKind::Quota,
        429 => ErrorKind::RateLimited,
        400 | 402 | 403 if out_of_credit(&lower) => ErrorKind::Quota,
        401 | 403 => ErrorKind::Auth,
        402 => ErrorKind::Quota,
        400 | 413 | 422 if OVERFLOW_WORDS.iter().any(|w| lower.contains(w)) => ErrorKind::Overflow,
        400 | 415 if MEDIA_WORDS.iter().any(|w| lower.contains(w)) => ErrorKind::Media,
        400 | 422 if !model_specific(&lower) => ErrorKind::Invalid,
        _ => ErrorKind::Other,
    };
    Failure {
        kind,
        status: Some(status.as_u16()),
        retryable: retryable_status(status) && !too_long,
        retry_after: if quota {
            Some(retry_after.unwrap_or(QUOTA_RECHECK))
        } else {
            retry_after
        },
        max_attempts: quota.then_some(2),
        message: format!("HTTP {status}: {}", truncate(body, 2000)),
    }
}

/// What an error carried inside a stream means. `code` is whatever names the failure: a
/// number (an HTTP status), or a string such as OpenAI's `rate_limit_exceeded` or
/// Anthropic's `rate_limit_error`; `message` is the text shown to the user, which also
/// decides the kind when the code says nothing. The same rules as for an HTTP body, so the
/// two paths read one failure the same way.
pub(super) fn classify_stream_error(code: Option<&Value>, message: &str) -> Failure {
    classify_stream_status(None, code, message)
}

/// `classify_stream_error` for a stream that may name an HTTP status beside its code. A code
/// of digits alone outside 100–599 (a gateway's own number, such as `1302`) says nothing, as
/// a number or as a string. A code nobody recognises lets the text decide whether it was a
/// rate limit, a spent quota, an overflow or a refusal, and otherwise fails at once; an
/// absent one is retried.
pub(super) fn classify_stream_status(
    status: Option<u16>,
    code: Option<&Value>,
    message: &str,
) -> Failure {
    let digits = |code: &str| !code.is_empty() && code.bytes().all(|b| b.is_ascii_digit());
    let numbered = code.and_then(|code| {
        code.as_u64().or_else(|| {
            code.as_str()
                .map(str::trim)
                .filter(|code| digits(code))
                .and_then(|code| code.parse().ok())
        })
    });
    let status = status
        .map(u64::from)
        .or(numbered)
        .and_then(|status| u16::try_from(status).ok())
        .filter(|status| (100..600).contains(status));
    let code = code
        .and_then(Value::as_str)
        .map(|code| code.trim().to_lowercase())
        .filter(|code| !digits(code))
        .unwrap_or_default();
    let lower = message.to_lowercase();
    let known = |words: &[&str]| words.iter().any(|w| code.contains(w));
    let said = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    let recognised = known(&SERVER_CODES)
        || known(&["rate_limit", "invalid_request"])
        || known(&QUOTA_CODES)
        || spent_quota(&code)
        || known(&OVERFLOW_WORDS)
        || known(&REFUSAL_WORDS)
        || known(&AUTH_CODES);
    let heard = status.is_none() && !recognised;
    let server_side = known(&SERVER_CODES) || status.is_some_and(|status| status >= 500);
    let rate = known(&["rate_limit"]) || status == Some(429) || (heard && said(&RATE_WORDS));
    let (kind, retryable) = if known(&QUOTA_CODES)
        || spent_quota(&code)
        || status == Some(402)
        || ((heard || rate) && spent_quota(&lower))
        || (!server_side && !rate && out_of_credit(&lower))
    {
        (ErrorKind::Quota, false)
    } else if rate {
        (ErrorKind::RateLimited, true)
    } else if known(&OVERFLOW_WORDS)
        || (!server_side
            && status.is_none_or(|status| matches!(status, 400 | 413 | 422))
            && said(&OVERFLOW_WORDS))
    {
        (ErrorKind::Overflow, false)
    } else if known(&REFUSAL_WORDS) || (heard && said(&REFUSAL_WORDS)) {
        (ErrorKind::Refused, false)
    } else if known(&AUTH_CODES)
        || matches!(status, Some(401 | 403))
        || (heard && said(&["invalid api key", "invalid_api_key"]))
    {
        (ErrorKind::Auth, false)
    } else if known(&["invalid_request"]) || matches!(status, Some(400 | 422)) {
        match model_specific(&lower) {
            true => (ErrorKind::Other, false),
            false => (ErrorKind::Invalid, false),
        }
    } else if server_side {
        (ErrorKind::Other, true)
    } else if let Some(status) = status {
        (
            ErrorKind::Other,
            StatusCode::from_u16(status).is_ok_and(retryable_status),
        )
    } else {
        (ErrorKind::Other, code.is_empty())
    };
    Failure::of(kind, status, retryable, message.to_string())
}

/// 500 ms doubling to a 32 s cap, plus up to 25% jitter.
pub(super) fn backoff(attempt: u32) -> Duration {
    let base = (500u64 << (attempt - 1).min(6)).min(32_000);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let jitter = base * u64::from(nanos % 26) / 100;
    Duration::from_millis(base + jitter)
}
