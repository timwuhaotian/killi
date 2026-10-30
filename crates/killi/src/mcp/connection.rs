use super::*;

use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};

use crate::broker::Launch;

impl Connection {
    pub(super) async fn open(config: &ServerConfig, broker: Option<&Path>) -> Result<Self> {
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
                    Some(socket) => broker::attach(socket, &launch, true).await.ok(),
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
                        (Box::new(stdout), Box::new(stdin), Some((child, tracked)))
                    }
                };
                let pending: Pending = Arc::default();
                let (routes, hung_up) = (pending.clone(), closed.clone());
                tokio::spawn(async move {
                    let mut lines = BufReader::new(reader).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
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
        };
        connection
            .request(
                "initialize",
                json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "killi", "version": env!("CARGO_PKG_VERSION")}}),
            )
            .await?;
        connection.notify("notifications/initialized").await?;
        Ok(connection)
    }

    pub(super) async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
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
                let mut line = message.to_string();
                line.push('\n');
                writer
                    .lock()
                    .await
                    .write_all(line.as_bytes())
                    .await
                    .context("the server closed its input")?;
                tokio::time::timeout(self.timeout, rx)
                    .await
                    .map_err(|_| anyhow!("{method} timed out after {}s", self.timeout.as_secs()))?
                    .map_err(|_| anyhow!("the server exited"))?
            }
            Transport::Http { .. } => self.post(&message, Some(id)).await?,
        };
        if let Some(error) = reply.get("error") {
            bail!(
                "{}",
                error["message"].as_str().unwrap_or(&error.to_string())
            );
        }
        Ok(reply["result"].clone())
    }

    async fn notify(&self, method: &str) -> Result<()> {
        let message = json!({"jsonrpc": "2.0", "method": method});
        match &self.transport {
            Transport::Lines { writer, .. } => {
                writer
                    .lock()
                    .await
                    .write_all(format!("{message}\n").as_bytes())
                    .await?;
            }
            Transport::Http { .. } => {
                self.post(&message, None).await?;
            }
        }
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
        if let Some(id) = session.borrow().as_deref() {
            request = request.header("mcp-session-id", id);
        }
        let response = request.send().await?;
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
