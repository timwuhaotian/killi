//! The `advisor` tool: the session's model asks a stronger one, the config's `advisor_model`,
//! for advice. The advisor reads the conversation exactly as sent, with a note after it that
//! makes it the advisor, and answers in text; that answer is the call's result. Nothing else
//! reaches the history, so the cached prefix stays as sent, and the advisor's request is billed
//! and logged on its own, never as the session's context.

use super::*;

/// What the advisor is told after the conversation it reads.
pub(super) const ADVISOR_NOTE: &str = "<system-reminder>You are now the advisor, not the agent: a stronger model that the agent working in this conversation consulted with its `advisor` call. You can't run tools, so answer in text only. Read the whole conversation, then advise the agent on what to do next: anything it misread or missed in the request, a better approach if there is one, risks and edge cases, and what to check before calling the work done. Be concrete, rank your points by importance and keep them brief. If the current approach is sound, say so in a line. Address the agent, not the user.</system-reminder>";

/// The result the advisor sees for the call that consults it.
const ASKING: &str = "This is the call consulting you; your reply becomes its result.";

/// The result the advisor sees for a call in the same reply that hasn't run yet.
const NOT_RUN: &str = "Not run yet: it comes after the advisor call.";

/// The advisor's request: the history as sent, a placeholder result for each call of the last
/// reply still open (providers reject a message between a call and its result), then the note.
pub(super) fn advisor_request(history: &[Value]) -> Vec<Value> {
    let mut request = history.to_vec();
    let names: HashMap<&str, &str> = history
        .iter()
        .rev()
        .find(|m| m["role"] == "assistant")
        .and_then(|m| m["tool_calls"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|c| Some((c["id"].as_str()?, c["function"]["name"].as_str()?)))
        .collect();
    for id in unanswered(history) {
        let content = if names.get(id.as_str()) == Some(&"advisor") {
            ASKING
        } else {
            NOT_RUN
        };
        request.push(json!({"role": "tool", "tool_call_id": id, "content": content}));
    }
    request.push(json!({"role": "user", "content": ADVISOR_NOTE}));
    request
}

/// The call's result from the advisor's reply: its text as advice, or why there is none.
pub(super) fn advice_output(alias: &str, text: &str) -> tools::Output {
    let advice = text.trim();
    if advice.is_empty() {
        return tools::Output::error(format!(
            "The advisor ({alias}) sent no advice. Carry on without it."
        ));
    }
    let mut output = tools::Output::ok(format!(
        "Advice from {alias}, a stronger model that read this conversation:\n\n{advice}"
    ))
    .brief(format!(
        "{alias} · {}",
        tools::count(advice.lines().count(), "line")
    ));
    output.display = Some(advice.to_string());
    output
}

impl Agent {
    /// Sets the model the `advisor` tool asks.
    pub fn set_advisor(&mut self, client: Client) {
        self.advisor = Some(Rc::new(client));
    }

    /// Drops the advisor: the session keeps its tool, whose calls now say none is set.
    pub fn clear_advisor(&mut self) {
        self.advisor = None;
    }

    /// Whether this session started with the `advisor` tool; only such a session can use an
    /// advisor, since its tools are fixed when it starts.
    pub fn has_advisor_tool(&self) -> bool {
        self.tools
            .iter()
            .any(|tool| tool["function"]["name"] == "advisor")
    }

    /// Runs an `advisor` call. Every failure is the call's error result, never the turn's: the
    /// model carries on without advice.
    pub(super) async fn consult_advisor(&mut self) -> tools::Output {
        if self.depth > 0 {
            return tools::Output::error(
                "Only the main session consults the advisor. Do the assigned work yourself and report open questions to your parent.",
            );
        }
        let Some(advisor) = self.advisor.clone() else {
            return tools::Output::error(
                "No advisor is available to this session now; the user can set one with /advisor. Carry on without it.",
            );
        };
        let alias = advisor.model.alias.clone();
        if let Some(window) = advisor.model.context_window
            && self.stats.context > window
        {
            return tools::Output::error(format!(
                "This conversation (~{} tokens) is larger than the advisor {alias}'s context window ({window}). Carry on without it.",
                self.stats.context
            ));
        }
        let request = advisor_request(&self.messages);
        let started = Instant::now();
        let sink = self.sink.clone();
        let mut on_delta = |delta: Delta| {
            if let Delta::Retry {
                attempt,
                wait,
                reason,
            } = delta
            {
                sink(Event::Sub {
                    label: "advisor".to_string(),
                    slot: None,
                    event: Box::new(Event::Retry {
                        attempt,
                        wait,
                        reason: reason.to_string(),
                    }),
                });
            }
        };
        let reply = advisor
            .complete(&request, &self.tools, &self.cache_key, &mut on_delta)
            .await;
        let took = started.elapsed();
        match reply {
            Ok(reply) => {
                let output = advice_output(&alias, &reply.text);
                self.account_advisor(&alias, Some(&reply.usage), took, output.is_error);
                output
            }
            Err(error) => {
                self.account_advisor(&alias, None, took, true);
                tools::Output::error(format!(
                    "The advisor ({alias}) couldn't answer: {error:#}. Carry on without it."
                ))
            }
        }
    }

    /// Counts the advisor's tokens in the session's totals and logs them as an `advisor`
    /// entry, not a `usage` one: a resume takes its context from the last `usage`, and the
    /// advisor's prompt size says nothing about this session's.
    fn account_advisor(
        &mut self,
        alias: &str,
        usage: Option<&Usage>,
        took: Duration,
        failed: bool,
    ) {
        if let Some(usage) = usage {
            let total = &mut self.stats.usage;
            total.prompt += usage.prompt;
            total.cached += usage.cached;
            total.completion += usage.completion;
            total.reasoning += usage.reasoning;
            (self.sink)(Event::Sub {
                label: "advisor".to_string(),
                slot: None,
                event: Box::new(Event::Usage(usage.clone())),
            });
        }
        let usage = usage.cloned().unwrap_or_default();
        let record = json!({
            "type": "advisor",
            "model": alias,
            "prompt": usage.prompt,
            "cached": usage.cached,
            "completion": usage.completion,
            "reasoning": usage.reasoning,
            "ms": millis(took),
            "is_error": failed,
        });
        if let Err(error) = self.session.record(&record) {
            (self.sink)(Event::Notice(format!(
                "Could not log the advisor's usage: {error:#}"
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use super::*;
    use crate::agent::tests::{idle_agent, sse, test_client_at};

    /// Answers each request with the next of `replies`, raw HTTP, and keeps the request bodies.
    async fn recording_stub(replies: Vec<String>) -> (String, Arc<StdMutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(StdMutex::new(Vec::new()));
        let seen = bodies.clone();
        tokio::spawn(async move {
            for reply in replies {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                let body = loop {
                    if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if buffer.len() >= end + 4 + length {
                            break String::from_utf8_lossy(&buffer[end + 4..]).into_owned();
                        }
                    }
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => break String::new(),
                        Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                    }
                };
                seen.lock().unwrap().push(body);
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        (url, bodies)
    }

    fn ok(sse_body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{sse_body}"
        )
    }

    /// One finished SSE reply with `text` that bills `prompt` input tokens.
    fn billed(text: &str, prompt: u64) -> String {
        format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":\"stop\"}}]}}\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":{prompt},\"completion_tokens\":5}}}}\n\ndata: [DONE]\n\n"
        )
    }

    /// One SSE reply that calls `advisor` as `id`.
    fn advisor_call(id: &str) -> String {
        format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"{id}\",\"function\":{{\"name\":\"advisor\",\"arguments\":\"{{}}\"}}}}]}}}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n"
        )
    }

    /// An agent on the main stub `main`, with the advisor tool and, when `advisor` is given,
    /// an advisor on that stub.
    fn advised_agent(name: &str, main: &str, advisor: Option<&str>) -> Agent {
        let mut agent = idle_agent(name, Controls::default(), Rc::new(|_| {}));
        Rc::get_mut(&mut agent.client).unwrap().model.base_url = main.to_string();
        agent.tools = Rc::new(vec![tools::advisor_definition()]);
        if let Some(url) = advisor {
            agent.set_advisor(test_client_at("strong", url));
        }
        agent
            .push(json!({"role": "system", "content": "sys"}))
            .unwrap();
        agent
    }

    fn tool_results(agent: &Agent) -> Vec<&str> {
        agent
            .messages
            .iter()
            .filter(|m| m["role"] == "tool")
            .filter_map(|m| m["content"].as_str())
            .collect()
    }

    fn log_types(agent: &Agent) -> Vec<String> {
        std::fs::read_to_string(&agent.session.log_path)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|entry| entry["type"].as_str().map(String::from))
            .collect()
    }

    #[test]
    fn the_advisor_reads_the_history_with_its_own_call_marked_and_the_note_last() {
        let history = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "fix the bug"}),
            json!({"role": "assistant", "tool_calls": [
                {"id": "r", "type": "function", "function": {"name": "read", "arguments": "{}"}},
                {"id": "a", "type": "function", "function": {"name": "advisor", "arguments": "{}"}},
                {"id": "w", "type": "function", "function": {"name": "write", "arguments": "{}"}},
            ]}),
            json!({"role": "tool", "tool_call_id": "r", "content": "file text"}),
        ];
        let request = advisor_request(&history);
        assert_eq!(&request[..4], &history[..], "the history goes as sent");
        assert_eq!(request[4]["tool_call_id"], "a");
        assert_eq!(request[4]["content"], ASKING);
        assert_eq!(request[5]["tool_call_id"], "w");
        assert_eq!(request[5]["content"], NOT_RUN);
        assert_eq!(request[6], json!({"role": "user", "content": ADVISOR_NOTE}));
        assert_eq!(request.len(), 7);
    }

    #[test]
    fn empty_advice_is_an_error_and_advice_is_attributed() {
        assert!(advice_output("strong", "  \n").is_error);
        let output = advice_output("strong", "Check the empty case.\nThen test.");
        assert!(!output.is_error);
        assert!(
            output.text.starts_with("Advice from strong"),
            "{}",
            output.text
        );
        assert!(output.text.ends_with("Check the empty case.\nThen test."));
        assert_eq!(output.brief, "strong · 2 lines");
        assert_eq!(
            output.display.as_deref(),
            Some("Check the empty case.\nThen test.")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn advice_is_the_calls_result_and_the_advisor_is_billed_apart() {
        let (main, _) =
            recording_stub(vec![ok(&advisor_call("a1")), ok(&billed("Done.", 1200))]).await;
        let (strong, asked) =
            recording_stub(vec![ok(&billed("Check the empty case.", 90_000))]).await;
        let mut agent = advised_agent("advisor-happy", &main, Some(&strong));
        let (text, stop) = agent.run("fix it", &[]).await.unwrap();
        assert_eq!((text.as_str(), stop), ("Done.", Stop::Completed));
        let results = tool_results(&agent);
        assert_eq!(results.len(), 1);
        assert!(
            results[0].starts_with("Advice from strong"),
            "{}",
            results[0]
        );
        assert!(
            results[0].ends_with("Check the empty case."),
            "{}",
            results[0]
        );
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 1, "one request to the advisor");
        let body: Value = serde_json::from_str(&asked[0]).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.last().unwrap()["content"], ADVISOR_NOTE);
        assert!(
            messages.iter().any(|m| m["content"] == "fix it"),
            "the advisor reads the user's request: {messages:?}"
        );
        assert!(
            messages.iter().any(|m| m["content"] == ASKING),
            "its own call has a result in its copy: {messages:?}"
        );
        assert!(
            !agent.messages.iter().any(|m| m["content"] == ADVISOR_NOTE),
            "the note never reaches the session's history"
        );
        let types = log_types(&agent);
        assert_eq!(
            types.iter().filter(|t| *t == "advisor").count(),
            1,
            "{types:?}"
        );
        assert_eq!(
            types.iter().filter(|t| *t == "usage").count(),
            2,
            "only the main model's requests are `usage`, which a resume reads context from: {types:?}"
        );
        assert_eq!(
            agent.stats.context, 1200,
            "the advisor's prompt is not this session's context"
        );
        assert_eq!(
            agent.stats.usage.prompt, 91_200,
            "its tokens still count in the totals"
        );
        let report = crate::session::load_reported(&agent.session.log_path).unwrap();
        assert_eq!(report.skipped, 0);
        assert_eq!(
            report.loaded.context, 1200,
            "a resume reads the session's own context"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failing_or_silent_advisor_is_an_error_result_and_the_turn_goes_on() {
        let refused = "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"error\":{\"message\":\"model overloaded\"}}".to_string();
        let (strong, _) = recording_stub(vec![refused, ok(&sse("", "stop"))]).await;
        let (main, _) = recording_stub(vec![
            ok(&advisor_call("a1")),
            ok(&advisor_call("a2")),
            ok(&sse("Done anyway.", "stop")),
        ])
        .await;
        let mut agent = advised_agent("advisor-fails", &main, Some(&strong));
        let (text, stop) = agent.run("fix it", &[]).await.unwrap();
        assert_eq!((text.as_str(), stop), ("Done anyway.", Stop::Completed));
        let results = tool_results(&agent);
        assert!(
            results[0].contains("couldn't answer") && results[0].contains("model overloaded"),
            "{}",
            results[0]
        );
        assert!(results[1].contains("sent no advice"), "{}", results[1]);
        let types = log_types(&agent);
        assert_eq!(
            types.iter().filter(|t| *t == "advisor").count(),
            2,
            "{types:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn advice_that_changes_is_never_blocked_as_a_repeat() {
        let (strong, asked) = recording_stub(vec![
            ok(&sse("First advice.", "stop")),
            ok(&sse("Second advice.", "stop")),
            ok(&sse("Third advice.", "stop")),
        ])
        .await;
        let (main, _) = recording_stub(vec![
            ok(&advisor_call("a1")),
            ok(&advisor_call("a2")),
            ok(&advisor_call("a3")),
            ok(&sse("Done.", "stop")),
        ])
        .await;
        let mut agent = advised_agent("advisor-repeat", &main, Some(&strong));
        agent.run("fix it", &[]).await.unwrap();
        assert_eq!(
            asked.lock().unwrap().len(),
            3,
            "every call reached the advisor"
        );
        let results = tool_results(&agent);
        assert!(results[2].ends_with("Third advice."), "{results:?}");
        assert!(
            !agent.messages.iter().any(|m| m["content"]
                .as_str()
                .is_some_and(|text| text.contains("exact advisor call"))),
            "advice that changed is not a repetition"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn without_an_advisor_or_below_the_main_session_the_call_says_so() {
        let mut agent = advised_agent("advisor-none", "http://127.0.0.1:1", None);
        let output = agent.consult_advisor().await;
        assert!(
            output.is_error && output.text.contains("/advisor"),
            "{}",
            output.text
        );
        agent.set_advisor(test_client_at("strong", "http://127.0.0.1:1"));
        agent.depth = 1;
        let output = agent.consult_advisor().await;
        assert!(
            output.is_error && output.text.contains("main session"),
            "{}",
            output.text
        );
        agent.depth = 0;
        agent.clear_advisor();
        assert!(
            agent
                .consult_advisor()
                .await
                .text
                .contains("No advisor is available")
        );
        assert!(
            agent.has_advisor_tool(),
            "turning it off keeps the session's tools as they are"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_conversation_larger_than_the_advisors_window_is_not_sent() {
        let mut agent = advised_agent("advisor-window", "http://127.0.0.1:1", None);
        let mut advisor = test_client_at("strong", "http://127.0.0.1:1");
        advisor.model.context_window = Some(1000);
        agent.set_advisor(advisor);
        agent.stats.context = 5000;
        let output = agent.consult_advisor().await;
        assert!(
            output.is_error && output.text.contains("context window"),
            "{}",
            output.text
        );
    }
}
