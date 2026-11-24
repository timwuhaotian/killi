//! Swarms: how many subagents one call runs at once.

use super::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A stub that answers every request with a finished SSE reply, records how many requests
/// were in flight at once, and holds each one for `hold` so overlap is visible.
async fn concurrent_stub(hold: Duration) -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let peak_out = peak.clone();
    let live_out = live.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let live = live.clone();
            let peak = peak.clone();
            tokio::spawn(async move {
                live.fetch_add(1, Ordering::SeqCst);
                peak.fetch_max(live.load(Ordering::SeqCst), Ordering::SeqCst);
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                    }
                }
                tokio::time::sleep(hold).await;
                let body = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{}",
                    sse("done Status: ready", "stop")
                );
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.shutdown().await;
                live.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    (url, peak_out, live_out)
}

#[tokio::test(flavor = "current_thread")]
async fn a_swarm_runs_swarm_parallel_members_at_once() {
    let (url, peak, live_out) = concurrent_stub(Duration::from_millis(300)).await;
    let mut agent = idle_agent("swarm-parallel", Controls::default(), Rc::new(|_| {}));
    agent.swarm_parallel.set(32);
    agent.client = Rc::new(test_client_at("primary", &url));
    agent
        .push(json!({"role": "system", "content": "sys"}))
        .unwrap();
    let items: Vec<String> = (1..=12).map(|n| n.to_string()).collect();
    let call = ToolCall {
        extra_content: None,
        id: "s1".into(),
        name: "agent_swarm".into(),
        arguments: json!({"prompt_template": "Do {{item}}.", "items": items}).to_string(),
    };
    let output = agent.run_swarm("s1", &call).await;
    assert!(!output.is_error, "{}", output.text);
    assert_eq!(live_out.load(Ordering::SeqCst), 0, "every member ended");
    assert!(
        peak.load(Ordering::SeqCst) >= 10,
        "expected ~12 at once, got {}",
        peak.load(Ordering::SeqCst)
    );
}
