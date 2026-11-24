use super::*;

use futures_util::StreamExt;
use tokio::io::{AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Child;

use crate::broker::{BrokerRefused, Launch};
use crate::tools::Tracked;

/// A server's error reply, kept as an error with its code so a caller can tell a reply that
/// rejected the arguments it was sent (invalid params) from a tool that failed to run.
#[derive(Debug)]
pub(super) struct RpcError {
    pub code: Option<i64>,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self.code {
            Some(code) => write!(f, "MCP error {code}: {}", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for RpcError {}

/// An HTTP server's 404 for a request that carried a session id: the server has forgotten
/// the session, so the client starts a new one.
#[derive(Debug)]
struct SessionExpired(String);

impl std::fmt::Display for SessionExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SessionExpired {}

/// A server started directly. Its whole group is killed when the connection goes, so the
/// real server behind a wrapper such as `npx` or `uvx` doesn't outlive it. The child is
/// never waited on here, so its pid can't be reused while this holds it.
pub(super) struct Direct {
    child: Child,
    _tracked: Option<Tracked>,
}

impl Drop for Direct {
    fn drop(&mut self) {
        if let Some(pgid) = self.child.id() {
            // SAFETY: killpg only signals the process group this server was started in.
            unsafe { libc::killpg(pgid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

/// Marks the connection closed if dropped before `done`: a line cut short by a timeout or a
/// cancel would garble every message after it.
struct Unfinished<'a>(Option<&'a AtomicBool>);

impl Unfinished<'_> {
    fn done(mut self) {
        self.0 = None;
    }
}

impl Drop for Unfinished<'_> {
    fn drop(&mut self) {
        if let Some(closed) = self.0 {
            closed.store(true, Ordering::SeqCst);
        }
    }
}

type Writer = tokio::sync::Mutex<Box<dyn AsyncWrite + Unpin + Send>>;

fn initialize_params() -> Value {
    json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "killi", "version": env!("CARGO_PKG_VERSION")}})
}

impl Connection {
    /// Connects to the server `config` names, through the broker when there is one. `stuck`
    /// says the last connection's request went unanswered past the timeout, so the broker
    /// can start the server afresh if it has said nothing since.
    pub(super) async fn open(
        config: &ServerConfig,
        broker: Option<&Path>,
        stuck: bool,
    ) -> Result<Self> {
        let timeout = config
            .tool_timeout_ms
            .map_or(DEFAULT_TIMEOUT, Duration::from_millis);
        let closed = Arc::new(AtomicBool::new(false));
        let transport = match (&config.command, &config.url) {
            (Some(command), _) => {
                let launch = Launch {
                    command: expand(command),
                    args: config.args.clone(),
                    env: config.env.clone(),
                    cwd: config.cwd.clone(),
                };
                let attached = match broker {
                    Some(socket) => {
                        match broker::attach(socket, &launch, true, stuck.then_some(timeout)).await
                        {
                            Ok(stream) => Some(stream),
                            // The broker ran the server and gave up waiting on it, or is still
                            // waiting; a direct start would only wait as long again.
                            Err(e) if e.is::<BrokerRefused>() => return Err(e),
                            // No usable broker, or one whose start failed where this session's
                            // own environment may succeed.
                            Err(_) => None,
                        }
                    }
                    None => None,
                };
                let (reader, writer, child): (
                    Box<dyn AsyncRead + Unpin + Send>,
                    Box<dyn AsyncWrite + Unpin + Send>,
                    _,
                ) = match attached {
                    Some(stream) => {
                        let (reader, writer) = stream.into_split();
                        (Box::new(reader), Box::new(writer), None)
                    }
                    None => {
                        let mut child = launch.spawn()?;
                        // Tracked while the server runs, so quitting on a signal ends its group too.
                        let tracked = child.id().map(crate::tools::track);
                        let stdout = child.stdout.take().expect("stdout is piped");
                        let stdin = child.stdin.take().expect("stdin is piped");
                        let direct = Direct {
                            child,
                            _tracked: tracked,
                        };
                        (Box::new(stdout), Box::new(stdin), Some(direct))
                    }
                };
                let pending: Pending = Arc::default();
                let (routes, hung_up) = (pending.clone(), closed.clone());
                tokio::spawn(async move {
                    let mut reader = BufReader::new(reader);
                    while let Ok(Some(line)) = broker::read_line(&mut reader).await {
                        let Ok(message) = serde_json::from_str::<Value>(&line) else {
                            continue;
                        };
                        if message.get("method").is_some() {
                            continue;
                        }
                        if let Some(id) = message["id"].as_u64()
                            && let Some(reply) = routes.lock().expect("pending lock").remove(&id)
                        {
                            let _ = reply.send(message);
                        }
                    }
                    hung_up.store(true, Ordering::SeqCst);
                    routes.lock().expect("pending lock").clear();
                });
                Transport::Lines {
                    writer: tokio::sync::Mutex::new(writer),
                    pending,
                    _child: child,
                }
            }
            (None, Some(url)) => {
                let mut headers: Vec<(String, String)> = config
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if let Some(var) = &config.bearer_token_env_var {
                    let token = std::env::var(var).with_context(|| format!("${var} isn't set"))?;
                    headers.push(("authorization".into(), format!("Bearer {token}")));
                }
                let client = reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(30))
                    .build()?;
                Transport::Http {
                    client,
                    url: url.clone(),
                    headers,
                    session: RefCell::new(None),
                }
            }
            (None, None) => bail!("the server config needs `command` or `url`"),
        };
        let connection = Self {
            transport,
            next_id: Cell::new(1),
            timeout,
            closed,
            timed_out: Cell::new(false),
        };
        connection
            .request("initialize", initialize_params())
            .await?;
        connection.notify("notifications/initialized").await?;
        Ok(connection)
    }

    pub(super) async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.take_id();
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let reply = match &self.transport {
            Transport::Lines {
                writer, pending, ..
            } => {
                let (tx, rx) = oneshot::channel();
                pending.lock().expect("pending lock").insert(id, tx);
                let _slot = PendingSlot {
                    pending: pending.clone(),
                    id,
                };
                let exchange = async {
                    self.send(writer, &message).await?;
                    rx.await.map_err(|_| anyhow!("the server exited"))
                };
                match tokio::time::timeout(self.timeout, exchange).await {
                    Ok(reply) => reply?,
                    // A server that doesn't answer in time may never answer: the next call
                    // reconnects and tells the broker, which restarts a server that has said
                    // nothing since.
                    Err(_) => {
                        self.closed.store(true, Ordering::SeqCst);
                        self.timed_out.set(true);
                        bail!("{method} timed out after {}s", self.timeout.as_secs())
                    }
                }
            }
            Transport::Http { .. } => match self.post(&message, Some(id)).await {
                Err(e) if e.is::<SessionExpired>() && method != "initialize" => {
                    self.reinitialize().await?;
                    self.post(&message, Some(id)).await?
                }
                reply => reply?,
            },
        };
        if let Some(error) = reply.get("error") {
            return Err(RpcError {
                code: error["code"].as_i64(),
                message: error["message"]
                    .as_str()
                    .unwrap_or(&error.to_string())
                    .to_string(),
            }
            .into());
        }
        Ok(reply["result"].clone())
    }

    fn take_id(&self) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        id
    }

    /// Writes one message as a line, marking the connection closed if the write is cut short.
    async fn send(&self, writer: &Writer, message: &Value) -> Result<()> {
        let mut writer = writer.lock().await;
        let unfinished = Unfinished(Some(&self.closed));
        writer
            .write_all(format!("{message}\n").as_bytes())
            .await
            .context("the server closed its input")?;
        unfinished.done();
        Ok(())
    }

    async fn notify(&self, method: &str) -> Result<()> {
        let message = json!({"jsonrpc": "2.0", "method": method});
        match &self.transport {
            Transport::Lines { writer, .. } => {
                tokio::time::timeout(self.timeout, self.send(writer, &message))
                    .await
                    .map_err(|_| {
                        anyhow!("{method} timed out after {}s", self.timeout.as_secs())
                    })??;
            }
            Transport::Http { .. } => {
                self.post(&message, None).await?;
            }
        }
        Ok(())
    }

    /// Starts a new HTTP session after the server forgot the old one.
    async fn reinitialize(&self) -> Result<()> {
        let id = self.take_id();
        let message = json!({"jsonrpc": "2.0", "id": id, "method": "initialize", "params": initialize_params()});
        let reply = self.post(&message, Some(id)).await?;
        if let Some(error) = reply.get("error") {
            bail!("initialize failed: {error}");
        }
        self.post(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            None,
        )
        .await?;
        Ok(())
    }

    /// Streamable HTTP: the reply comes back as JSON or as server-sent events.
    async fn post(&self, message: &Value, id: Option<u64>) -> Result<Value> {
        let Transport::Http {
            client,
            url,
            headers,
            session,
        } = &self.transport
        else {
            unreachable!()
        };
        let mut request = client
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL_VERSION)
            .timeout(self.timeout)
            .body(message.to_string());
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let sent_session = session.borrow().clone();
        if let Some(id) = &sent_session {
            request = request.header("mcp-session-id", id);
        }
        let response = request.send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND && sent_session.is_some() {
            if *session.borrow() == sent_session {
                *session.borrow_mut() = None;
            }
            return Err(SessionExpired(format!(
                "HTTP 404 for the session: {}",
                crate::llm::truncate(&response.text().await.unwrap_or_default(), 500)
            ))
            .into());
        }
        if let Some(id) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *session.borrow_mut() = Some(id.to_string());
        }
        let status = response.status();
        if !status.is_success() {
            bail!(
                "HTTP {status}: {}",
                crate::llm::truncate(&response.text().await.unwrap_or_default(), 500)
            );
        }
        let Some(id) = id else { return Ok(Value::Null) };
        let streamed = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.contains("event-stream"));
        if !streamed {
            return Ok(serde_json::from_slice(&response.bytes().await?)?);
        }
        let mut stream = response.bytes_stream();
        let mut lines = crate::llm::SseLines::default();
        while let Some(chunk) = stream.next().await {
            lines.push(&chunk?);
            while let Some(line) = lines.next_line() {
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                if let Ok(reply) = serde_json::from_str::<Value>(data.trim())
                    && reply["id"].as_u64() == Some(id)
                {
                    return Ok(reply);
                }
            }
        }
        bail!("the server closed the stream without replying")
    }
}
