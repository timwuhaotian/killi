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
        parser.event(&serde_json::from_str(event).unwrap(), &mut reply, &mut sink).ok().unwrap();
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
    assert!(!parser.event(&event, &mut reply, &mut sink).unwrap_err().retryable);
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
    assert_eq!(reply.tool_calls.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(), ["read", "grep"]);
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
        Parser::default().event(event, &mut reply, &mut sink).ok().unwrap();
    }
    let mut parser = Parser::default();
    assert!(parser.finish(&mut reply).unwrap_err().retryable);
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
    merge_reasoning_detail(&mut reply, &json!({"type":"reasoning.summary","index":0,"summary":"think "}));
    merge_reasoning_detail(&mut reply, &json!({"type":"reasoning.encrypted","index":0,"data":"cipher"}));
    merge_reasoning_detail(&mut reply, &json!({"type":"reasoning.summary","index":0,"summary":"more"}));
    assert_eq!(reply.reasoning_items.len(), 2);
    assert_eq!(reply.reasoning_items[0]["summary"], "think more");
    assert!(reply.reasoning_items[0].get("data").is_none());
    assert_eq!(reply.reasoning_items[1]["data"], "cipher");

    merge_reasoning_detail(&mut reply, &json!({"type":"summary","id":null,"summary":"part "}));
    merge_reasoning_detail(&mut reply, &json!({"type":"summary","id":null,"summary":"two"}));
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
    assert_eq!(replayed[0]["reasoning_details"], json!([{"type": "reasoning.text", "index": 0, "text": "plan"}]));
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
    assert_eq!(sent[0]["content"], json!([{"type": "text", "text": "rules", "cache_control": mark}]));
    assert_eq!(sent[1]["content"], "run it");
    assert!(sent[2]["content"].is_null());
    assert_eq!(sent[3]["content"], json!([{"type": "text", "text": "done", "cache_control": mark}]));
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
    assert_eq!(sent[0]["content"][0]["cache_control"], json!({"type": "ephemeral"}));
    assert!(sent[0]["content"][1].get("cache_control").is_none());
}

#[test]
fn prepare_marks_breakpoints_only_when_the_model_asks() {
    let mut model = ResolvedModel {
        alias: "m".into(),
        model: "claude".into(),
        protocol: crate::config::Protocol::OpenAi,
        base_url: "http://localhost".into(),
        api_key: String::new(),
        headers: Default::default(),
        session_header: None,
        max_output: None,
        context_window: None,
        images: false,
        thinking: Thinking::Off,
        cache_control: false,
        chatgpt: false,
    };
    let messages = vec![json!({"role": "system", "content": "rules"}), json!({"role": "user", "content": "hi"})];
    let plain: Value = serde_json::from_slice(&prepare(&model, &messages, &[], "k").unwrap().body).unwrap();
    assert_eq!(plain["messages"][1]["content"], "hi");
    model.cache_control = true;
    let marked: Value = serde_json::from_slice(&prepare(&model, &messages, &[], "k").unwrap().body).unwrap();
    assert_eq!(marked["messages"][1]["content"][0]["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(messages[1]["content"], "hi");
}

#[test]
fn images_become_text_when_the_model_cannot_see_them() {
    let model = ResolvedModel {
        alias: "m".into(),
        model: "text".into(),
        protocol: crate::config::Protocol::OpenAi,
        base_url: "http://localhost".into(),
        api_key: String::new(),
        headers: Default::default(),
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
    let body: Value = serde_json::from_slice(&prepare(&model, &messages, &[], "k").unwrap().body).unwrap();
    let content = body["messages"][0]["content"].clone();
    assert!(content.as_array().unwrap().iter().all(|part| part["type"] != "image_url"), "{content}");
    assert!(content.as_array().unwrap().iter().any(|part| part["text"] == super::IMAGE_OMITTED), "{content}");
    assert_eq!(messages[0]["content"][1]["type"], "image_url", "history itself stays as sent");
}

#[test]
fn prepare_caps_output_from_context_and_preserves_string_reasoning() {
    let model = ResolvedModel {
        alias: "m".into(),
        model: "gpt-4".into(),
        protocol: crate::config::Protocol::OpenAi,
        base_url: "http://localhost".into(),
        api_key: String::new(),
        headers: Default::default(),
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
