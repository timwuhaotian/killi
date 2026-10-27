use super::*;

#[test]
fn claude_versions_pick_thinking_and_limits() {
    let opus = ClaudeVersion::parse("claude-opus-5-5").unwrap();
    assert!(!opus.budget_thinking());
    assert_eq!(opus.max_tokens(), 128_000);
    let sonnet = ClaudeVersion::parse("claude-sonnet-4-5-20250929").unwrap();
    assert!(sonnet.budget_thinking());
    assert_eq!(sonnet.max_tokens(), 64_000);
    assert!(ClaudeVersion::parse("MiniMax-M3.1-Flash").is_none());
}

#[test]
fn history_converts_with_merged_tool_results_and_one_cache_breakpoint() {
    let history = vec![
        json!({"role": "user", "content": "fix it"}),
        json!({"role": "assistant", "reasoning_items": [{"type": "thinking", "thinking": "hm", "signature": "sig"}],
               "tool_calls": [
                   {"id": "a", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"x\"}"}},
                   {"id": "b", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"y\"}"}}]}),
        json!({"role": "tool", "tool_call_id": "a", "content": "one"}),
        json!({"role": "tool", "tool_call_id": "b", "content": ""}),
        json!({"role": "user", "content": "<system-reminder>note</system-reminder>"}),
    ];
    let out = convert_messages(&history, true, true);
    assert_eq!(out.len(), 3);
    assert_eq!(out[1]["content"][0], json!({"type": "thinking", "thinking": "hm", "signature": "sig"}));
    assert_eq!(out[1]["content"][1]["input"], json!({"path": "x"}));
    let user = out[2]["content"].as_array().unwrap();
    assert_eq!(user.len(), 3);
    assert_eq!(user[1]["content"], "(empty)");
    assert_eq!(user[2]["cache_control"], json!({"type": "ephemeral"}));
    assert!(user[0].get("cache_control").is_none());
}

#[test]
fn a_prompt_and_its_pasted_images_are_one_user_turn() {
    let history = vec![
        json!({"role": "user", "content": "what is [Image #1]"}),
        json!({"role": "user", "content": [
            {"type": "text", "text": "[Image #1]:"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]}),
    ];
    let out = convert_messages(&history, true, true);
    assert_eq!(out.len(), 1);
    let blocks = out[0]["content"].as_array().unwrap();
    assert_eq!(blocks[1], json!({"type": "text", "text": "[Image #1]:"}));
    assert_eq!(blocks[2]["source"], json!({"type": "base64", "media_type": "image/png", "data": "AAAA"}));
}

#[test]
fn images_become_text_when_the_model_cannot_see_them() {
    let history = vec![json!({"role": "user", "content": [
        {"type": "text", "text": "look"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
    ]})];
    let out = convert_messages(&history, true, false);
    let blocks = out[0]["content"].as_array().unwrap();
    assert!(blocks.iter().all(|block| block["type"] != "image"), "{blocks:?}");
    assert!(blocks.iter().any(|block| block["text"] == super::IMAGE_OMITTED), "{blocks:?}");
    assert_eq!(history[0]["content"][1]["type"], "image_url", "history itself stays as sent");
}

#[test]
fn unsigned_thinking_is_dropped_only_for_claude() {
    let message = json!({"role": "assistant", "reasoning_content": "plan", "content": "ok"});
    assert_eq!(assistant_blocks(&message, false)[0], json!({"type": "thinking", "thinking": "plan"}));
    assert_eq!(assistant_blocks(&message, true).len(), 1);
}

#[test]
fn chat_reasoning_items_do_not_hide_scalar_reasoning_from_anthropic() {
    let message = json!({
        "role": "assistant",
        "reasoning_content": "plan",
        "reasoning_items": [{"type": "reasoning.text", "text": "plan"}],
        "content": "ok"
    });
    assert_eq!(assistant_blocks(&message, false)[0], json!({"type": "thinking", "thinking": "plan"}));
}

#[test]
fn whitespace_only_assistant_text_is_skipped() {
    let message = json!({
        "role": "assistant",
        "content": "  \n  ",
        "tool_calls": [
            {"id": "a", "type": "function", "function": {"name": "read", "arguments": "{\"path\":\"x\"}"}}
        ]
    });
    let blocks = assistant_blocks(&message, true);
    assert_eq!(blocks.len(), 1, "whitespace text is not a block Anthropics accepts: {blocks:?}");
    assert_eq!(blocks[0]["type"], "tool_use");
    assert!(assistant_blocks(&json!({"role": "assistant", "content": " \n "}), true).is_empty());
}

#[test]
fn stream_assembles_thinking_text_and_tool_calls() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let events = [
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 5, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "plan"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "s1"}}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Reading."}}),
        json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "t1", "name": "read", "input": {}}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}}),
        json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "\"a\"}"}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 9}}),
        json!({"type": "message_stop"}),
    ];
    for event in &events {
        parser.event(event, &mut reply, &mut sink).ok().unwrap();
    }
    parser.finish(&mut reply).unwrap();
    assert_eq!(reply.reasoning_items[0], json!({"type": "thinking", "thinking": "plan", "signature": "s1"}));
    assert_eq!(reply.text, "Reading.");
    assert_eq!(reply.tool_calls[0].arguments, "{\"path\":\"a\"}");
    assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!((reply.usage.prompt, reply.usage.cached, reply.usage.completion), (105, 100, 9));
}

#[test]
fn thinking_and_signature_deltas_add_up() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    parser.event(&json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}), &mut reply, &mut sink).ok().unwrap();
    for part in ["a", "b", "c"] {
        parser.event(&json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": part}}), &mut reply, &mut sink).ok().unwrap();
        parser.event(&json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": part.to_uppercase()}}), &mut reply, &mut sink).ok().unwrap();
    }
    assert_eq!(reply.reasoning_items[0], json!({"type": "thinking", "thinking": "abc", "signature": "ABC"}));
    assert_eq!(reply.reasoning, "abc");
}

#[test]
fn max_tokens_is_capped_by_remaining_context() {
    let model = ResolvedModel {
        alias: "m".into(),
        model: "compatible".into(),
        protocol: crate::config::Protocol::Anthropic,
        base_url: String::new(),
        api_key: String::new(),
        headers: Default::default(),
        session_header: None,
        max_output: Some(4_000),
        context_window: Some(2_000),
        images: false,
        thinking: Thinking::Off,
        cache_control: false,
        chatgpt: false,
    };
    let messages = vec![json!({"role": "user", "content": "hello"})];
    let expected = super::cap_output(&model, &messages, &[], 4_000);
    assert_eq!(max_tokens(&model, None, &messages, &[]), expected);
    assert!(expected < 2_000);
}

#[test]
fn message_stop_is_required() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    parser.event(&json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}), &mut reply, &mut sink).unwrap();
    assert!(parser.finish(&mut reply).unwrap_err().retryable);
    parser.event(&json!({"type": "message_stop"}), &mut reply, &mut sink).unwrap();
    parser.finish(&mut reply).unwrap();
}

#[test]
fn thinking_budget_leaves_visible_output_even_at_tiny_limits() {
    let budget = |max| thinking_params(&Thinking::On, true, max);
    assert_eq!(budget(1)[0].1, json!({"type": "disabled"}));
    assert_eq!(budget(1_024)[0].1, json!({"type": "disabled"}));
    assert_eq!(budget(1_025)[0].1["budget_tokens"], 1_024);
    assert_eq!(budget(2_000)[0].1["budget_tokens"], 1_999);
    assert_eq!(budget(64_000)[0].1["budget_tokens"], 32_000);
}

#[test]
fn minimax_usage_in_message_delta_wins() {
    let mut parser = Parser::default();
    let mut reply = Reply::default();
    let mut sink = |_: Delta| {};
    let events = [
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 2200}}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"input_tokens": 2, "output_tokens": 2, "cache_read_input_tokens": 2198}}),
    ];
    for event in &events {
        parser.event(event, &mut reply, &mut sink).ok().unwrap();
    }
    assert_eq!((reply.usage.prompt, reply.usage.cached), (2200, 2198));
}
