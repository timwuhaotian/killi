use super::*;

mod session;
mod turn;
mod units;

fn call(arguments: &str) -> ToolCall {
    ToolCall { id: "c".into(), name: "read".into(), arguments: arguments.into() }
}

/// A client that talks to a test stub instead of a provider.
fn test_client() -> Client {
    test_client_at("test", "")
}

/// A client that talks to the stub at `base_url`, named `alias`.
fn test_client_at(alias: &str, base_url: &str) -> Client {
    let model = crate::config::ResolvedModel {
        alias: alias.into(),
        model: "test".into(),
        protocol: crate::config::Protocol::OpenAi,
        base_url: base_url.into(),
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
    Client::new(model).unwrap()
}

/// Serves `replies` as SSE responses on 127.0.0.1 and returns the base URL to point a
/// test client at.
async fn stub_replies(replies: Vec<String>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buffer.len() >= end + 4 + length {
                        break;
                    }
                }
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                }
            }
            let body = format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{reply}");
            let _ = socket.write_all(body.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    url
}

/// Serves `replies` as raw HTTP responses on 127.0.0.1 and returns the base URL to point
/// a test client at. Each reply is written whole and the connection closed, so a client may
/// send several requests and see them answered in order.
async fn serve_raw(replies: Vec<String>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        for reply in replies {
            let Ok((mut socket, _)) = listener.accept().await else { break };
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buffer.len() >= end + 4 + length {
                        break;
                    }
                }
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                }
            }
            let _ = socket.write_all(reply.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    url
}

/// One SSE reply with `text` and a choice-level `finish_reason`.
fn sse(text: &str, finish_reason: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":\"{finish_reason}\"}}]}}\n\ndata: [DONE]\n\n"
    )
}

/// An agent that has never asked a model anything: enough to run a reply's tool calls.
/// Its scratch folder goes when the agent does, so `cargo test` leaves nothing behind.
fn idle_agent(name: &str, controls: Controls, sink: Sink) -> Agent {
    let tmp = crate::test_temp::TempDir::new(&format!("killi-{name}"));
    let log = tmp.join("session.jsonl");
    std::fs::write(&log, "").unwrap();
    let mut agent = Agent::assemble(test_client(), tmp.path(), Vec::new(), Settings::default(), controls, sink, Session::open(&log).unwrap());
    Rc::get_mut(&mut agent.ctx).expect("a fresh agent owns its context").temp = Some(tmp);
    agent
}

/// A stub that answers every request with a 429 and no wait, so `complete` burns its whole
/// retry budget on it at once.
async fn rate_limited_stub(attempts: usize) -> String {
    let limited = "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\nretry-after: 0\r\nconnection: close\r\n\r\n{\"error\":{\"message\":\"slow down\"}}".to_string();
    serve_raw(vec![limited; attempts]).await
}

/// A stub that answers one request with a finished SSE reply.
async fn answering_stub(text: &str) -> String {
    answering_stub_times(text, 1).await
}

/// A stub that answers `times` requests with a finished SSE reply.
async fn answering_stub_times(text: &str, times: usize) -> String {
    let good = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{}",
        sse(text, "stop")
    );
    serve_raw(vec![good; times]).await
}
