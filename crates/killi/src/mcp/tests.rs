use super::*;

/// A tiny MCP server in bash: answers initialize, tools/list and tools/call.
    const SERVER: &str = r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"t","version":"1"}}}\n' "$id" ;;
    *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}}\n' "$id" ;;
    *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echoed by %s"}]}}\n' "$id" "$$" ;;
  esac
done"#;

fn mcp() -> Mcp {
    let config = ServerConfig {
        command: Some("/bin/bash".into()),
        args: vec!["-c".into(), SERVER.into()],
        env: BTreeMap::new(),
        url: None,
        headers: BTreeMap::new(),
        bearer_token_env_var: None,
        enabled: None,
        tool_timeout_ms: Some(5_000),
        cwd: std::env::temp_dir(),
    };
    Mcp { servers: BTreeMap::from([("t".to_string(), config)]), ..Mcp::default() }
}

async fn echo(mcp: &Mcp) -> String {
    mcp.run(&json!({"server": "t", "tool": "echo", "arguments": {}}), false).await.text
}

fn server(command: Option<&str>, args: &[&str], url: Option<&str>) -> ServerConfig {
    ServerConfig {
        command: command.map(String::from),
        args: args.iter().map(|s| s.to_string()).collect(),
        env: BTreeMap::new(),
        url: url.map(String::from),
        headers: BTreeMap::new(),
        bearer_token_env_var: None,
        enabled: None,
        tool_timeout_ms: None,
        cwd: std::env::temp_dir(),
    }
}

#[test]
fn the_trust_input_covers_more_than_the_command_line() {
    let mut config = server(Some("/bin/serve"), &["--port", "1"], None);
    let mut mcp = Mcp { servers: BTreeMap::from([("t".to_string(), config.clone())]), project: BTreeSet::from(["t".to_string()]), ..Mcp::default() };
    assert!(mcp.is_project("t") && !mcp.is_project("other"));
    let before = mcp.trust_input("t").unwrap();
    config.env.insert("NODE_OPTIONS".into(), "--require ./evil.js".into());
    mcp.servers.insert("t".to_string(), config.clone());
    assert_ne!(mcp.trust_input("t").unwrap(), before, "an env change re-asks");
    let before = mcp.trust_input("t").unwrap();
    config.bearer_token_env_var = Some("OTHER_TOKEN".into());
    mcp.servers.insert("t".to_string(), config);
    assert_ne!(mcp.trust_input("t").unwrap(), before, "a credentials change re-asks");
    assert_eq!(mcp.command_line("t").unwrap(), "/bin/serve --port 1", "the display is unchanged by either");
}

#[test]
fn command_line_names_what_a_server_would_run() {
    let home = crate::config::home();
    let mcp = Mcp {
        servers: BTreeMap::from([
            ("local".to_string(), server(Some("/bin/serve"), &["--port", "1"], None)),
            ("home".to_string(), server(Some("~/bin/serve"), &[], None)),
            ("remote".to_string(), server(None, &[], Some("https://mcp.example/rpc"))),
        ]),
        ..Mcp::default()
    };
    assert_eq!(mcp.command_line("local").as_deref(), Some("/bin/serve --port 1"));
    let expanded = format!("{}/bin/serve", home.display());
    assert_eq!(mcp.command_line("home").as_deref(), Some(expanded.as_str()));
    assert_eq!(mcp.command_line("remote").as_deref(), Some("https://mcp.example/rpc"));
    assert!(mcp.command_line("missing").is_none());
}

#[test]
fn arguments_accept_an_object_or_a_string_holding_one() {
    assert_eq!(Mcp::tool_arguments(&json!({})).unwrap(), json!({}));
    assert_eq!(Mcp::tool_arguments(&json!({"arguments": {"text": "hi"}})).unwrap(), json!({"text": "hi"}));
    assert_eq!(Mcp::tool_arguments(&json!({"arguments": "{\"text\": \"hi\"}"})).unwrap(), json!({"text": "hi"}));
    assert!(Mcp::tool_arguments(&json!({"arguments": "[1]"})).unwrap_err().contains("not a JSON object"));
    assert!(Mcp::tool_arguments(&json!({"arguments": "{oops"})).unwrap_err().contains("not JSON"));
    assert_eq!(Mcp::tool_arguments(&json!({"arguments": 3})).unwrap(), json!({}));
}

#[test]
fn images_beyond_the_attached_one_are_named_never_dumped() {
    let big = "A".repeat(4096);
    let content = vec![json!({"type": "text", "text": "hi"}), json!({"type": "image", "mimeType": "image/png", "data": big}), json!({"type": "image", "data": "BBBB"})];
    let (text, attached) = Mcp::render_parts(&content, true);
    assert_eq!(text, ["hi", "(image attached below)", "[image omitted: image/png (0 KB not shown)]"]);
    assert!(attached.unwrap().starts_with("data:image/png;base64,"));
    let (text, attached) = Mcp::render_parts(&content, false);
    assert_eq!(text, ["hi", "[image omitted: image/png (3 KB not shown)]", "[image omitted: image/png (0 KB not shown)]"]);
    assert!(attached.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn gateway_parses_string_arguments() {
    let mcp = mcp();
    let called = mcp.run(&json!({"server": "t", "tool": "echo", "arguments": "{\"text\": \"hi\"}"}), false).await;
    assert!(called.text.starts_with("echoed by "), "{}", called.text);
    let bad = mcp.run(&json!({"server": "t", "tool": "echo", "arguments": "{oops"}), false).await;
    assert!(bad.is_error && bad.text.contains("not JSON"), "{}", bad.text);
}

#[tokio::test(flavor = "current_thread")]
async fn gateway_lists_then_calls() {
    let mcp = mcp();
    let listed = mcp.run(&json!({"server": "t"}), false).await;
    assert!(listed.text.contains("## echo\nEcho text"), "{}", listed.text);
    let called = mcp.run(&json!({"server": "t", "tool": "echo", "arguments": {"text": "hi"}}), false).await;
    assert!(called.text.starts_with("echoed by "), "{}", called.text);
    assert!(mcp.run(&json!({"server": "nope"}), false).await.is_error);
}

/// A server that answers initialize and then nothing: every later request hangs.
    const HANGING: &str = r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done"#;

fn hanging() -> Mcp {
    let config = ServerConfig {
        command: Some("/bin/bash".into()),
        args: vec!["-c".into(), HANGING.into()],
        env: BTreeMap::new(),
        url: None,
        headers: BTreeMap::new(),
        bearer_token_env_var: None,
        enabled: None,
        tool_timeout_ms: Some(500),
        cwd: std::env::temp_dir(),
    };
    Mcp { servers: BTreeMap::from([("h".to_string(), config)]), ..Mcp::default() }
}

fn pending_of(connection: &Connection) -> Pending {
    match &connection.transport {
        Transport::Lines { pending, .. } => pending.clone(),
        Transport::Http { .. } => unreachable!("tests use stdio servers"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_timed_out_request_leaves_no_pending_entry() {
    let mcp = hanging();
    let connection = mcp.connection("h").await.unwrap();
    let pending = pending_of(&connection);
    let error = connection.request("tools/call", json!({"name": "x", "arguments": {}})).await.unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error:#}");
    assert!(pending.lock().expect("pending lock").is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_cancelled_request_leaves_no_pending_entry() {
    use std::future::Future as _;
    let mcp = hanging();
    let connection = mcp.connection("h").await.unwrap();
    let pending = pending_of(&connection);
    let mut request = Box::pin(connection.request("tools/call", json!({"name": "x", "arguments": {}})));
    futures_util::future::poll_fn(|cx| {
        let _ = request.as_mut().poll(cx);
        std::task::Poll::Ready(())
    })
    .await;
    assert!(!pending.lock().expect("pending lock").is_empty(), "the request registers before it is cancelled");
    drop(request);
    assert!(pending.lock().expect("pending lock").is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn eager_servers_start_together() {
    let server = |name: &str| {
        let config = ServerConfig {
            command: Some("/bin/bash".into()),
            args: vec!["-c".into(), SERVER.into()],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            bearer_token_env_var: None,
            enabled: None,
            tool_timeout_ms: Some(5_000),
            cwd: std::env::temp_dir(),
        };
        (name.to_string(), config)
    };
    let mut mcp = Mcp { servers: BTreeMap::from([server("t1"), server("t2")]), ..Mcp::default() };
    let (definitions, problems) = mcp.start_eager(&["t1".to_string(), "t2".to_string()]).await;
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(definitions.len(), 2);
    assert!(mcp.is_native("mcp__t1__echo") && mcp.is_native("mcp__t2__echo"));
    assert!(mcp.call_native("mcp__t2__echo", json!({}), false).await.text.starts_with("echoed by "));
}

#[tokio::test(flavor = "current_thread")]
async fn eager_servers_get_native_tools() {
    let mut mcp = mcp();
    let (definitions, problems) = mcp.start_eager(&["t".to_string()]).await;
    assert!(problems.is_empty());
    assert_eq!(definitions[0]["function"]["name"], "mcp__t__echo");
    assert!(mcp.is_native("mcp__t__echo"));
    assert!(mcp.call_native("mcp__t__echo", json!({}), false).await.text.starts_with("echoed by "));
}

#[tokio::test(flavor = "current_thread")]
async fn a_server_turned_off_refuses_calls_until_turned_on() {
    let tmp = crate::test_temp::TempDir::new("killi-mcp-off");
    let path = tmp.join("off.json");
    let mcp = Mcp { off_path: Some(path.clone()), ..mcp() };
    let first = echo(&mcp).await;
    assert!(first.starts_with("echoed by "), "{first}");
    assert!(!mcp.toggle("t").unwrap());
    assert!(mcp.live.borrow().is_empty());
    assert!(echo(&mcp).await.contains("turned off"));
    assert!(mcp.names().is_empty() && !mcp.list()[0].on);
    assert_eq!(read_off(&path), BTreeSet::from(["t".to_string()]));
    assert!(mcp.toggle("t").unwrap());
    assert!(echo(&mcp).await.starts_with("echoed by "));
    assert_eq!(mcp.take_notes().len(), 2);
    assert!(mcp.take_notes().is_empty());
    assert!(read_off(&path).is_empty());
    assert!(mcp.toggle("nope").is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn sessions_share_a_server_through_the_broker_and_reconnect_after_it_exits() {
    let tmp = crate::test_temp::TempDir::new("killi-broker");
    let socket = tmp.join("broker.sock");
    let serving = tokio::spawn({
        let socket = socket.clone();
        async move { broker::serve(&socket).await }
    });
    while tokio::net::UnixStream::connect(&socket).await.is_err() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let shared = || Mcp { broker: Some(socket.clone()), ..mcp() };
    let (first, second) = (shared(), shared());
    let answer = echo(&first).await;
    assert!(answer.starts_with("echoed by "), "{answer}");
    assert_eq!(echo(&second).await, answer);
    assert_ne!(echo(&mcp()).await, answer, "a session without the broker starts its own server");

    let pid: i32 = answer["echoed by ".len()..].parse().unwrap();
    // SAFETY: kill only signals the test server the broker started.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    while !first.live.borrow()["t"].closed.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let restarted = echo(&first).await;
    assert!(restarted.starts_with("echoed by ") && restarted != answer, "{restarted}");
    assert_eq!(echo(&second).await, restarted);
    serving.abort();
}
