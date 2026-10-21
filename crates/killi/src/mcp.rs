//! MCP servers, reached through one `mcp` tool so the tool list never changes: called with a
//! server name it lists that server's tools, and with a tool name it calls it. No server
//! starts until it is first used. Servers a project names in `[mcp] eager` start with the
//! session instead and get native tool definitions, fixed for the session.
//!
//! Servers come from `~/.killi/mcp.json` and the project's `.mcp.json`, in the
//! `{"mcpServers": {...}}` format Claude Code and Kimi Code use.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Stdio;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

use crate::config::{home, killi_home};
use crate::tools::Output;

const PROTOCOL_VERSION: &str = "2025-06-18";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    bearer_token_env_var: Option<String>,
    enabled: Option<bool>,
    tool_timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
struct File {
    #[serde(rename = "mcpServers", default)]
    servers: BTreeMap<String, ServerConfig>,
}

#[derive(Default)]
pub struct Mcp {
    servers: BTreeMap<String, ServerConfig>,
    live: RefCell<HashMap<String, Rc<Connection>>>,
    listed: RefCell<HashMap<String, Vec<Value>>>,
    /// Native tool names of eager servers' tools, to their server and tool.
    native: HashMap<String, (String, String)>,
}

impl Mcp {
    pub fn load(cwd: &Path) -> Self {
        let root = cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(cwd);
        let mut servers = BTreeMap::new();
        for path in [killi_home().join("mcp.json"), root.join(".mcp.json")] {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            if let Ok(file) = serde_json::from_str::<File>(&text) {
                servers.extend(file.servers.into_iter().filter(|(_, s)| s.enabled != Some(false)));
            }
        }
        Self { servers, ..Self::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    pub fn names(&self) -> Vec<&str> {
        self.servers.keys().map(String::as_str).collect()
    }

    /// The `mcp` gateway tool, naming the servers it can reach.
    pub fn definition(&self) -> Option<Value> {
        if self.servers.is_empty() {
            return None;
        }
        let names = self.names().join(", ");
        Some(json!({"type": "function", "function": {
            "name": "mcp",
            "description": format!("Use tools on MCP servers: {names}. Call with only `server` to list its tools and their arguments, then with `tool` and `arguments` to run one."),
            "parameters": {"type": "object", "properties": {
                "server": {"type": "string"},
                "tool": {"type": "string"},
                "arguments": {"type": "object"}
            }, "required": ["server"]}
        }}))
    }

    /// Starts the named servers now and returns native definitions of their tools.
    pub async fn start_eager(&mut self, names: &[String]) -> (Vec<Value>, Vec<String>) {
        let mut definitions = Vec::new();
        let mut problems = Vec::new();
        for name in names {
            match self.tools(name).await {
                Ok(tools) => {
                    for tool in tools {
                        let tool_name = tool["name"].as_str().unwrap_or_default();
                        let native = native_name(name, tool_name);
                        self.native.insert(native.clone(), (name.clone(), tool_name.to_string()));
                        definitions.push(json!({"type": "function", "function": {
                            "name": native,
                            "description": tool["description"].as_str().unwrap_or_default(),
                            "parameters": tool.get("inputSchema").cloned().unwrap_or_else(|| json!({"type": "object"})),
                        }}));
                    }
                }
                Err(e) => problems.push(format!("MCP server {name}: {e:#}")),
            }
        }
        (definitions, problems)
    }

    pub fn is_native(&self, name: &str) -> bool {
        self.native.contains_key(name)
    }

    pub async fn call_native(&self, name: &str, arguments: Value, images: bool) -> Output {
        let Some((server, tool)) = self.native.get(name) else {
            return Output::error(format!("Unknown tool `{name}`."));
        };
        self.call(server, tool, arguments, images).await
    }

    /// The `mcp` tool.
    pub async fn run(&self, args: &Value, images: bool) -> Output {
        let Some(server) = args["server"].as_str() else {
            return Output::error("mcp needs `server`.");
        };
        if !self.servers.contains_key(server) {
            return Output::error(format!("No MCP server `{server}`. Servers: {}.", self.names().join(", ")));
        }
        match args["tool"].as_str().filter(|t| !t.is_empty()) {
            None => match self.tools(server).await {
                Ok(tools) => {
                    let mut text = format!("Tools on {server}:\n");
                    for tool in &tools {
                        text.push_str(&format!(
                            "\n## {}\n{}\nArguments: {}\n",
                            tool["name"].as_str().unwrap_or_default(),
                            tool["description"].as_str().unwrap_or_default().trim(),
                            tool.get("inputSchema").map(Value::to_string).unwrap_or_else(|| "{}".into()),
                        ));
                    }
                    Output::ok(text).brief(format!("{} tools", tools.len()))
                }
                Err(e) => Output::error(format!("Could not reach MCP server {server}: {e:#}")),
            },
            Some(tool) => {
                let arguments = args.get("arguments").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
                self.call(server, tool, arguments, images).await
            }
        }
    }

    async fn tools(&self, server: &str) -> Result<Vec<Value>> {
        if let Some(tools) = self.listed.borrow().get(server) {
            return Ok(tools.clone());
        }
        let connection = self.connection(server).await?;
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let result = connection.request("tools/list", params).await?;
            tools.extend(result["tools"].as_array().cloned().unwrap_or_default());
            match result["nextCursor"].as_str() {
                Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
                _ => break,
            }
        }
        self.listed.borrow_mut().insert(server.to_string(), tools.clone());
        Ok(tools)
    }

    async fn call(&self, server: &str, tool: &str, arguments: Value, images: bool) -> Output {
        let connection = match self.connection(server).await {
            Ok(connection) => connection,
            Err(e) => return Output::error(format!("Could not reach MCP server {server}: {e:#}")),
        };
        let result = match connection.request("tools/call", json!({"name": tool, "arguments": arguments})).await {
            Ok(result) => result,
            Err(e) => return Output::error(format!("{server}/{tool} failed: {e:#}")),
        };
        let mut text = Vec::new();
        let mut image = None;
        for part in result["content"].as_array().into_iter().flatten() {
            match part["type"].as_str() {
                Some("text") => text.push(part["text"].as_str().unwrap_or_default().to_string()),
                Some("image") if images && image.is_none() => {
                    image = Some(format!("data:{};base64,{}", part["mimeType"].as_str().unwrap_or("image/png"), part["data"].as_str().unwrap_or_default()));
                    text.push("(image attached below)".into());
                }
                Some("resource") => text.push(part["resource"]["text"].as_str().map_or_else(|| part["resource"].to_string(), String::from)),
                _ => text.push(part.to_string()),
            }
        }
        if text.is_empty()
            && let Some(structured) = result.get("structuredContent")
        {
            text.push(structured.to_string());
        }
        let body = if text.is_empty() { "(no content)".to_string() } else { text.join("\n") };
        let mut output = if result["isError"] == true { Output::error(body) } else { Output::ok(body).brief(format!("{server}/{tool}")) };
        output.image = image;
        output
    }

    async fn connection(&self, server: &str) -> Result<Rc<Connection>> {
        if let Some(connection) = self.live.borrow().get(server) {
            return Ok(connection.clone());
        }
        let config = self.servers.get(server).ok_or_else(|| anyhow!("no MCP server `{server}`"))?;
        let connection = Rc::new(Connection::open(config).await?);
        self.live.borrow_mut().insert(server.to_string(), connection.clone());
        Ok(connection)
    }
}

/// `mcp__server__tool`, limited to the characters and length tool names allow.
fn native_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .take(64)
        .collect()
}

struct Connection {
    transport: Transport,
    next_id: Cell<u64>,
    timeout: Duration,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

enum Transport {
    Stdio { stdin: tokio::sync::Mutex<ChildStdin>, pending: Pending, _child: Child },
    Http { client: reqwest::Client, url: String, headers: Vec<(String, String)>, session: RefCell<Option<String>> },
}

impl Connection {
    async fn open(config: &ServerConfig) -> Result<Self> {
        let timeout = config.tool_timeout_ms.map_or(DEFAULT_TIMEOUT, Duration::from_millis);
        let transport = match (&config.command, &config.url) {
            (Some(command), _) => {
                let command = expand(command);
                let mut child = Command::new(&command)
                    .args(&config.args)
                    .envs(&config.env)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .with_context(|| format!("could not start {command}"))?;
                let stdout = child.stdout.take().expect("stdout is piped");
                let stdin = child.stdin.take().expect("stdin is piped");
                let pending: Pending = Arc::default();
                let routes = pending.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
                        if message.get("method").is_some() {
                            continue;
                        }
                        if let Some(id) = message["id"].as_u64()
                            && let Some(reply) = routes.lock().expect("pending lock").remove(&id)
                        {
                            let _ = reply.send(message);
                        }
                    }
                });
                Transport::Stdio { stdin: tokio::sync::Mutex::new(stdin), pending, _child: child }
            }
            (None, Some(url)) => {
                let mut headers: Vec<(String, String)> = config.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                if let Some(var) = &config.bearer_token_env_var {
                    let token = std::env::var(var).with_context(|| format!("${var} isn't set"))?;
                    headers.push(("authorization".into(), format!("Bearer {token}")));
                }
                let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(30)).build()?;
                Transport::Http { client, url: url.clone(), headers, session: RefCell::new(None) }
            }
            (None, None) => bail!("the server config needs `command` or `url`"),
        };
        let connection = Self { transport, next_id: Cell::new(1), timeout };
        connection
            .request(
                "initialize",
                json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "killi", "version": env!("CARGO_PKG_VERSION")}}),
            )
            .await?;
        connection.notify("notifications/initialized").await?;
        Ok(connection)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let reply = match &self.transport {
            Transport::Stdio { stdin, pending, .. } => {
                let (tx, rx) = oneshot::channel();
                pending.lock().expect("pending lock").insert(id, tx);
                let mut line = message.to_string();
                line.push('\n');
                stdin.lock().await.write_all(line.as_bytes()).await.context("the server closed its input")?;
                tokio::time::timeout(self.timeout, rx)
                    .await
                    .map_err(|_| anyhow!("{method} timed out after {}s", self.timeout.as_secs()))?
                    .map_err(|_| anyhow!("the server exited"))?
            }
            Transport::Http { .. } => self.post(&message, Some(id)).await?,
        };
        if let Some(error) = reply.get("error") {
            bail!("{}", error["message"].as_str().unwrap_or(&error.to_string()));
        }
        Ok(reply["result"].clone())
    }

    async fn notify(&self, method: &str) -> Result<()> {
        let message = json!({"jsonrpc": "2.0", "method": method});
        match &self.transport {
            Transport::Stdio { stdin, .. } => {
                stdin.lock().await.write_all(format!("{message}\n").as_bytes()).await?;
            }
            Transport::Http { .. } => {
                self.post(&message, None).await?;
            }
        }
        Ok(())
    }

    /// Streamable HTTP: the reply comes back as JSON or as server-sent events.
    async fn post(&self, message: &Value, id: Option<u64>) -> Result<Value> {
        let Transport::Http { client, url, headers, session } = &self.transport else { unreachable!() };
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
        if let Some(id) = response.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *session.borrow_mut() = Some(id.to_string());
        }
        let status = response.status();
        if !status.is_success() {
            bail!("HTTP {status}: {}", crate::llm::truncate(&response.text().await.unwrap_or_default(), 500));
        }
        let Some(id) = id else { return Ok(Value::Null) };
        let streamed = response.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.contains("event-stream"));
        if !streamed {
            return Ok(serde_json::from_slice(&response.bytes().await?)?);
        }
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        while let Some(chunk) = stream.next().await {
            buffer.extend_from_slice(&chunk?);
            while let Some(end) = buffer.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buffer.drain(..=end).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else { continue };
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

fn expand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest).display().to_string(),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny MCP server in bash: answers initialize, tools/list and tools/call.
    const SERVER: &str = r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"t","version":"1"}}}\n' "$id" ;;
    *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}}\n' "$id" ;;
    *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echoed"}]}}\n' "$id" ;;
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
        };
        Mcp { servers: BTreeMap::from([("t".to_string(), config)]), ..Mcp::default() }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gateway_lists_then_calls() {
        let mcp = mcp();
        let listed = mcp.run(&json!({"server": "t"}), false).await;
        assert!(listed.text.contains("## echo\nEcho text"), "{}", listed.text);
        let called = mcp.run(&json!({"server": "t", "tool": "echo", "arguments": {"text": "hi"}}), false).await;
        assert_eq!(called.text, "echoed");
        assert!(mcp.run(&json!({"server": "nope"}), false).await.is_error);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn eager_servers_get_native_tools() {
        let mut mcp = mcp();
        let (definitions, problems) = mcp.start_eager(&["t".to_string()]).await;
        assert!(problems.is_empty());
        assert_eq!(definitions[0]["function"]["name"], "mcp__t__echo");
        assert!(mcp.is_native("mcp__t__echo"));
        assert_eq!(mcp.call_native("mcp__t__echo", json!({}), false).await.text, "echoed");
    }
}
