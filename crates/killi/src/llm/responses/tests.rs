use super::*;

#[test]
fn history_becomes_input_items() {
    let history = vec![
        json!({"role": "user", "content": "hi"}),
        json!({"role": "assistant", "reasoning_items": [{"type": "reasoning", "killi_origin": "m", "status": "completed", "encrypted_content": "e"}],
               "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{}"}}]}),
        json!({"role": "tool", "tool_call_id": "c1", "content": "data"}),
    ];
    let input = convert_input(&history, true, &Default::default(), "m");
    assert_eq!(input[0], json!({"role": "user", "content": "hi"}));
    assert_eq!(input[1]["encrypted_content"], "e");
    assert!(input[1].get("killi_origin").is_none());
    assert!(input[1].get("status").is_none());
    assert_eq!(
        input[2],
        json!({"type": "function_call", "call_id": "c1", "name": "read", "arguments": "{}"})
    );
    assert_eq!(
        input[3],
        json!({"type": "function_call_output", "call_id": "c1", "output": "data"})
    );
}

#[test]
fn reasoning_from_another_or_unknown_model_is_not_replayed() {
    let history = vec![json!({"role": "assistant", "reasoning_items": [
        {"type": "reasoning", "id": "rs_other", "killi_origin": "other/m", "encrypted_content": "other"},
        {"type": "reasoning", "id": "rs_legacy", "encrypted_content": "legacy"},
    ]})];
    let input = convert_input(&history, true, &Default::default(), "m");
    assert!(input.is_empty(), "{input:?}");
}

#[test]
fn images_become_text_when_the_model_cannot_see_them() {
    let history = vec![json!({"role": "user", "content": [
        {"type": "text", "text": "look"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
    ]})];
    let input = convert_input(&history, false, &Default::default(), "m");
    let content = input[0]["content"].clone();
    assert!(
        content
            .as_array()
            .unwrap()
            .iter()
            .all(|part| part["type"] != "input_image"),
        "{content}"
    );
    assert!(
        content
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["text"] == super::IMAGE_OMITTED),
        "{content}"
    );
    assert_eq!(
        history[0]["content"][1]["type"], "image_url",
        "history itself stays as sent"
    );
}

#[test]
fn stream_assembles_calls_and_usage() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let events = [
        json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "function_call", "call_id": "c1", "name": "grep"}}),
        json!({"type": "response.function_call_arguments.delta", "output_index": 1, "delta": "{\"pattern\":"}),
        json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "function_call", "arguments": "{\"pattern\":\"x\"}"}}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "reasoning", "encrypted_content": "e"}}),
        json!({"type": "response.completed", "response": {"usage": {"input_tokens": 50, "input_tokens_details": {"cached_tokens": 40}, "output_tokens": 7}}}),
    ];
    for event in &events {
        parser.event(event, &mut reply, &mut sink).ok().unwrap();
    }
    assert_eq!(reply.tool_calls[0].arguments, "{\"pattern\":\"x\"}");
    assert_eq!(reply.reasoning_items.len(), 1);
    assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!((reply.usage.prompt, reply.usage.cached), (50, 40));
    parser.finish(&mut reply).unwrap();
}

#[test]
fn a_call_without_streamed_arguments_becomes_empty_json() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let events = [
        json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "function_call", "call_id": "c1", "name": "read"}}),
        json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "function_call", "call_id": "c1", "name": "read"}}),
        json!({"type": "response.completed", "response": {"usage": {}}}),
    ];
    for event in &events {
        parser.event(event, &mut reply, &mut sink).ok().unwrap();
    }
    parser.finish(&mut reply).unwrap();
    assert_eq!(reply.tool_calls[0].arguments, "{}");
}

#[test]
fn stored_arguments_that_arent_json_are_repaired_in_input() {
    let history = vec![json!({"role": "assistant", "tool_calls": [
        {"id": "c1", "type": "function", "function": {"name": "bash", "arguments": ""}}
    ]})];
    let input = convert_input(&history, true, &Default::default(), "m");
    assert_eq!(input[0]["arguments"], "{}");
}

#[test]
fn terminal_event_is_required_and_incomplete_reasons_are_classified() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    assert!(parser.finish(&mut reply).unwrap_err().retryable);

    let incomplete = json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "content_filter"}}});
    assert!(
        !parser
            .event(&incomplete, &mut reply, &mut sink)
            .unwrap_err()
            .retryable
    );
    let incomplete = json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "internal_error"}}});
    assert!(
        parser
            .event(&incomplete, &mut reply, &mut sink)
            .unwrap_err()
            .retryable
    );
    let truncated = json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "max_output_tokens"}}});
    parser.event(&truncated, &mut reply, &mut sink).unwrap();
    assert_eq!(reply.finish_reason.as_deref(), Some("length"));
    parser.finish(&mut reply).unwrap();
}

#[test]
fn reasoning_without_encrypted_content_is_not_replayed() {
    let model = ResolvedModel {
        alias: "m".into(),
        model: "gpt-5".into(),
        protocol: crate::config::Protocol::Responses,
        base_url: "http://localhost".into(),
        api_key: String::new(),
        headers: Default::default(),
        tool_names: Default::default(),
        session_header: None,
        max_output: None,
        context_window: None,
        images: false,
        thinking: Thinking::Off,
        cache_control: false,
        chatgpt: false,
    };
    let messages = vec![
        json!({"role": "user", "content": "hi"}),
        json!({"role": "assistant", "content": "ok", "reasoning_items": [
            {"type": "reasoning", "id": "rs_bare", "killi_origin": "m"},
            {"type": "reasoning", "id": "rs_summary", "killi_origin": "m", "summary": [{"type": "summary_text", "text": "for gateways"}]},
            {"type": "reasoning", "id": "rs_encrypted", "killi_origin": "m", "encrypted_content": "e"},
        ]}),
    ];
    let prepared = prepare(&model, &messages, &[], "k").unwrap();
    let body: Value = serde_json::from_slice(&prepared.body).unwrap();
    let ids: Vec<&str> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert!(
        !ids.contains(&"rs_bare"),
        "a reasoning item with nothing to replay must not be sent"
    );
    assert!(ids.contains(&"rs_encrypted"));
    assert!(
        ids.contains(&"rs_summary"),
        "summary text is kept for gateways"
    );
    assert!(
        body.get("include").is_none(),
        "thinking off does not ask for encrypted reasoning"
    );
}

#[test]
fn prepare_caps_output_from_remaining_context() {
    let model = ResolvedModel {
        alias: "m".into(),
        model: "gpt-5".into(),
        protocol: crate::config::Protocol::Responses,
        base_url: "http://localhost".into(),
        api_key: String::new(),
        headers: Default::default(),
        tool_names: Default::default(),
        session_header: None,
        max_output: Some(4_000),
        context_window: Some(2_000),
        images: false,
        thinking: Thinking::Off,
        cache_control: false,
        chatgpt: false,
    };
    let messages = vec![json!({"role": "user", "content": "hello"})];
    let prepared = prepare(&model, &messages, &[], "cache").unwrap();
    let body: Value = serde_json::from_slice(&prepared.body).unwrap();
    let expected = super::cap_output(&model, &messages, &[], 4_000);
    assert_eq!(body["max_output_tokens"], expected);
}

#[test]
fn a_chatgpt_plan_request_has_no_output_cap_and_no_key() {
    let model = ResolvedModel {
        alias: "m".into(),
        model: "gpt-5".into(),
        protocol: crate::config::Protocol::Responses,
        base_url: "https://api.openai.com/v1".into(),
        api_key: String::new(),
        headers: Default::default(),
        tool_names: Default::default(),
        session_header: None,
        max_output: Some(4_000),
        context_window: Some(272_000),
        images: false,
        thinking: Thinking::Effort("high".into()),
        cache_control: false,
        chatgpt: true,
    };
    let prepared = prepare(
        &model,
        &[
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "hi"}),
        ],
        &[],
        "cache",
    )
    .unwrap();
    let body: Value = serde_json::from_slice(&prepared.body).unwrap();
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(
        (
            body["store"].clone(),
            body["stream"].clone(),
            body["instructions"].clone()
        ),
        (json!(false), json!(true), json!("sys"))
    );
    assert!(
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["role"] != "system"),
        "the plan rejects system items"
    );
    assert!(
        prepared
            .headers
            .iter()
            .all(|(name, _)| name != "authorization"),
        "the token is added per attempt"
    );
}

#[test]
fn mapped_tool_names_go_out_and_come_back() {
    let model = ResolvedModel {
        alias: "m".into(),
        model: "muse".into(),
        protocol: crate::config::Protocol::Responses,
        base_url: "https://api.meta.ai/v1".into(),
        api_key: String::new(),
        headers: Default::default(),
        tool_names: BTreeMap::from([
            ("bash".to_string(), "run_command".to_string()),
            ("web_search".to_string(), "search_web".to_string()),
        ]),
        session_header: None,
        max_output: None,
        context_window: None,
        images: false,
        thinking: Thinking::Off,
        cache_control: false,
        chatgpt: false,
    };
    let messages = vec![
        json!({"role": "user", "content": "list"}),
        json!({"role": "assistant", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}}]}),
        json!({"role": "tool", "tool_call_id": "c1", "content": "files"}),
    ];
    let tools = vec![
        json!({"function": {"name": "bash", "description": "Run", "parameters": {}}}),
        json!({"function": {"name": "web_search", "description": "Search", "parameters": {}}}),
    ];
    let prepared = prepare(&model, &messages, &tools, "k").unwrap();
    let body: Value = serde_json::from_slice(&prepared.body).unwrap();
    let names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["run_command", "search_web"],
        "the names the API refuses are mapped"
    );
    assert_eq!(
        body["input"][1]["name"], "run_command",
        "a replayed call keeps the wire name"
    );

    let mut parser = Parser::new(&model);
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let added = json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "function_call", "call_id": "c2", "name": "search_web"}});
    parser.event(&added, &mut reply, &mut sink).unwrap();
    assert_eq!(
        reply.tool_calls[0].name, "web_search",
        "the call comes back under killi's name"
    );
    let reasoning = json!({"type": "response.output_item.done", "item": {"type": "reasoning", "encrypted_content": "e"}});
    parser.event(&reasoning, &mut reply, &mut sink).unwrap();
    assert_eq!(reply.reasoning_items[0]["killi_origin"], "m");
}

#[test]
fn a_refusal_delta_is_fatal() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let event = json!({"type": "response.refusal.delta", "delta": "cannot help"});
    assert!(
        !parser
            .event(&event, &mut reply, &mut sink)
            .unwrap_err()
            .retryable
    );
}

#[test]
fn plan_errors_mid_stream_retry_only_when_temporary() {
    let mut sink = |_: Delta| {};
    let failed =
        |code: &str| json!({"type": "response.failed", "response": {"error": {"code": code}}});
    let mut reply = Reply::default();
    assert!(
        Parser::default()
            .event(
                &failed("subscription_sharing_usage_unavailable"),
                &mut reply,
                &mut sink
            )
            .unwrap_err()
            .retryable
    );
    assert!(
        !Parser::default()
            .event(
                &failed("subscription_sharing_usage_limit_exceeded"),
                &mut reply,
                &mut sink
            )
            .unwrap_err()
            .retryable
    );
}

#[test]
fn a_spent_plan_error_mid_stream_is_not_retried() {
    let mut sink = |_: Delta| {};
    let mut reply = Reply::default();
    // The code sits under `error`, not beside the type: read from the wrong place it is
    // an empty code, which reads as "try again" and spends ten attempts on a dead plan.
    let event = json!({
        "type": "error",
        "sequence_number": 3,
        "error": {
            "code": "subscription_sharing_usage_limit_exceeded",
            "message": "The ChatGPT user has reached their Subscription Sharing usage limit.",
            "param": null,
            "type": "invalid_request_error"
        }
    });
    let failure = Parser::default()
        .event(&event, &mut reply, &mut sink)
        .unwrap_err();
    assert_eq!(failure.kind, ErrorKind::Quota);
    assert!(!failure.retryable);
    assert!(failure.message.contains("usage limit"), "{}", failure.message);
}

#[test]
fn a_rate_limit_mid_stream_is_still_retried() {
    let mut sink = |_: Delta| {};
    let mut reply = Reply::default();
    let event = json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow down"});
    let failure = Parser::default()
        .event(&event, &mut reply, &mut sink)
        .unwrap_err();
    assert_eq!(failure.kind, ErrorKind::RateLimited);
    assert!(failure.retryable);
}

#[test]
fn a_server_error_mid_stream_is_still_retried() {
    let mut sink = |_: Delta| {};
    let mut reply = Reply::default();
    let event = json!({"type": "error", "code": "server_error", "message": "try again"});
    let failure = Parser::default()
        .event(&event, &mut reply, &mut sink)
        .unwrap_err();
    assert!(failure.retryable);
    assert_eq!(failure.kind, ErrorKind::Other);
}

#[test]
fn an_unknown_error_mid_stream_is_not_retried() {
    let mut sink = |_: Delta| {};
    let mut reply = Reply::default();
    let event = json!({"type": "error", "code": "invalid_request_error", "message": "bad"});
    let failure = Parser::default()
        .event(&event, &mut reply, &mut sink)
        .unwrap_err();
    assert!(!failure.retryable);
}
