use super::*;

fn model() -> ResolvedModel {
    ResolvedModel {
        alias: "commandcode/spark".into(),
        model: "meta/muse-spark-1.3-contributor".into(),
        display_name: None,
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
        plan: None,
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
    assert_eq!(
        body["params"]["system"],
        json!([{"type": "text", "text": "sys"}])
    );
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
        &[
            json!({"type": "function", "function": {"name": "read", "description": "Read", "parameters": {"type": "object"}}}),
        ],
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
        &[json!({"role": "user", "content": [
            {"type": "text", "text": "look"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
            {"type": "image_url", "image_url": {"url": "https://example/x.png"}},
        ]})],
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
        (
            reply.usage.prompt,
            reply.usage.cached,
            reply.usage.completion,
            reply.usage.reasoning
        ),
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
    assert!(
        !fatal.retryable && fatal.kind == ErrorKind::Quota,
        "{fatal:?}"
    );
    let fatal = error(json!({"type": "error", "code": "invalid_request_error", "message": "bad"}));
    assert!(!fatal.retryable, "{fatal:?}");
    let paced = error(json!({"type": "error", "statusCode": 429, "message": "slow"}));
    assert!(
        paced.retryable && paced.kind == ErrorKind::RateLimited,
        "{paced:?}"
    );
    assert!(error(json!({"type": "error", "message": "boom"})).retryable);
    let named = error(json!({"type": "error", "statusCode": 400,
        "code": "rate_limit_exceeded", "message": "slow down"}));
    assert_eq!(
        (named.kind, named.retryable, named.status),
        (ErrorKind::RateLimited, true, Some(400)),
        "a code beside a status still counts"
    );
    assert!(
        Parser::default()
            .finish(&mut Reply::default())
            .unwrap_err()
            .retryable
    );
}

#[test]
fn a_filtered_finish_is_a_refusal() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    for event in [
        json!({"type": "text-delta", "id": "txt-0", "text": "partial"}),
        json!({"type": "finish", "finishReason": "content-filter"}),
    ] {
        parser
            .event(&event, &mut reply, &mut |_: Delta| {})
            .unwrap();
    }
    parser.finish(&mut reply).unwrap();
    let refused = super::super::validate_reply(reply).unwrap_err();
    assert_eq!(
        (refused.kind, refused.retryable),
        (ErrorKind::Refused, false)
    );
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
