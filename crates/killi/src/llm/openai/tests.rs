use super::*;
use serde_json::json;

#[test]
fn stream_events_assemble_a_reply() {
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let events = [
        r#"{"choices":[{"delta":{"reasoning_content":"think"}}]}"#,
        r#"{"choices":[{"delta":{"content":"Hi"}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"pa"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":8}}}"#,
    ];
    let mut parser = Parser::default();
    for event in events {
        parser
            .event(&serde_json::from_str(event).unwrap(), &mut reply, &mut sink)
            .ok()
            .unwrap();
    }
    assert_eq!(reply.text, "Hi");
    assert_eq!(reply.reasoning, "think");
    assert_eq!(reply.reasoning_key, Some("reasoning_content"));
    assert_eq!(reply.tool_calls[0].name, "read");
    assert_eq!(reply.tool_calls[0].arguments, r#"{"path":"a"}"#);
    assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!((reply.usage.prompt, reply.usage.cached), (10, 8));
    parser.finish(&mut reply).unwrap();
}

#[test]
fn done_is_a_terminator_but_an_unterminated_stream_is_not() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    assert!(parser.finish(&mut reply).unwrap_err().retryable);
    parser.done();
    parser.finish(&mut reply).unwrap();
}

#[test]
fn a_refusal_delta_is_fatal() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let event = json!({"choices":[{"delta":{"refusal":"cannot help"}}]});
    assert!(
        !parser
            .event(&event, &mut reply, &mut sink)
            .unwrap_err()
            .retryable
    );
}

#[test]
fn sparse_call_indices_do_not_create_nameless_calls() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let events = [
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"read","arguments":"{}"}}]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":5,"id":"b","function":{"name":"grep","arguments":"{}"}}]}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ];
    for event in &events {
        parser.event(event, &mut reply, &mut sink).unwrap();
    }
    parser.finish(&mut reply).unwrap();
    assert_eq!(reply.tool_calls.len(), 2);
    assert_eq!(
        reply
            .tool_calls
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>(),
        ["read", "grep"]
    );
}

#[test]
fn a_huge_call_index_does_not_panic() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let events = [
        json!({"choices":[{"delta":{"tool_calls":[{"index": u64::MAX,"id":"a","function":{"name":"read","arguments":"{}"}}]}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ];
    for event in &events {
        parser.event(event, &mut reply, &mut sink).unwrap();
    }
    parser.finish(&mut reply).unwrap();
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].name, "read");
}

#[test]
fn calls_without_names_are_dropped_when_the_stream_finishes() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let events = [
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"read","arguments":"{}"}}]}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ];
    for event in &events {
        parser.event(event, &mut reply, &mut sink).unwrap();
    }
    parser.finish(&mut reply).unwrap();
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].name, "read");

    let mut reply = Reply::default();
    let events = [
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ];
    for event in &events {
        Parser::default()
            .event(event, &mut reply, &mut sink)
            .ok()
            .unwrap();
    }
    let mut parser = Parser::default();
    assert!(parser.finish(&mut reply).unwrap_err().retryable);
}

#[test]
fn a_call_without_streamed_arguments_becomes_empty_json() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let events = [
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"bash"}}]}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ];
    for event in &events {
        parser.event(event, &mut reply, &mut sink).unwrap();
    }
    parser.done();
    parser.finish(&mut reply).unwrap();
    assert_eq!(reply.tool_calls[0].arguments, "{}");
}

#[test]
fn stored_arguments_that_arent_json_are_repaired_on_the_copy_being_sent() {
    let model = ResolvedModel {
        alias: "m".into(),
        model: "gpt".into(),
        display_name: None,
        protocol: crate::config::Protocol::OpenAi,
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
    let messages = vec![json!({
        "role": "assistant", "content": null,
        "tool_calls": [
            {"id": "c1", "type": "function", "function": {"name": "bash", "arguments": ""}},
            {"id": "c2", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"a\"}"}},
        ]
    })];
    let original = messages.clone();
    let body: Value =
        serde_json::from_slice(&prepare(&model, &messages, &[], "k").unwrap().body).unwrap();
    assert_eq!(
        body["messages"][0]["tool_calls"][0]["function"]["arguments"],
        "{}"
    );
    assert_eq!(
        body["messages"][0]["tool_calls"][1]["function"]["arguments"],
        r#"{"path":"a"}"#
    );
    assert_eq!(messages, original, "history stays as stored");
}

#[test]
fn reasoning_detail_deltas_accumulate_by_item() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let events = [
        json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","index":0,"format":"x","text":"think "}]}}]}),
        json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","index":0,"format":"x","text":"more"}]}}]}),
        json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","index":1,"text":"other"}]}}]}),
    ];
    for event in &events {
        parser.event(event, &mut reply, &mut sink).unwrap();
    }
    assert_eq!(reply.reasoning_items.len(), 2);
    assert_eq!(reply.reasoning_items[0]["text"], "think more");
    assert_eq!(reply.reasoning_items[0]["format"], "x");
    assert_eq!(reply.reasoning_items[1]["text"], "other");
    assert!(reply.reasoning.is_empty());
}

#[test]
fn reasoning_detail_types_with_one_index_stay_separate_and_null_ids_continue_a_block() {
    let mut reply = Reply::default();
    merge_reasoning_detail(
        &mut reply,
        &json!({"type":"reasoning.summary","index":0,"summary":"think "}),
    );
    merge_reasoning_detail(
        &mut reply,
        &json!({"type":"reasoning.encrypted","index":0,"data":"cipher"}),
    );
    merge_reasoning_detail(
        &mut reply,
        &json!({"type":"reasoning.summary","index":0,"summary":"more"}),
    );
    assert_eq!(reply.reasoning_items.len(), 2);
    assert_eq!(reply.reasoning_items[0]["summary"], "think more");
    assert!(reply.reasoning_items[0].get("data").is_none());
    assert_eq!(reply.reasoning_items[1]["data"], "cipher");

    merge_reasoning_detail(
        &mut reply,
        &json!({"type":"summary","id":null,"summary":"part "}),
    );
    merge_reasoning_detail(
        &mut reply,
        &json!({"type":"summary","id":null,"summary":"two"}),
    );
    assert_eq!(reply.reasoning_items[2]["summary"], "part two");
}

#[test]
fn kimi_reasoning_items_drop_stream_indexes_when_replayed() {
    let messages = vec![json!({
        "role": "assistant",
        "reasoning_items": [
            {"type": "summary", "index": 0, "summary": "plan"},
            {"type": "encrypted", "index": 0, "data": "cipher"},
            {"type": "reasoning.text", "index": 0, "text": "keep"}
        ]
    })];
    let details = replay_history(&messages)[0]["reasoning_details"].clone();
    assert!(details[0].get("index").is_none());
    assert!(details[1].get("index").is_none());
    assert_eq!(details[2]["index"], 0);
}

#[test]
fn reasoning_items_replay_as_details_without_mutating_history() {
    let messages = vec![json!({
        "role": "assistant",
        "reasoning_items": [
            {"type": "reasoning.text", "index": 0, "text": "plan"},
            {"type": "reasoning", "encrypted_content": "responses-only"}
        ],
        "content": "ok"
    })];
    let original = messages.clone();
    let replayed = replay_history(&messages);
    assert_eq!(messages, original);
    assert_eq!(
        replayed[0]["reasoning_details"],
        json!([{"type": "reasoning.text", "index": 0, "text": "plan"}])
    );
    assert!(replayed[0].get("reasoning_items").is_none());
}

#[test]
fn cache_breakpoints_mark_the_system_and_newest_message_on_the_copy_only() {
    let history = vec![
        json!({"role": "system", "content": "rules"}),
        json!({"role": "user", "content": "run it"}),
        json!({"role": "assistant", "content": null, "tool_calls": [{"id": "c", "type": "function", "function": {"name": "bash", "arguments": "{}"}}]}),
        json!({"role": "tool", "tool_call_id": "c", "content": "done"}),
    ];
    let original = history.clone();
    let mut sent = history.clone();
    mark_cache_breakpoints(&mut sent);
    let mark = json!({"type": "ephemeral"});
    assert_eq!(
        sent[0]["content"],
        json!([{"type": "text", "text": "rules", "cache_control": mark}])
    );
    assert_eq!(sent[1]["content"], "run it");
    assert!(sent[2]["content"].is_null());
    assert_eq!(
        sent[3]["content"],
        json!([{"type": "text", "text": "done", "cache_control": mark}])
    );
    assert_eq!(history, original);
    assert_eq!(sent.iter().filter(|m| m["content"].is_array()).count(), 2);
}

#[test]
fn cache_breakpoints_go_on_the_last_text_block_and_skip_images() {
    let mut sent = vec![json!({"role": "user", "content": [
        {"type": "text", "text": "look"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA"}},
    ]})];
    mark_cache_breakpoints(&mut sent);
    assert_eq!(
        sent[0]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert!(sent[0]["content"][1].get("cache_control").is_none());
}

#[test]
fn prepare_marks_breakpoints_only_when_the_model_asks() {
    let mut model = ResolvedModel {
        alias: "m".into(),
        model: "claude".into(),
        display_name: None,
        protocol: crate::config::Protocol::OpenAi,
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
        json!({"role": "system", "content": "rules"}),
        json!({"role": "user", "content": "hi"}),
    ];
    let plain: Value =
        serde_json::from_slice(&prepare(&model, &messages, &[], "k").unwrap().body).unwrap();
    assert_eq!(plain["messages"][1]["content"], "hi");
    model.cache_control = true;
    let marked: Value =
        serde_json::from_slice(&prepare(&model, &messages, &[], "k").unwrap().body).unwrap();
    assert_eq!(
        marked["messages"][1]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(messages[1]["content"], "hi");
}

#[test]
fn images_become_text_when_the_model_cannot_see_them() {
    let model = ResolvedModel {
        alias: "m".into(),
        model: "text".into(),
        display_name: None,
        protocol: crate::config::Protocol::OpenAi,
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
    let messages = vec![json!({"role": "user", "content": [
        {"type": "text", "text": "look"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
    ]})];
    let body: Value =
        serde_json::from_slice(&prepare(&model, &messages, &[], "k").unwrap().body).unwrap();
    let content = body["messages"][0]["content"].clone();
    assert!(
        content
            .as_array()
            .unwrap()
            .iter()
            .all(|part| part["type"] != "image_url"),
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
        messages[0]["content"][1]["type"], "image_url",
        "history itself stays as sent"
    );
}

#[test]
fn prepare_caps_output_from_context_and_preserves_string_reasoning() {
    let model = ResolvedModel {
        alias: "m".into(),
        model: "gpt-4".into(),
        display_name: None,
        protocol: crate::config::Protocol::OpenAi,
        base_url: "http://localhost".into(),
        api_key: String::new(),
        headers: Default::default(),
        tool_names: Default::default(),
        session_header: None,
        max_output: Some(2_000),
        context_window: Some(2_000),
        images: false,
        thinking: Thinking::Off,
        cache_control: false,
        chatgpt: false,
    };
    let messages = vec![
        json!({"role": "user", "content": "hello"}),
        json!({"role": "assistant", "reasoning_content": "plan", "content": "ok"}),
    ];
    let prepared = prepare(&model, &messages, &[], "cache").unwrap();
    let body: Value = serde_json::from_slice(&prepared.body).unwrap();
    let expected = super::cap_output(&model, &messages, &[], 2_000);
    assert_eq!(body["max_tokens"], expected);
    assert_eq!(body["messages"][1]["reasoning_content"], "plan");
    assert!(body.get("max_completion_tokens").is_none());
}

#[test]
fn unknown_delta_fields_are_kept_and_an_empty_reasoning_field_is_evidence() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let events = [
        json!({"choices":[{"delta":{"reasoning_content":""}}]}),
        json!({"choices":[{"delta":{"role":"assistant","deep_thought":"hm"}}]}),
        json!({"choices":[{"delta":{"deep_thought":"more"}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
    ];
    for event in &events {
        parser.event(event, &mut reply, &mut sink).unwrap();
    }
    assert_eq!(
        reply.reasoning_key,
        Some("reasoning_content"),
        "an empty field is still the field the provider used"
    );
    assert!(!reply.reasoning_kept());
    assert_eq!(
        reply.unread_fields,
        vec!["deep_thought".to_string()],
        "deduped, and role/content don't count"
    );
}

#[test]
fn mapped_tool_names_go_out_and_come_back() {
    let model = ResolvedModel {
        alias: "m".into(),
        model: "gpt".into(),
        display_name: None,
        protocol: crate::config::Protocol::OpenAi,
        base_url: "http://localhost".into(),
        api_key: String::new(),
        headers: Default::default(),
        tool_names: BTreeMap::from([("bash".to_string(), "run_command".to_string())]),
        session_header: None,
        max_output: None,
        context_window: None,
        images: false,
        thinking: Thinking::Off,
        cache_control: false,
        chatgpt: false,
    };
    let messages = vec![
        json!({"role": "assistant", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "bash", "arguments": "{}"}}]}),
    ];
    let tools = vec![
        json!({"type": "function", "function": {"name": "bash", "description": "Run", "parameters": {}}}),
    ];
    let prepared = prepare(&model, &messages, &tools, "k").unwrap();
    let body: Value = serde_json::from_slice(&prepared.body).unwrap();
    assert_eq!(body["tools"][0]["function"]["name"], "run_command");
    assert_eq!(
        body["messages"][0]["tool_calls"][0]["function"]["name"], "run_command",
        "the copy being sent carries the provider's names"
    );
    assert_eq!(
        messages[0]["tool_calls"][0]["function"]["name"], "bash",
        "history stays as stored"
    );

    let mut parser = Parser::new(&model.tool_names);
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    for event in [
        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"name": "run_command", "arguments": "{}"}}]}}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
    ] {
        parser.event(&event, &mut reply, &mut sink).unwrap();
    }
    parser.done();
    parser.finish(&mut reply).unwrap();
    assert_eq!(
        reply.tool_calls[0].name, "bash",
        "the call comes back under killi's name"
    );
}

/// An error object in a data line, read by its code (string or number) and its text.
fn mid_stream(error: Value) -> Failure {
    Parser::default()
        .event(
            &json!({ "error": error }),
            &mut Reply::default(),
            &mut |_: Delta| {},
        )
        .unwrap_err()
}

#[test]
fn errors_mid_stream_are_classified_by_code_and_text() {
    let limited = mid_stream(json!({"code": "rate_limit_exceeded", "message": "slow down"}));
    assert_eq!(
        (limited.kind, limited.retryable),
        (ErrorKind::RateLimited, true)
    );
    let spent = mid_stream(
        json!({"code": "insufficient_quota", "message": "You exceeded your current quota"}),
    );
    assert_eq!((spent.kind, spent.retryable), (ErrorKind::Quota, false));
    let long = mid_stream(json!({"code": "context_length_exceeded", "message": "too long"}));
    assert_eq!((long.kind, long.retryable), (ErrorKind::Overflow, false));
    let numbered = mid_stream(
        json!({"code": 400, "message": "This model's maximum context length is 128000 tokens"}),
    );
    assert_eq!(
        (numbered.kind, numbered.status),
        (ErrorKind::Overflow, Some(400))
    );
    let typed =
        mid_stream(json!({"code": null, "type": "invalid_request_error", "message": "bad"}));
    assert_eq!((typed.kind, typed.retryable), (ErrorKind::Invalid, false));
    let busy = mid_stream(json!({"code": 502, "message": "upstream error"}));
    assert_eq!((busy.kind, busy.retryable), (ErrorKind::Other, true));
    for code in [json!(1302), json!("1302")] {
        let throttled =
            mid_stream(json!({"code": code, "message": "Rate limit reached for requests"}));
        assert_eq!(
            (throttled.kind, throttled.retryable, throttled.status),
            (ErrorKind::RateLimited, true, None),
            "a gateway's own number says nothing, as a number or a string: {code}"
        );
    }
}

#[test]
fn a_refusal_delta_is_a_refusal() {
    let event = json!({"choices":[{"delta":{"refusal":"cannot help"}}]});
    let refused = Parser::default()
        .event(&event, &mut Reply::default(), &mut |_: Delta| {})
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Refused);
}

/// The calls a stream of tool-call fragments assembles, as (id, name, arguments).
fn assembled(fragments: &[Value]) -> Vec<(String, String, String)> {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    for fragment in fragments {
        let event = json!({"choices":[{"delta":{"tool_calls":[fragment]}}]});
        parser.event(&event, &mut reply, &mut sink).unwrap();
    }
    let done = json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]});
    parser.event(&done, &mut reply, &mut sink).unwrap();
    parser.finish(&mut reply).unwrap();
    reply
        .tool_calls
        .into_iter()
        .map(|call| (call.id, call.name, call.arguments))
        .collect()
}

fn call(id: &str, name: &str, arguments: &str) -> (String, String, String) {
    (id.into(), name.into(), arguments.into())
}

#[test]
fn fragments_without_an_index_continue_the_latest_call() {
    let calls = assembled(&[
        json!({"id": "a", "function": {"name": "read", "arguments": "{\"pa"}}),
        json!({"function": {"arguments": "th\":\"x\"}"}}),
        json!({"id": "b", "function": {"name": "grep", "arguments": "{}"}}),
    ]);
    assert_eq!(
        calls,
        [
            call("a", "read", r#"{"path":"x"}"#),
            call("b", "grep", "{}")
        ]
    );
}

#[test]
fn a_name_or_id_sent_again_is_not_doubled() {
    let calls = assembled(&[
        json!({"index": 0, "id": "a", "function": {"name": "read", "arguments": "{\"pa"}}),
        json!({"index": 0, "id": "a", "function": {"name": "read", "arguments": "th\":\"x\"}"}}),
    ]);
    assert_eq!(calls, [call("a", "read", r#"{"path":"x"}"#)]);
    let pieces = assembled(&[
        json!({"index": 0, "id": "a", "function": {"name": "re"}}),
        json!({"index": 0, "function": {"name": "ad", "arguments": "{}"}}),
    ]);
    assert_eq!(pieces, [call("a", "read", "{}")]);
}

#[test]
fn interleaved_calls_keep_their_own_fragments() {
    let calls = assembled(&[
        json!({"index": 0, "id": "a", "function": {"name": "read", "arguments": "{\"path\":"}}),
        json!({"index": 1, "id": "b", "function": {"name": "grep", "arguments": "{\"pattern\":"}}),
        json!({"index": 0, "function": {"arguments": "\"x\"}"}}),
        json!({"index": 1, "function": {"arguments": "\"y\"}"}}),
    ]);
    assert_eq!(
        calls,
        [
            call("a", "read", r#"{"path":"x"}"#),
            call("b", "grep", r#"{"pattern":"y"}"#)
        ]
    );
    let renumbered = assembled(&[
        json!({"index": 0, "id": "a", "function": {"name": "read", "arguments": "{}"}}),
        json!({"index": 0, "id": "b", "function": {"name": "grep", "arguments": "{}"}}),
    ]);
    assert_eq!(
        renumbered,
        [call("a", "read", "{}"), call("b", "grep", "{}")],
        "a new id under the same index is a new call"
    );
}

#[test]
fn a_fresh_id_without_a_name_continues_the_call() {
    let indexed = assembled(&[
        json!({"index": 0, "id": "a", "function": {"name": "read", "arguments": "{\"pa"}}),
        json!({"index": 0, "id": "a2", "function": {"arguments": "th\":\"x\"}"}}),
    ]);
    assert_eq!(indexed, [call("a", "read", r#"{"path":"x"}"#)]);
    let unindexed = assembled(&[
        json!({"id": "a", "function": {"name": "read", "arguments": "{\"pa"}}),
        json!({"id": "a2", "function": {"arguments": "th\":\"x\"}"}}),
        json!({"id": "b", "function": {"name": "grep", "arguments": "{}"}}),
    ]);
    assert_eq!(
        unindexed,
        [
            call("a", "read", r#"{"path":"x"}"#),
            call("b", "grep", "{}")
        ]
    );
}
