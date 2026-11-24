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

/// A server whose one tool needs an argument: for what a model is told when it can't
/// say what the tool takes.
const NEEDS: &str = r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"greet","description":"Greet someone","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"loud":{"type":"boolean"}},"required":["name"]}}]}}\n' "$id" ;;
    *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"greeted"}]}}\n' "$id" ;;
  esac
done"#;

/// A server that rejects every call as invalid params, whatever arguments it was sent.
const STRICT: &str = r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"greet","description":"Greet someone","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"loud":{"type":"boolean"}},"required":["name"]}}]}}\n' "$id" ;;
    *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32602,"message":"Invalid arguments for tool greet: expected string, received undefined at name"}}\n' "$id" ;;
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
    Mcp {
        servers: BTreeMap::from([("t".to_string(), config)]),
        ..Mcp::default()
    }
}

async fn echo(mcp: &Mcp) -> String {
    mcp.run(
        &json!({"server": "t", "tool": "echo", "arguments": {}}),
        false,
    )
    .await
    .text
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
    let mut mcp = Mcp {
        servers: BTreeMap::from([("t".to_string(), config.clone())]),
        project: BTreeSet::from(["t".to_string()]),
        ..Mcp::default()
    };
    assert!(mcp.is_project("t") && !mcp.is_project("other"));
    let before = mcp.trust_input("t").unwrap();
    config
        .env
        .insert("NODE_OPTIONS".into(), "--require ./evil.js".into());
    mcp.servers.insert("t".to_string(), config.clone());
    assert_ne!(
        mcp.trust_input("t").unwrap(),
        before,
        "an env change re-asks"
    );
    let before = mcp.trust_input("t").unwrap();
    config.bearer_token_env_var = Some("OTHER_TOKEN".into());
    mcp.servers.insert("t".to_string(), config);
    assert_ne!(
        mcp.trust_input("t").unwrap(),
        before,
        "a credentials change re-asks"
    );
    assert_eq!(
        mcp.command_line("t").unwrap(),
        "/bin/serve --port 1",
        "the display is unchanged by either"
    );
}

#[test]
fn command_line_names_what_a_server_would_run() {
    let home = crate::config::home();
    let mcp = Mcp {
        servers: BTreeMap::from([
            (
                "local".to_string(),
                server(Some("/bin/serve"), &["--port", "1"], None),
            ),
            ("home".to_string(), server(Some("~/bin/serve"), &[], None)),
            (
                "remote".to_string(),
                server(None, &[], Some("https://mcp.example/rpc")),
            ),
        ]),
        ..Mcp::default()
    };
    assert_eq!(
        mcp.command_line("local").as_deref(),
        Some("/bin/serve --port 1")
    );
    let expanded = format!("{}/bin/serve", home.display());
    assert_eq!(mcp.command_line("home").as_deref(), Some(expanded.as_str()));
    assert_eq!(
        mcp.command_line("remote").as_deref(),
        Some("https://mcp.example/rpc")
    );
    assert!(mcp.command_line("missing").is_none());
}

#[test]
fn arguments_accept_an_object_or_a_string_holding_one() {
    assert_eq!(
        Mcp::sent_arguments(&json!({})).unwrap(),
        None,
        "missing is no arguments"
    );
    assert_eq!(
        Mcp::sent_arguments(&json!({"arguments": {"text": "hi"}})).unwrap(),
        Some(json!({"text": "hi"}))
    );
    assert_eq!(
        Mcp::sent_arguments(&json!({"arguments": "{\"text\": \"hi\"}"})).unwrap(),
        Some(json!({"text": "hi"}))
    );
    assert_eq!(
        Mcp::sent_arguments(&json!({"arguments": ""})).unwrap(),
        None,
        "an empty string is no arguments, not a parse failure"
    );
    assert_eq!(
        Mcp::sent_arguments(&json!({"arguments": 3})).unwrap(),
        None,
        "a value no tool takes is no arguments"
    );
    assert!(
        Mcp::sent_arguments(&json!({"arguments": "[1]"}))
            .unwrap_err()
            .contains("not a JSON object")
    );
    assert!(
        Mcp::sent_arguments(&json!({"arguments": "{oops"}))
            .unwrap_err()
            .contains("not JSON")
    );
}

/// Servers for the argument shape: `n` runs a tool that needs one, `s` rejects every
/// call as invalid params, and `t` runs a tool that needs none.
fn needs() -> Mcp {
    Mcp {
        servers: BTreeMap::from([
            (
                "n".to_string(),
                server(Some("/bin/bash"), &["-c", NEEDS], None),
            ),
            (
                "t".to_string(),
                server(Some("/bin/bash"), &["-c", SERVER], None),
            ),
        ]),
        ..Mcp::default()
    }
}

fn strict() -> Mcp {
    Mcp {
        servers: BTreeMap::from([(
            "s".to_string(),
            server(Some("/bin/bash"), &["-c", STRICT], None),
        )]),
        ..Mcp::default()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_call_without_arguments_names_what_the_tool_takes() {
    let mcp = needs();
    for sent in [
        json!({"server": "n", "tool": "greet"}),
        json!({"server": "n", "tool": "greet", "arguments": ""}),
    ] {
        let refused = mcp.run(&sent, false).await;
        assert!(refused.is_error, "{}", refused.text);
        assert!(
            refused
                .text
                .contains("name (string, required), loud (boolean)"),
            "{}",
            refused.text
        );
        assert!(
            refused.text.contains("{\"name\": \"<string>\"}"),
            "{}",
            refused.text
        );
    }
    let malformed = mcp
        .run(
            &json!({"server": "n", "tool": "greet", "arguments": "{oops"}),
            false,
        )
        .await;
    assert!(
        malformed.text.contains("not JSON") && malformed.text.contains("name (string"),
        "{}",
        malformed.text
    );
    // A well-formed call still runs, and a tool that takes nothing needs no arguments.
    assert_eq!(
        mcp.run(
            &json!({"server": "n", "tool": "greet", "arguments": {"name": "kil"}}),
            false
        )
        .await
        .text,
        "greeted"
    );
    assert!(
        mcp.run(&json!({"server": "t", "tool": "echo"}), false)
            .await
            .text
            .starts_with("echoed by ")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_server_that_rejects_the_arguments_is_told_what_the_tool_takes() {
    let mcp = strict();
    let refused = mcp
        .run(
            &json!({"server": "s", "tool": "greet", "arguments": {"loud": true}}),
            false,
        )
        .await;
    assert!(refused.is_error, "{}", refused.text);
    assert!(
        refused.text.contains("MCP error -32602"),
        "{}",
        refused.text
    );
    assert!(
        refused.text.contains("name (string, required)"),
        "{}",
        refused.text
    );
}

#[test]
fn images_beyond_the_attached_one_are_named_never_dumped() {
    let big = "A".repeat(4096);
    let content = vec![
        json!({"type": "text", "text": "hi"}),
        json!({"type": "image", "mimeType": "image/png", "data": big}),
        json!({"type": "image", "data": "BBBB"}),
    ];
    let (text, attached) = Mcp::render_parts(&content, true);
    assert_eq!(
        text,
        [
            "hi",
            "(image attached below)",
            "[image omitted: image/png (0 KB not shown)]"
        ]
    );
    assert!(attached.unwrap().starts_with("data:image/png;base64,"));
    let (text, attached) = Mcp::render_parts(&content, false);
    assert_eq!(
        text,
        [
            "hi",
            "[image omitted: image/png (3 KB not shown)]",
            "[image omitted: image/png (0 KB not shown)]"
        ]
    );
    assert!(attached.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn gateway_parses_string_arguments() {
    let mcp = mcp();
    let called = mcp
        .run(
            &json!({"server": "t", "tool": "echo", "arguments": "{\"text\": \"hi\"}"}),
            false,
        )
        .await;
    assert!(called.text.starts_with("echoed by "), "{}", called.text);
    let bad = mcp
        .run(
            &json!({"server": "t", "tool": "echo", "arguments": "{oops"}),
            false,
        )
        .await;
    assert!(
        bad.is_error && bad.text.contains("not JSON"),
        "{}",
        bad.text
    );
}

#[tokio::test(flavor = "current_thread")]
async fn gateway_lists_then_calls() {
    let mcp = mcp();
    let listed = mcp.run(&json!({"server": "t"}), false).await;
    assert!(
        listed.text.contains("## echo\nEcho text"),
        "{}",
        listed.text
    );
    let called = mcp
        .run(
            &json!({"server": "t", "tool": "echo", "arguments": {"text": "hi"}}),
            false,
        )
        .await;
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
    Mcp {
        servers: BTreeMap::from([("h".to_string(), config)]),
        ..Mcp::default()
    }
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
    let error = connection
        .request("tools/call", json!({"name": "x", "arguments": {}}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error:#}");
    assert!(pending.lock().expect("pending lock").is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_cancelled_request_leaves_no_pending_entry() {
    use std::future::Future as _;
    let mcp = hanging();
    let connection = mcp.connection("h").await.unwrap();
    let pending = pending_of(&connection);
    let mut request =
        Box::pin(connection.request("tools/call", json!({"name": "x", "arguments": {}})));
    futures_util::future::poll_fn(|cx| {
        let _ = request.as_mut().poll(cx);
        std::task::Poll::Ready(())
    })
    .await;
    assert!(
        !pending.lock().expect("pending lock").is_empty(),
        "the request registers before it is cancelled"
    );
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
    let mut mcp = Mcp {
        servers: BTreeMap::from([server("t1"), server("t2")]),
        ..Mcp::default()
    };
    let (definitions, problems) = mcp.start_eager(&["t1".to_string(), "t2".to_string()]).await;
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(definitions.len(), 2);
    assert!(mcp.is_native("mcp__t1__echo") && mcp.is_native("mcp__t2__echo"));
    assert!(
        mcp.call_native("mcp__t2__echo", json!({}), false)
            .await
            .text
            .starts_with("echoed by ")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn eager_servers_get_native_tools() {
    let mut mcp = mcp();
    let (definitions, problems) = mcp.start_eager(&["t".to_string()]).await;
    assert!(problems.is_empty());
    assert_eq!(definitions[0]["function"]["name"], "mcp__t__echo");
    assert!(mcp.is_native("mcp__t__echo"));
    assert!(
        mcp.call_native("mcp__t__echo", json!({}), false)
            .await
            .text
            .starts_with("echoed by ")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_server_turned_off_refuses_calls_until_turned_on() {
    let tmp = crate::test_temp::TempDir::new("killi-mcp-off");
    let path = tmp.join("off.json");
    let mcp = Mcp {
        off_path: Some(path.clone()),
        ..mcp()
    };
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
    let shared = || Mcp {
        broker: Some(socket.clone()),
        ..mcp()
    };
    let (first, second) = (shared(), shared());
    let answer = echo(&first).await;
    assert!(answer.starts_with("echoed by "), "{answer}");
    assert_eq!(echo(&second).await, answer);
    assert_ne!(
        echo(&mcp()).await,
        answer,
        "a session without the broker starts its own server"
    );

    let pid: i32 = answer["echoed by ".len()..].parse().unwrap();
    // SAFETY: kill only signals the test server the broker started.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    while !first.live.borrow()["t"].closed.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let restarted = echo(&first).await;
    assert!(
        restarted.starts_with("echoed by ") && restarted != answer,
        "{restarted}"
    );
    assert_eq!(echo(&second).await, restarted);
    serving.abort();
}

fn bash_server(name: &str, script: &str, timeout_ms: u64, cwd: &Path) -> Mcp {
    let config = ServerConfig {
        command: Some("/bin/bash".into()),
        args: vec!["-c".into(), script.into()],
        env: BTreeMap::new(),
        url: None,
        headers: BTreeMap::new(),
        bearer_token_env_var: None,
        enabled: None,
        tool_timeout_ms: Some(timeout_ms),
        cwd: cwd.to_path_buf(),
    };
    Mcp {
        servers: BTreeMap::from([(name.to_string(), config)]),
        ..Mcp::default()
    }
}

/// Answers initialize, then stops reading its input.
const DEAF: &str = r#"IFS= read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
exec sleep 300"#;

#[tokio::test(flavor = "current_thread")]
async fn a_timed_out_request_closes_the_connection_so_the_next_call_reconnects() {
    let mcp = hanging();
    let connection = mcp.connection("h").await.unwrap();
    assert!(
        connection
            .request("tools/call", json!({"name": "x", "arguments": {}}))
            .await
            .is_err()
    );
    assert!(connection.closed.load(Ordering::SeqCst));
    let again = mcp.connection("h").await.unwrap();
    assert!(!Rc::ptr_eq(&connection, &again));
}

#[tokio::test(flavor = "current_thread")]
async fn a_write_the_server_never_takes_times_out_and_closes_the_connection() {
    let mcp = bash_server("d", DEAF, 500, &std::env::temp_dir());
    let connection = mcp.connection("d").await.unwrap();
    let big = "x".repeat(4 * 1024 * 1024);
    let error = connection
        .request(
            "tools/call",
            json!({"name": "x", "arguments": {"text": big}}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error:#}");
    assert!(connection.closed.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "current_thread")]
async fn a_write_cancelled_partway_closes_the_connection() {
    use std::future::Future as _;
    let mcp = bash_server("d", DEAF, 60_000, &std::env::temp_dir());
    let connection = mcp.connection("d").await.unwrap();
    let big = "x".repeat(4 * 1024 * 1024);
    let mut request = Box::pin(connection.request(
        "tools/call",
        json!({"name": "x", "arguments": {"text": big}}),
    ));
    futures_util::future::poll_fn(|cx| {
        assert!(request.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert!(!connection.closed.load(Ordering::SeqCst));
    drop(request);
    assert!(
        connection.closed.load(Ordering::SeqCst),
        "half a line is on the stream"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn lines_that_arent_utf8_are_skipped_not_fatal() {
    let noisy = r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  printf 'noise \xff\xfe\n'
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echoed"}]}}\n' "$id" ;;
  esac
done"#;
    let mcp = bash_server("t", noisy, 5_000, &std::env::temp_dir());
    assert_eq!(echo(&mcp).await, "echoed");
    assert_eq!(echo(&mcp).await, "echoed");
}

#[tokio::test(flavor = "current_thread")]
async fn callers_that_arrive_together_share_one_connection() {
    let mcp = mcp();
    let (first, second) = tokio::join!(mcp.connection("t"), mcp.connection("t"));
    assert!(Rc::ptr_eq(&first.unwrap(), &second.unwrap()));
}

#[tokio::test(flavor = "current_thread")]
async fn a_server_turned_off_while_opening_isnt_kept() {
    use std::future::Future as _;
    let mcp = mcp();
    let mut opening = Box::pin(mcp.connection("t"));
    futures_util::future::poll_fn(|cx| {
        assert!(opening.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert!(!mcp.toggle("t").unwrap());
    assert!(opening.await.is_ok());
    assert!(mcp.live.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_directly_started_server_takes_its_whole_group_with_it() {
    let tmp = crate::test_temp::TempDir::new("killi-mcp-group");
    let parent = r#"sleep 300 & echo $! > pid
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done"#;
    let mcp = bash_server("p", parent, 5_000, &tmp);
    let connection = mcp.connection("p").await.unwrap();
    let pid: i32 = std::fs::read_to_string(tmp.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    drop(connection);
    assert!(!mcp.toggle("p").unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    // SAFETY: signal 0 only checks that the process exists.
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the grandchild outlived its server"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A broker that answers every launch line with `answer`.
fn fake_broker(socket: &Path, answer: &'static str) {
    crate::broker::ensure_socket_dir(socket).unwrap();
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Ok((stream, _)) = listener.accept().await {
            let (reader, mut writer) = stream.into_split();
            let _ = broker::read_line(&mut tokio::io::BufReader::new(reader)).await;
            let _ = writer.write_all(format!("{answer}\n").as_bytes()).await;
        }
    });
}

#[tokio::test(flavor = "current_thread")]
async fn a_broker_that_gave_up_on_the_server_is_reported_not_worked_around() {
    let tmp = crate::test_temp::TempDir::new("km");
    let socket = tmp.join("f").join("broker.sock");
    fake_broker(
        &socket,
        r#"{"error":"initialize timed out after 60s","final":true}"#,
    );
    let refused = Mcp {
        broker: Some(socket),
        ..mcp()
    };
    let text = echo(&refused).await;
    assert!(text.contains("initialize timed out"), "{text}");

    let socket = tmp.join("s").join("broker.sock");
    fake_broker(&socket, r#"{"error":"could not start x","final":false}"#);
    let fallback = Mcp {
        broker: Some(socket),
        ..mcp()
    };
    let text = echo(&fallback).await;
    assert!(
        text.starts_with("echoed by "),
        "a failed start falls back to a direct one: {text}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn after_a_timeout_the_next_attach_tells_the_broker_how_long_it_waited() {
    let tmp = crate::test_temp::TempDir::new("km");
    let socket = tmp.join("r").join("broker.sock");
    crate::broker::ensure_socket_dir(&socket).unwrap();
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let (seen, mut lines) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Ok((stream, _)) = listener.accept().await {
            let (reader, mut writer) = stream.into_split();
            let line = broker::read_line(&mut tokio::io::BufReader::new(reader)).await;
            let _ = seen.send(line.unwrap().unwrap());
            let _ = writer
                .write_all(b"{\"error\":\"no\",\"final\":false}\n")
                .await;
        }
    });
    let mcp = Mcp {
        broker: Some(socket),
        ..hanging()
    };
    let connection = mcp.connection("h").await.unwrap();
    let first: Value = serde_json::from_str(&lines.recv().await.unwrap()).unwrap();
    assert!(first.get("stuck_after_ms").is_none(), "{first}");
    assert!(
        connection
            .request("tools/call", json!({"name": "x", "arguments": {}}))
            .await
            .is_err()
    );
    mcp.connection("h").await.unwrap();
    let second: Value = serde_json::from_str(&lines.recv().await.unwrap()).unwrap();
    assert_eq!(second["stuck_after_ms"], 500, "{second}");
    assert_eq!(second["command"], "/bin/bash");
}

#[tokio::test(flavor = "current_thread")]
async fn an_eager_server_that_never_starts_is_reported_in_time() {
    let silent = "exec sleep 300";
    let mut mcp = bash_server("s", silent, 60_000, &std::env::temp_dir());
    let (definitions, problems) = mcp
        .start_eager_within(&["s".to_string()], Duration::from_millis(300))
        .await;
    assert!(definitions.is_empty());
    assert!(problems[0].contains("didn't start"), "{problems:?}");
}

/// An HTTP MCP server that hands out a new session at each initialize and answers 404 to any
/// other session; `current` set to `None` stands for the server restarting.
struct SessionServer {
    url: String,
    current: Arc<Mutex<Option<String>>>,
    initializes: Arc<std::sync::atomic::AtomicUsize>,
}

async fn session_server() -> SessionServer {
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let current: Arc<Mutex<Option<String>>> = Arc::default();
    let initializes = Arc::new(AtomicUsize::new(0));
    let (state, count) = (current.clone(), initializes.clone());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4_096];
            let (head, body) = loop {
                if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buffer.len() >= end + 4 + length {
                        break (head, buffer[end + 4..end + 4 + length].to_vec());
                    }
                }
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => break (String::new(), Vec::new()),
                    Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                }
            };
            let message: Value = serde_json::from_slice(&body).unwrap_or_default();
            let session = head
                .lines()
                .find_map(|line| line.strip_prefix("mcp-session-id:"))
                .map(|v| v.trim().to_string());
            let (status, extra, reply) = if message["method"] == "initialize" {
                let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                let id = format!("s{n}");
                *state.lock().unwrap() = Some(id.clone());
                let reply = json!({"jsonrpc": "2.0", "id": message["id"], "result": {}});
                (
                    "200 OK",
                    format!("mcp-session-id: {id}\r\n"),
                    reply.to_string(),
                )
            } else if session != *state.lock().unwrap() {
                ("404 Not Found", String::new(), "no such session".into())
            } else if message.get("id").is_none() {
                ("202 Accepted", String::new(), String::new())
            } else {
                let reply = json!({"jsonrpc": "2.0", "id": message["id"], "result": {"tools": []}});
                ("200 OK", String::new(), reply.to_string())
            };
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n{reply}",
                reply.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    SessionServer {
        url,
        current,
        initializes,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn an_http_session_the_server_forgot_is_started_again() {
    let stub = session_server().await;
    let config = server(None, &[], Some(&stub.url));
    let connection = Connection::open(&config, None, false).await.unwrap();
    connection.request("tools/list", json!({})).await.unwrap();
    *stub.current.lock().unwrap() = None;
    let tools = connection.request("tools/list", json!({})).await.unwrap();
    assert_eq!(tools["tools"], json!([]));
    assert_eq!(stub.initializes.load(Ordering::SeqCst), 2);
}
