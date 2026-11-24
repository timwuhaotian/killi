use super::*;

#[test]
fn the_brokers_socket_lives_in_a_private_versioned_folder() {
    let socket = socket_path();
    assert_eq!(socket.file_name().unwrap(), "broker.sock");
    let dir = socket
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy();
    assert!(dir.contains(env!("CARGO_PKG_VERSION")), "{dir}");
}

#[test]
fn a_loose_existing_socket_folder_is_tightened() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("killi-broker-loose-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    ensure_socket_dir(&dir.join("broker.sock")).unwrap();
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_socket_folder_is_created_for_this_user_alone() {
    use std::os::unix::fs::PermissionsExt;
    let socket = std::env::temp_dir()
        .join(format!("killi-broker-test-{}", std::process::id()))
        .join("broker.sock");
    ensure_socket_dir(&socket).unwrap();
    let mode = std::fs::metadata(socket.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    std::fs::remove_dir_all(socket.parent().unwrap()).unwrap();
}

/// A stdio server that answers the broker's initialize handshake and nothing else.
const SERVER: &str = r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done"#;

#[tokio::test(flavor = "current_thread")]
async fn a_running_server_is_reused() {
    let dir = std::env::temp_dir().join(format!("killi-broker-reuse-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let launch = Launch {
        command: "/bin/bash".into(),
        args: vec!["-c".into(), SERVER.into()],
        env: Default::default(),
        cwd: dir.clone(),
    };
    let broker = Broker::default();
    let first = server(&broker, &launch, None).await.unwrap();
    let second = server(&broker, &launch, None).await.unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(broker.servers.lock().await.len(), 1);
    first.kill();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn only_this_users_processes_pass_the_peer_check() {
    let dir = std::env::temp_dir().join(format!("killi-broker-peer-{}", std::process::id()));
    ensure_socket_dir(&dir.join("broker.sock")).unwrap();
    let socket = dir.join("test.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let accepted = tokio::spawn(async move { listener.accept().await.map(|(stream, _)| stream) });
    let _connecting = UnixStream::connect(&socket).await.unwrap();
    assert!(own_user(&accepted.await.unwrap().unwrap()));
    std::fs::remove_dir_all(&dir).unwrap();
}

fn bash(script: &str, cwd: &Path) -> Launch {
    Launch {
        command: "/bin/bash".into(),
        args: vec!["-c".into(), script.into()],
        env: Default::default(),
        cwd: cwd.to_path_buf(),
    }
}

fn short_limits() -> Limits {
    Limits {
        init: Duration::from_secs(10),
        write: Duration::from_millis(300),
        stale: Duration::from_millis(200),
    }
}

/// Waits up to five seconds for `pid` to be gone.
async fn gone(pid: i32) -> bool {
    for _ in 0..250 {
        // SAFETY: signal 0 only checks that the process exists.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// The pid a test server wrote to `file` for the grandchild it left running.
async fn pid_in(file: &Path) -> i32 {
    for _ in 0..250 {
        if let Some(pid) = std::fs::read_to_string(file)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no pid in {}", file.display());
}

/// Answers initialize, then leaves a grandchild running and keeps serving.
const PARENT: &str = r#"sleep 300 & echo $! > pid
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done"#;

#[test]
fn lines_are_capped_and_decoded_leniently() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let input: &[u8] = b"{\"a\":1}\r\n\xff\xfe\n0123456789abcdef\nshort\nlast";
        let mut reader = BufReader::new(input);
        let mut lines = Vec::new();
        while let Some(line) = line::read_capped(&mut reader, 10).await.unwrap() {
            lines.push(line);
        }
        assert_eq!(
            lines,
            ["{\"a\":1}", "\u{fffd}\u{fffd}", "", "short", "last"],
            "a line over the cap comes back empty"
        );
    });
}

#[tokio::test(flavor = "current_thread")]
async fn a_server_that_stops_reading_is_killed_and_the_next_attach_starts_another() {
    let tmp = crate::test_temp::TempDir::new("killi-broker-stuck");
    let stuck = r#"IFS= read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
exec sleep 300"#;
    let launch = bash(stuck, &tmp);
    let broker = Broker {
        limits: short_limits(),
        ..Broker::default()
    };
    let first = server(&broker, &launch, None).await.unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let big = "x".repeat(4 * 1024 * 1024);
    let error = first
        .forward(
            json!({"jsonrpc": "2.0", "method": "tools/call", "params": {"text": big}}),
            Route::Client(tx, json!(1)),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("stopped reading"), "{error:#}");
    assert!(*first.exited.borrow());
    let second = server(&broker, &launch, None).await.unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    second.kill();
}

#[tokio::test(flavor = "current_thread")]
async fn a_request_given_up_on_and_never_answered_gets_the_server_swept() {
    let tmp = crate::test_temp::TempDir::new("killi-broker-sweep");
    let broker = Broker {
        limits: short_limits(),
        ..Broker::default()
    };
    let launch = bash(SERVER, &tmp);
    let server = server(&broker, &launch, None).await.unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    server
        .forward(
            json!({"jsonrpc": "2.0", "method": "tools/call"}),
            Route::Client(tx, json!(1)),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    server::sweep(&mut *broker.servers.lock().await);
    assert!(
        !*server.exited.borrow(),
        "a slow server is left alone while its client still waits"
    );
    assert_eq!(broker.servers.lock().await.len(), 1);
    drop(rx);
    server::sweep(&mut *broker.servers.lock().await);
    assert!(*server.exited.borrow());
    assert!(broker.servers.lock().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_dropped_server_takes_its_whole_group_with_it() {
    let tmp = crate::test_temp::TempDir::new("killi-broker-group");
    let server = Server::start(&bash(PARENT, &tmp), Limits::default())
        .await
        .unwrap();
    let grandchild = pid_in(&tmp.join("pid")).await;
    drop(server);
    assert!(gone(grandchild).await, "the grandchild outlived its server");
}

#[tokio::test(flavor = "current_thread")]
async fn a_leader_that_exits_ends_the_server_even_while_its_output_stays_open() {
    let tmp = crate::test_temp::TempDir::new("killi-broker-leader");
    let leaves = r#"IFS= read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
IFS= read -r line
sleep 300 & echo $! > pid
exit 0"#;
    let server = Server::start(&bash(leaves, &tmp), Limits::default())
        .await
        .unwrap();
    let grandchild = pid_in(&tmp.join("pid")).await;
    let mut exited = server.exited.clone();
    tokio::time::timeout(Duration::from_secs(5), exited.wait_for(|gone| *gone))
        .await
        .expect("the exit is seen")
        .unwrap();
    assert!(
        gone(grandchild).await,
        "what the leader left behind is killed"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn attach_refuses_a_folder_or_socket_that_isnt_this_users_own() {
    let tmp = crate::test_temp::TempDir::new("kb");
    let launch = bash(SERVER, &tmp);
    let real = tmp.join("r");
    ensure_socket_dir(&real.join("broker.sock")).unwrap();
    let _listener = UnixListener::bind(real.join("broker.sock")).unwrap();
    let link = tmp.join("l");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let error = attach(&link.join("broker.sock"), &launch, false, None)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("not this user's own folder"),
        "{error:#}"
    );

    let plain = tmp.join("p");
    ensure_socket_dir(&plain.join("broker.sock")).unwrap();
    std::fs::write(plain.join("broker.sock"), "").unwrap();
    let error = attach(&plain.join("broker.sock"), &launch, false, None)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("not this user's broker socket"),
        "{error:#}"
    );
}

/// A broker that answers every launch line with `answer`.
fn fake_broker(socket: &Path, answer: &'static str) {
    ensure_socket_dir(socket).unwrap();
    let listener = UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (reader, mut writer) = stream.into_split();
            let _ = read_line(&mut BufReader::new(reader)).await;
            let _ = writer.write_all(format!("{answer}\n").as_bytes()).await;
        }
    });
}

#[tokio::test(flavor = "current_thread")]
async fn only_a_broker_that_gave_up_on_a_started_server_refuses() {
    let tmp = crate::test_temp::TempDir::new("kb");
    let launch = bash(SERVER, &tmp);
    let socket = tmp.join("f").join("broker.sock");
    fake_broker(
        &socket,
        r#"{"error":"initialize timed out after 60s","final":true}"#,
    );
    let error = attach(&socket, &launch, false, None).await.unwrap_err();
    assert!(error.is::<BrokerRefused>(), "{error:#}");

    let socket = tmp.join("s").join("broker.sock");
    fake_broker(&socket, r#"{"error":"could not start npx","final":false}"#);
    let error = attach(&socket, &launch, false, None).await.unwrap_err();
    assert!(!error.is::<BrokerRefused>(), "{error:#}");
    assert!(
        error.to_string().contains("could not start npx"),
        "{error:#}"
    );
}

#[test]
fn a_long_line_is_dropped_across_reads_and_a_line_at_the_cap_is_kept() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let input: &[u8] = b"ok\n0123456789abcdefghij\n0123456789\nend";
        let mut reader = BufReader::with_capacity(4, input);
        let mut lines = Vec::new();
        while let Some(line) = line::read_capped(&mut reader, 10).await.unwrap() {
            lines.push(line);
        }
        assert_eq!(lines, ["ok", "", "0123456789", "end"]);
    });
}

/// Answers initialize, then takes in its input a little at a time.
const SLOW: &str = r#"IFS= read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
while :; do dd bs=16384 count=1 of=/dev/null 2>/dev/null; sleep 0.05; done"#;

#[tokio::test(flavor = "current_thread")]
async fn a_server_that_takes_a_long_line_slowly_is_left_alone() {
    let tmp = crate::test_temp::TempDir::new("killi-broker-slow");
    let limits = Limits {
        write: Duration::from_secs(1),
        ..short_limits()
    };
    let server = Server::start(&bash(SLOW, &tmp), limits).await.unwrap();
    let big = "x".repeat(256 * 1024);
    let request = || {
        let (tx, rx) = mpsc::unbounded_channel();
        let sent = server.forward(
            json!({"jsonrpc": "2.0", "method": "tools/call", "params": {"text": big}}),
            Route::Client(tx, json!(1)),
        );
        (sent, rx)
    };
    let ((first, _a), (second, _b)) = (request(), request());
    let started = Instant::now();
    let (first, second) = tokio::join!(first, second);
    first.unwrap();
    second.unwrap();
    assert!(
        started.elapsed() > limits.write,
        "the writes took {:?}, too fast to show anything",
        started.elapsed()
    );
    assert!(!*server.exited.borrow());
    server.kill();
}

/// A broker on `socket` with `limits`, serving clients as `serve` does.
fn test_broker(socket: &Path, limits: Limits) -> Arc<Broker> {
    ensure_socket_dir(socket).unwrap();
    let listener = UnixListener::bind(socket).unwrap();
    let broker = Arc::new(Broker {
        limits,
        ..Broker::default()
    });
    tokio::spawn({
        let broker = broker.clone();
        async move {
            while let Ok((stream, _)) = listener.accept().await {
                let broker = broker.clone();
                tokio::spawn(async move { client(&broker, stream).await });
            }
        }
    });
    broker
}

/// Every line `stream` gets until the broker lets it go.
async fn lines_until_closed<R: tokio::io::AsyncRead + Unpin>(stream: R) -> Vec<Value> {
    let mut reader = BufReader::new(stream);
    let mut lines = Vec::new();
    loop {
        let line = tokio::time::timeout(Duration::from_secs(5), read_line(&mut reader))
            .await
            .expect("the broker lets the client go")
            .unwrap();
        let Some(line) = line else { return lines };
        lines.push(serde_json::from_str(&line).unwrap());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_server_stopped_for_not_reading_answers_every_waiting_client_once() {
    let tmp = crate::test_temp::TempDir::new("kb");
    let socket = tmp.join("b").join("broker.sock");
    let broker = test_broker(&socket, short_limits());
    let stuck = r#"IFS= read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
exec sleep 300"#;
    let launch = bash(stuck, &tmp);
    let mut other = attach(&socket, &launch, false, None).await.unwrap();
    other
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\"}\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let first = broker.servers.lock().await[&launch].clone();
    let mut writer = attach(&socket, &launch, false, None).await.unwrap();
    let big = "x".repeat(4 * 1024 * 1024);
    let line = format!(
        "{}\n",
        json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"text": big}})
    );
    let (reader, mut half) = writer.split();
    let (_, mine) = tokio::join!(half.write_all(line.as_bytes()), lines_until_closed(reader));
    let theirs = lines_until_closed(other).await;
    for (lines, id) in [(&theirs, 7), (&mine, 9)] {
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["id"], id, "{lines:?}");
        let message = lines[0]["error"]["message"].as_str().unwrap();
        assert!(message.contains("stopped reading"), "{message}");
    }
    let _again = attach(&socket, &launch, false, None).await.unwrap();
    let second = broker.servers.lock().await[&launch].clone();
    assert!(!Arc::ptr_eq(&first, &second));
    second.kill();
}

#[tokio::test(flavor = "current_thread")]
async fn a_client_whose_request_timed_out_gets_a_fresh_server_only_from_a_silent_one() {
    let tmp = crate::test_temp::TempDir::new("killi-broker-stuck-hint");
    let broker = Broker {
        limits: short_limits(),
        ..Broker::default()
    };
    let launch = bash(SERVER, &tmp);
    let first = server(&broker, &launch, None).await.unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let call = json!({"jsonrpc": "2.0", "method": "tools/call"});
    first
        .forward(call.clone(), Route::Client(tx.clone(), json!(1)))
        .await
        .unwrap();
    let hint = Some(Duration::from_millis(100));
    let same = server(&broker, &launch, hint).await.unwrap();
    assert!(
        Arc::ptr_eq(&first, &same),
        "a request younger than the write limit doesn't count"
    );

    tokio::time::sleep(Duration::from_millis(400)).await;
    let (answered, reply) = oneshot::channel();
    first
        .forward(
            json!({"jsonrpc": "2.0", "method": "initialize"}),
            Route::Broker(answered),
        )
        .await
        .unwrap();
    reply.await.unwrap();
    let same = server(&broker, &launch, hint).await.unwrap();
    assert!(
        Arc::ptr_eq(&first, &same),
        "a server that answered anyone since isn't stuck"
    );

    first
        .forward(call, Route::Client(tx, json!(2)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let fresh = server(&broker, &launch, hint).await.unwrap();
    assert!(!Arc::ptr_eq(&first, &fresh));
    assert!(*first.exited.borrow());
    let failed: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok())
        .map(|line| serde_json::from_str(&line).unwrap())
        .collect();
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert!(
        failed[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("answered nothing"),
        "{failed:?}"
    );
    let unhinted = server(&broker, &launch, None).await.unwrap();
    assert!(Arc::ptr_eq(&fresh, &unhinted));
    fresh.kill();
}

#[tokio::test(flavor = "current_thread")]
async fn an_attach_behind_an_initialize_that_timed_out_gets_the_same_answer_at_once() {
    let tmp = crate::test_temp::TempDir::new("killi-broker-init");
    let broker = Broker {
        limits: Limits {
            init: Duration::from_secs(1),
            ..short_limits()
        },
        ..Broker::default()
    };
    let launch = bash("exec sleep 300", &tmp);
    let started = Instant::now();
    let (first, second) = tokio::join!(server(&broker, &launch, None), async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        server(&broker, &launch, None).await
    });
    for result in [first, second] {
        let error = result.err().expect("initialize times out");
        assert!(error.is::<InitTimedOut>(), "{error:#}");
    }
    assert!(
        started.elapsed() < Duration::from_millis(1800),
        "the second attach waited on a start of its own: {:?}",
        started.elapsed()
    );
    let again = Instant::now();
    let error = server(&broker, &launch, None).await.err().unwrap();
    assert!(error.is::<InitTimedOut>(), "{error:#}");
    assert!(again.elapsed() < Duration::from_millis(500));
}
