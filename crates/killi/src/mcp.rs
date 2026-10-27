//! MCP servers, reached through one `mcp` tool so the tool list never changes: called with a
//! server name it lists that server's tools, and with a tool name it calls it. No server
//! starts until it is first used. Servers a project names in `[mcp] eager` start with the
//! session instead and get native tool definitions, fixed for the session.
//!
//! Servers come from `~/.killi/mcp.json` and the project's `.mcp.json`, in the
//! `{"mcpServers": {...}}` format Claude Code and Kimi Code use. Stdio servers run in the
//! shared broker (see `broker.rs`), so killi sessions share them: user servers start in the
//! home folder, project servers in the project root.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::oneshot;

use crate::broker::{self, Launch};

use crate::config::{home, killi_home};
use crate::tools::Output;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
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
    /// Where a stdio server starts: the home folder for user servers, else the project root.
    #[serde(skip)]
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct File {
    #[serde(rename = "mcpServers", default)]
    servers: BTreeMap<String, ServerConfig>,
}

#[derive(Default)]
pub struct Mcp {
    servers: BTreeMap<String, ServerConfig>,
    /// Names whose effective definition came from the project's `.mcp.json` (it replaces a
    /// user server of the same name), so global eager entries naming them need trust too.
    project: BTreeSet<String>,
    live: RefCell<HashMap<String, Rc<Connection>>>,
    listed: RefCell<HashMap<String, Vec<Value>>>,
    /// Native tool names of eager servers' tools, to their server and tool.
    native: HashMap<String, (String, String)>,
    /// The broker's socket; without one, each stdio server is started directly.
    broker: Option<PathBuf>,
    /// Servers the user turned off with /mcp.
    off: RefCell<BTreeSet<String>>,
    /// Where `off` is kept for later sessions; nothing is kept without it.
    off_path: Option<PathBuf>,
    /// Turning a server on or off, to tell the model at its next prompt.
    notes: RefCell<Vec<String>>,
}

/// A server as /mcp lists it.
pub struct Listed {
    pub name: String,
    pub on: bool,
    /// The URL's host, or the command that starts it.
    pub hint: String,
}

impl Mcp {
    pub fn load(cwd: &Path) -> Self {
        let root = cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(cwd);
        let mut servers = BTreeMap::new();
        let mut project = BTreeSet::new();
        for (path, cwd, is_project) in
            [(killi_home().join("mcp.json"), home(), false), (root.join(".mcp.json"), root.to_path_buf(), true)]
        {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            if let Ok(file) = serde_json::from_str::<File>(&text) {
                for (name, server) in file.servers.into_iter().filter(|(_, s)| s.enabled != Some(false)) {
                    if is_project {
                        project.insert(name.clone());
                    }
                    servers.insert(name, ServerConfig { cwd: cwd.clone(), ..server });
                }
            }
        }
        let off_path = killi_home().join("mcp-off.json");
        let off = read_off(&off_path);
        Self { servers, project, broker: Some(broker::socket_path()), off: RefCell::new(off), off_path: Some(off_path), ..Self::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// The servers that are on.
    pub fn names(&self) -> Vec<&str> {
        let off = self.off.borrow();
        self.servers.keys().filter(|name| !off.contains(*name)).map(String::as_str).collect()
    }

    /// Every configured server, on or off, for /mcp.
    pub fn list(&self) -> Vec<Listed> {
        let off = self.off.borrow();
        self.servers
            .iter()
            .map(|(name, config)| {
                let hint = match (&config.url, &config.command) {
                    (Some(url), _) => url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or(url).to_string(),
                    (None, command) => std::iter::once(command.as_deref().unwrap_or_default()).chain(config.args.iter().map(String::as_str)).collect::<Vec<_>>().join(" "),
                };
                Listed { name: name.clone(), on: !off.contains(name), hint }
            })
            .collect()
    }

    /// Whether `name`'s effective definition came from the project's `.mcp.json`.
    pub fn is_project(&self, name: &str) -> bool {
        self.project.contains(name)
    }

    /// Everything `name` would run with or send, for the startup trust fingerprint: a change
    /// to its environment, headers, credentials source or working folder re-asks, just as a
    /// changed command does. Hashed by the caller, never shown.
    pub fn trust_input(&self, name: &str) -> Option<String> {
        let config = self.servers.get(name)?;
        Some(
            json!({
                "command": config.command.as_deref().map(expand),
                "args": config.args,
                "env": config.env,
                "url": config.url,
                "headers": config.headers,
                "bearer_token_env_var": config.bearer_token_env_var,
                "cwd": config.cwd,
            })
            .to_string(),
        )
    }

    /// The command line `name` would run, for the startup trust check: the expanded command
    /// with its arguments, or the server's URL when it runs nowhere local.
    pub fn command_line(&self, name: &str) -> Option<String> {
        let config = self.servers.get(name)?;
        match (&config.command, &config.url) {
            (Some(command), _) => Some(std::iter::once(expand(command)).chain(config.args.iter().cloned()).collect::<Vec<_>>().join(" ")),
            (None, Some(url)) => Some(url.clone()),
            (None, None) => None,
        }
    }

    /// Turns a server on or off for this session and the ones after it, and returns whether
    /// it is on now. The tool list stays as it is; calls to a server that is off are refused.
    pub fn toggle(&self, name: &str) -> Result<bool> {
        if !self.servers.contains_key(name) {
            bail!("no MCP server `{name}`; servers: {}", self.servers.keys().cloned().collect::<Vec<_>>().join(", "));
        }
        let on = {
            let mut off = self.off.borrow_mut();
            let was_off = off.remove(name);
            if !was_off {
                off.insert(name.to_string());
            }
            was_off
        };
        if !on {
            self.live.borrow_mut().remove(name);
        }
        self.notes.borrow_mut().push(match on {
            true => format!("The user turned on the MCP server {name}; reach it through the `mcp` tool."),
            false => format!("The user turned off the MCP server {name}; don't use it."),
        });
        if let Some(path) = &self.off_path {
            crate::fs_atomic::write(path, serde_json::to_string(&*self.off.borrow())?.as_bytes(), None)
                .with_context(|| format!("could not write {}", path.display()))?;
        }
        Ok(on)
    }

    /// What changed since the model last heard, as notes for its next prompt.
    pub fn take_notes(&self) -> Vec<String> {
        std::mem::take(&mut self.notes.borrow_mut())
    }

    fn refused(&self, server: &str) -> Option<Output> {
        self.off
            .borrow()
            .contains(server)
            .then(|| Output::error(format!("The user turned off the MCP server `{server}` with /mcp. Don't use it unless they turn it on again.")))
    }

    /// The `mcp` gateway tool, naming the servers it can reach.
    pub fn definition(&self) -> Option<Value> {
        if self.servers.is_empty() {
            return None;
        }
        let names = match self.names().join(", ") {
            names if names.is_empty() => "none are on now".to_string(),
            names => names,
        };
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

    /// Starts the named servers now and returns native definitions of their tools. The
    /// servers start together, so one slow server doesn't hold the others behind it.
    pub async fn start_eager(&mut self, names: &[String]) -> (Vec<Value>, Vec<String>) {
        let mut definitions = Vec::new();
        let mut problems = Vec::new();
        let started = futures_util::future::join_all(names.iter().map(|name| {
            let tools = self.tools(name);
            async move { (name, tools.await) }
        }))
        .await;
        for (name, tools) in started {
            match tools {
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
        if let Some(refused) = self.refused(server) {
            return refused;
        }
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
        if let Some(refused) = self.refused(server) {
            return refused;
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
            Some(tool) => match Self::tool_arguments(args) {
                Ok(arguments) => self.call(server, tool, arguments, images).await,
                Err(e) => Output::error(e),
            },
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
        let empty = Vec::new();
        let content = result["content"].as_array().unwrap_or(&empty);
        let (mut text, image) = Self::render_parts(content, images);
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

    /// The text lines of an MCP result and the image to attach, if any: the first image
    /// when the model sees images, every other one named and never dumped as base64.
    fn render_parts(content: &[Value], images: bool) -> (Vec<String>, Option<String>) {
        let mut text = Vec::new();
        let mut image = None;
        for part in content {
            match part["type"].as_str() {
                Some("text") => text.push(part["text"].as_str().unwrap_or_default().to_string()),
                Some("image") if images && image.is_none() => {
                    image = Some(format!("data:{};base64,{}", part["mimeType"].as_str().unwrap_or("image/png"), part["data"].as_str().unwrap_or_default()));
                    text.push("(image attached below)".into());
                }
                Some("image") => text.push(image_note(part)),
                Some("resource") => text.push(part["resource"]["text"].as_str().map_or_else(|| part["resource"].to_string(), String::from)),
                _ => text.push(part.to_string()),
            }
        }
        (text, image)
    }

    /// The `arguments` for an MCP tool call: an object as sent, or a string holding one.
    /// Anything else (missing, or a value no tool takes) is no arguments, as before.
    fn tool_arguments(args: &Value) -> Result<Value, String> {
        match args.get("arguments") {
            None | Some(Value::Null) => Ok(json!({})),
            Some(Value::Object(_)) => Ok(args["arguments"].clone()),
            Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
                Ok(parsed @ Value::Object(_)) => Ok(parsed),
                Ok(_) => Err("mcp `arguments` is a string but not a JSON object; send an object.".into()),
                Err(e) => Err(format!("mcp `arguments` is a string but not JSON: {e}")),
            },
            Some(_) => Ok(json!({})),
        }
    }

    async fn connection(&self, server: &str) -> Result<Rc<Connection>> {
        if let Some(connection) = self.live.borrow().get(server).filter(|c| !c.closed.load(Ordering::SeqCst)) {
            return Ok(connection.clone());
        }
        let config = self.servers.get(server).ok_or_else(|| anyhow!("no MCP server `{server}`"))?;
        let connection = Rc::new(Connection::open(config, self.broker.as_deref()).await?);
        self.live.borrow_mut().insert(server.to_string(), connection.clone());
        Ok(connection)
    }
}

/// The servers turned off, from `path`; none when it is missing or unreadable.
fn read_off(path: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

/// An image the model isn't shown, named by type and size instead of dumped as base64.
fn image_note(part: &Value) -> String {
    let mime = part["mimeType"].as_str().unwrap_or("image/png");
    let kb = part["data"].as_str().map_or(0, |data| data.len() * 3 / 4 / 1024);
    format!("[image omitted: {mime} ({kb} KB not shown)]")
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
    /// Set once a stdio server or the broker hangs up, so the next use reconnects.
    closed: Arc<AtomicBool>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

/// A reply slot that goes away with the request: a timed-out, cancelled or never-sent
/// request leaves nothing in `pending`, so dead ids never pile up behind later ones.
struct PendingSlot {
    pending: Pending,
    id: u64,
}

impl Drop for PendingSlot {
    fn drop(&mut self) {
        self.pending.lock().expect("pending lock").remove(&self.id);
    }
}

enum Transport {
    /// JSON lines to the broker, or to a server started directly when no broker answers.
    /// A directly started server's group stays tracked while it runs, for quitting on a signal.
    Lines { writer: tokio::sync::Mutex<Box<dyn AsyncWrite + Unpin + Send>>, pending: Pending, _child: Option<(Child, Option<crate::tools::Tracked>)> },
    Http { client: reqwest::Client, url: String, headers: Vec<(String, String)>, session: RefCell<Option<String>> },
}

impl Connection {
    async fn open(config: &ServerConfig, broker: Option<&Path>) -> Result<Self> {
        let timeout = config.tool_timeout_ms.map_or(DEFAULT_TIMEOUT, Duration::from_millis);
        let closed = Arc::new(AtomicBool::new(false));
        let transport = match (&config.command, &config.url) {
            (Some(command), _) => {
                let launch = Launch { command: expand(command), args: config.args.clone(), env: config.env.clone(), cwd: config.cwd.clone() };
                let attached = match broker {
                    Some(socket) => broker::attach(socket, &launch, true).await.ok(),
                    None => None,
                };
                let (reader, writer, child): (Box<dyn AsyncRead + Unpin + Send>, Box<dyn AsyncWrite + Unpin + Send>, _) = match attached {
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
                    hung_up.store(true, Ordering::SeqCst);
                    routes.lock().expect("pending lock").clear();
                });
                Transport::Lines { writer: tokio::sync::Mutex::new(writer), pending, _child: child }
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
        let connection = Self { transport, next_id: Cell::new(1), timeout, closed };
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
            Transport::Lines { writer, pending, .. } => {
                let (tx, rx) = oneshot::channel();
                pending.lock().expect("pending lock").insert(id, tx);
                let _slot = PendingSlot { pending: pending.clone(), id };
                let mut line = message.to_string();
                line.push('\n');
                writer.lock().await.write_all(line.as_bytes()).await.context("the server closed its input")?;
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
            Transport::Lines { writer, .. } => {
                writer.lock().await.write_all(format!("{message}\n").as_bytes()).await?;
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
        let mut lines = crate::llm::SseLines::default();
        while let Some(chunk) = stream.next().await {
            lines.push(&chunk?);
            while let Some(line) = lines.next_line() {
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
}
