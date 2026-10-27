//! MCP servers, reached through one `mcp` tool so the tool list never changes: called with a
//! server name it lists that server's tools, and with a tool name it calls it. No server
//! starts until it is first used. Servers a project names in `[mcp] eager` start with the
//! session instead and get native tool definitions, fixed for the session.
//!
//! Servers come from `~/.killi/mcp.json` and the project's `.mcp.json`, in the
//! `{"mcpServers": {...}}` format Claude Code and Kimi Code use. Stdio servers run in the
//! shared broker (see `broker.rs`), so killi sessions share them: user servers start in the
//! home folder, project servers in the project root.

mod connection;
mod gateway;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWrite;
use tokio::process::Child;
use tokio::sync::oneshot;

use crate::broker;

use crate::config::{home, killi_home};

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

fn expand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest).display().to_string(),
        None => path.to_string(),
    }
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

#[cfg(test)]
mod tests;
