//! How a turn's answer is put together when its last replies aren't simply the answer: a
//! reply cut off mid-thought, and a report that a reminder at the turn's end sent back.

use super::super::ending::{CUT_THINKING, answer};
use super::*;

/// One SSE reply that only reasons and stops at the output token limit.
fn cut_thought(reasoning: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"reasoning_content\":\"{reasoning}\"}},\"finish_reason\":\"length\"}}]}}\n\ndata: [DONE]\n\n"
    )
}

/// One SSE reply that makes a single tool call.
fn tool_call(name: &str, arguments: Value) -> String {
    let chunk = json!({"choices": [{"delta": {"tool_calls": [{
        "index": 0, "id": "a", "function": {"name": name, "arguments": arguments.to_string()},
    }]}, "finish_reason": "tool_calls"}]});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

async fn agent_answering(name: &str, replies: Vec<String>) -> Agent {
    let url = stub_replies(replies).await;
    let mut agent = idle_agent(name, Controls::default(), Rc::new(|_| {}));
    Rc::get_mut(&mut agent.client).unwrap().model.base_url = url;
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    agent
}

fn reminders(agent: &Agent, text: &str) -> usize {
    agent
        .messages
        .iter()
        .filter(|m| m["content"] == text)
        .count()
}

#[tokio::test(flavor = "current_thread")]
async fn a_reply_cut_off_while_still_thinking_is_not_kept() {
    let mut agent = agent_answering(
        "cut-thought",
        vec![cut_thought("round and round"), sse("Done.", "stop")],
    )
    .await;
    let (text, stop) = agent.run("fix it", &[]).await.unwrap();
    assert_eq!((text.as_str(), stop), ("Done.", Stop::Completed));
    assert_eq!(reminders(&agent, CUT_THINKING), 1);
    let history = serde_json::to_string(&*agent.messages).unwrap();
    assert!(!history.contains("round and round"), "{history}");
    let assistants = agent
        .messages
        .iter()
        .filter(|m| m["role"] == "assistant")
        .count();
    assert_eq!(assistants, 1, "only the reply that said something");
    let log = std::fs::read_to_string(&agent.session.log_path).unwrap();
    assert!(
        log.contains("hit the output token limit while still thinking (15 characters"),
        "{log}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_turn_that_only_ever_thinks_ends_at_the_output_limit() {
    let mut agent = agent_answering("cut-thought-always", vec![cut_thought("hm"); 4]).await;
    let (text, stop) = agent.run("fix it", &[]).await.unwrap();
    assert_eq!((text.as_str(), stop), ("", Stop::MaxTokens));
    assert_eq!(reminders(&agent, CUT_THINKING), 3);
    assert!(
        agent.messages.iter().all(|m| m["role"] != "assistant"),
        "{:?}",
        agent.messages
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_report_the_open_task_reminder_sent_back_stays_in_the_answer() {
    let done = json!({"todos": [{"content": "review the diff", "status": "completed"}]});
    let mut agent = agent_answering(
        "held-report",
        vec![
            sse("REPORT: two findings.", "stop"),
            tool_call("todo_write", done),
            sse("List closed; the report is above.", "stop"),
        ],
    )
    .await;
    let open = json!({"todos": [{"content": "review the diff", "status": "in_progress"}]});
    assert!(!agent.todo_write(&open).is_error);
    let (text, stop) = agent.run("review it", &[]).await.unwrap();
    assert_eq!(stop, Stop::Completed);
    assert_eq!(
        text,
        "REPORT: two findings.\n\nList closed; the report is above."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn work_after_the_reminder_makes_the_last_reply_the_answer() {
    let mut agent = agent_answering(
        "held-then-work",
        vec![
            sse("Draft: one finding so far.", "stop"),
            tool_call("glob", json!({"pattern": "*.none"})),
            sse("Final: two findings.", "stop"),
        ],
    )
    .await;
    let open = json!({"todos": [{"content": "review the diff", "status": "in_progress"}]});
    assert!(!agent.todo_write(&open).is_error);
    let (text, _) = agent.run("review it", &[]).await.unwrap();
    assert_eq!(text, "Final: two findings.");
}

#[test]
fn an_answer_that_repeats_the_held_reply_is_not_doubled() {
    assert_eq!(answer(String::new(), "last".into()), "last");
    assert_eq!(answer("report".into(), String::new()), "report");
    assert_eq!(
        answer("report".into(), "report, and the list is closed".into()),
        "report, and the list is closed"
    );
    assert_eq!(
        answer("report".into(), "see above".into()),
        "report\n\nsee above"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_session_the_user_watches_keeps_its_last_reply_as_the_answer() {
    let url = stub_replies(vec![
        sse("REPORT: two findings.", "stop"),
        sse("The list waits on your call.", "stop"),
    ])
    .await;
    let controls = Controls {
        interactive: true,
        ..Controls::default()
    };
    let mut agent = idle_agent("held-interactive", controls, Rc::new(|_| {}));
    Rc::get_mut(&mut agent.client).unwrap().model.base_url = url;
    let open = json!({"todos": [{"content": "review the diff", "status": "in_progress"}]});
    assert!(!agent.todo_write(&open).is_error);
    let (text, _) = agent.run("review it", &[]).await.unwrap();
    assert_eq!(text, "The list waits on your call.");
}
