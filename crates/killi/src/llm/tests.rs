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
