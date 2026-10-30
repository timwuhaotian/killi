//! One process that runs stdio MCP servers for every killi on this machine, so sessions share
//! a server instead of each starting its own. killi starts it, as `killi mcp-serve`, the first
//! time a session uses a stdio server; it exits once no killi has been attached for `IDLE`.
//!
//! A client sends one line naming the server to launch, gets `{"ok": true}` or an error back,
//! then speaks MCP as if to the server itself. The broker initializes each server once and
//! answers every client's `initialize` from that, gives requests ids of its own so replies go
//! back to the client that asked, and drops what servers send unasked.

use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, ChildStdin, Command};

use crate::tools::OwnSession;
use tokio::sync::{mpsc, oneshot, watch};

/// How long the broker and its servers outlive the last attached killi.
const IDLE: Duration = Duration::from_secs(15 * 60);
const INIT_TIMEOUT: Duration = Duration::from_secs(60);

/// A stdio server as the broker starts it; servers with the same launch are shared.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Launch {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
}

impl Launch {
    /// Starts the server in its own session, its stdin and stdout piped.
    pub fn spawn(&self) -> Result<Child> {
        Command::new(&self.command)
            .args(&self.args)
            .envs(&self.env)
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .own_session()
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("could not start {}", self.command))
    }
}

/// The broker's socket, in a folder of this user's own in the temporary folder because
/// socket paths are limited to about 100 bytes. The user and version are in the name, so a
/// killi only talks to its own user's broker of the same version.
pub fn socket_path() -> PathBuf {
    // SAFETY: getuid has no preconditions and can't fail.
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir()
        .join(format!("killi-mcp-{uid}-{}", env!("CARGO_PKG_VERSION")))
        .join("broker.sock")
}

/// Creates the broker socket's folder for this user alone, like the live sessions' sockets.
/// The folder's path is predictable, so an existing one only counts when it is this user's
/// own private folder: another user who pre-creates it loose could otherwise replace the
/// socket inside it with a fake broker.
fn ensure_socket_dir(socket: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Some(dir) = socket.parent() else {
        return Ok(());
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("could not create {}", dir.display()))?;
    let meta = std::fs::symlink_metadata(dir)
        .with_context(|| format!("could not check {}", dir.display()))?;
    // SAFETY: getuid has no preconditions and can't fail.
    if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != unsafe { libc::getuid() } {
        bail!(
            "{} is not this user's own folder; refusing to put the broker socket there",
            dir.display()
        );
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("could not tighten {}", dir.display()))?;
    }
    Ok(())
}

/// Whether `stream` comes from this user's own process, like the live sessions' socket check.
fn own_user(stream: &UnixStream) -> bool {
    // SAFETY: getuid has no preconditions and can't fail.
    stream
        .peer_cred()
        .is_ok_and(|cred| cred.uid() == unsafe { libc::getuid() })
}

/// Connects to the broker at `socket` for `launch`, starting the broker when `start` is set and
/// none answers.
pub async fn attach(socket: &Path, launch: &Launch, start: bool) -> Result<UnixStream> {
    let mut stream = match UnixStream::connect(socket).await {
        Ok(stream) => stream,
        Err(_) if start => start_broker(socket).await?,
        Err(e) => return Err(e).with_context(|| format!("no MCP broker at {}", socket.display())),
    };
    stream
        .write_all(format!("{}\n", serde_json::to_string(launch)?).as_bytes())
        .await?;
    let mut reply = Vec::new();
    loop {
        let mut byte = [0u8];
        if tokio::io::AsyncReadExt::read(&mut stream, &mut byte).await? == 0 || byte[0] == b'\n' {
            break;
        }
        reply.push(byte[0]);
    }
    let reply: Value =
        serde_json::from_slice(&reply).context("the MCP broker closed the connection")?;
    if let Some(error) = reply["error"].as_str() {
        bail!("{error}");
    }
    Ok(stream)
}

async fn start_broker(socket: &Path) -> Result<UnixStream> {
    std::process::Command::new(std::env::current_exe()?)
        .arg("mcp-serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .own_session()
        .spawn()
        .context("could not start the MCP broker")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        match UnixStream::connect(socket).await {
            Ok(stream) => return Ok(stream),
            Err(e) if Instant::now() > deadline => {
                return Err(e).context("the MCP broker didn't start");
            }
            Err(_) => {}
        }
    }
}

/// Where a server's reply goes: a client's request under its own id, or the broker's own.
enum Route {
    Client(mpsc::UnboundedSender<String>, Value),
    Broker(oneshot::Sender<Value>),
}

struct Server {
    stdin: tokio::sync::Mutex<ChildStdin>,
    routes: Arc<Mutex<HashMap<u64, Route>>>,
    next_id: AtomicU64,
    /// Turns true when the server exits; its clients are then let go, so they reconnect.
    exited: watch::Receiver<bool>,
    child: Mutex<Child>,
    /// The `initialize` result every client gets.
    init: Value,
}

impl Server {
    async fn start(launch: &Launch) -> Result<Self> {
        let mut child = launch.spawn()?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stdin = child.stdin.take().expect("stdin is piped");
        let routes: Arc<Mutex<HashMap<u64, Route>>> = Arc::default();
        let (exit, exited) = watch::channel(false);
        let reader_routes = routes.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(mut message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("method").is_some() {
                    continue;
                }
                let Some(route) = message["id"]
                    .as_u64()
                    .and_then(|id| reader_routes.lock().expect("routes").remove(&id))
                else {
                    continue;
                };
                match route {
                    Route::Client(client, id) => {
                        message["id"] = id;
                        let _ = client.send(message.to_string());
                    }
                    Route::Broker(reply) => {
                        let _ = reply.send(message);
                    }
                }
            }
            let _ = exit.send(true);
            for (_, route) in reader_routes.lock().expect("routes").drain() {
                if let Route::Client(client, id) = route {
                    let _ = client.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": "the server exited"}}).to_string());
                }
            }
        });
        let mut server = Self {
            stdin: tokio::sync::Mutex::new(stdin),
            routes,
            next_id: AtomicU64::new(1),
            exited,
            child: Mutex::new(child),
            init: Value::Null,
        };
        let params = json!({
            "protocolVersion": crate::mcp::PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "killi", "version": env!("CARGO_PKG_VERSION")},
        });
        let (tx, rx) = oneshot::channel();
        let id = server
            .forward(
                json!({"jsonrpc": "2.0", "method": "initialize", "params": params}),
                Route::Broker(tx),
            )
            .await?;
        let reply = tokio::time::timeout(INIT_TIMEOUT, rx)
            .await
            .map_err(|_| anyhow!("initialize timed out"))?
            .map_err(|_| anyhow!("the server exited while starting"))?;
        if let Some(error) = reply.get("error") {
            bail!("initialize failed: {error}");
        }
        server.routes.lock().expect("routes").remove(&id);
        server.init = reply["result"].clone();
        server
            .write(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await?;
        Ok(server)
    }

    /// Sends a request under a fresh id whose reply goes to `route`.
    async fn forward(&self, mut message: Value, route: Route) -> Result<u64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        message["id"] = id.into();
        self.routes.lock().expect("routes").insert(id, route);
        self.write(&message).await?;
        Ok(id)
    }

    async fn write(&self, message: &Value) -> Result<()> {
        self.stdin
            .lock()
            .await
            .write_all(format!("{message}\n").as_bytes())
            .await
            .context("the server closed its input")
    }

    fn kill(&self) {
        if let Some(pid) = self.child.lock().expect("child").id() {
            // SAFETY: killpg only sends a signal to the process group the broker started.
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

#[derive(Default)]
struct Broker {
    servers: tokio::sync::Mutex<HashMap<Launch, Arc<Server>>>,
    /// One start at a time per launch, so simultaneous attaches share one spawn instead of
    /// racing two servers (the loser of which could fail on a port or leak its children).
    starts: Mutex<HashMap<Launch, Arc<tokio::sync::Mutex<()>>>>,
    clients: AtomicUsize,
    last_left: Mutex<Option<Instant>>,
}

/// Runs the broker on `socket` until it has been idle for `IDLE`.
pub async fn serve(socket: &Path) -> Result<()> {
    if UnixStream::connect(socket).await.is_ok() {
        return Ok(());
    }
    ensure_socket_dir(socket)?;
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)
        .with_context(|| format!("could not listen on {}", socket.display()))?;
    let broker = Arc::new(Broker {
        last_left: Mutex::new(Some(Instant::now())),
        ..Broker::default()
    });
    let mut check = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                broker.clients.fetch_add(1, Ordering::SeqCst);
                let broker = broker.clone();
                tokio::spawn(async move {
                    let _ = client(&broker, stream).await;
                    if broker.clients.fetch_sub(1, Ordering::SeqCst) == 1 {
                        *broker.last_left.lock().expect("last_left") = Some(Instant::now());
                    }
                });
            }
            _ = check.tick() => {
                let idle = broker.clients.load(Ordering::SeqCst) == 0
                    && broker.last_left.lock().expect("last_left").is_some_and(|t| t.elapsed() > IDLE);
                if idle {
                    break;
                }
            }
        }
    }
    let _ = std::fs::remove_file(socket);
    for server in broker.servers.lock().await.values() {
        server.kill();
    }
    Ok(())
}

/// Serves one attached killi until it disconnects.
async fn client(broker: &Broker, stream: UnixStream) -> Result<()> {
    if !own_user(&stream) {
        bail!("a process of another user connected");
    }
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let launch: Launch = serde_json::from_str(
        &lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("no launch line"))?,
    )?;
    let server = match server(broker, &launch).await {
        Ok(server) => server,
        Err(e) => {
            writer
                .write_all(format!("{}\n", json!({"error": format!("{e:#}")})).as_bytes())
                .await?;
            return Ok(());
        }
    };
    writer.write_all(b"{\"ok\":true}\n").await?;
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writing = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if writer
                .write_all(format!("{line}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let mut exited = server.exited.clone();
    loop {
        let line = tokio::select! {
            line = lines.next_line() => line?,
            _ = exited.wait_for(|gone| *gone) => None,
        };
        let Some(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = message.get("id").filter(|id| !id.is_null()).cloned() else {
            continue;
        };
        if message["method"] == "initialize" {
            let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "result": server.init}).to_string());
        } else if let Err(e) = server
            .forward(message, Route::Client(tx.clone(), id.clone()))
            .await
        {
            let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": format!("{e:#}")}}).to_string());
        }
    }
    writing.abort();
    Ok(())
}

/// The running server for `launch`, started on first use and again after it exits. Starts
/// of the same launch are serialized (different launches still start in parallel), and the
/// map lock only guards the lookup and the insert, so one slow server doesn't hold every
/// other attach behind it.
async fn server(broker: &Broker, launch: &Launch) -> Result<Arc<Server>> {
    if let Some(server) = broker
        .servers
        .lock()
        .await
        .get(launch)
        .filter(|s| !*s.exited.borrow())
        .cloned()
    {
        return Ok(server);
    }
    let start = broker
        .starts
        .lock()
        .expect("starts")
        .entry(launch.clone())
        .or_default()
        .clone();
    let _starting = start.lock().await;
    if let Some(server) = broker
        .servers
        .lock()
        .await
        .get(launch)
        .filter(|s| !*s.exited.borrow())
        .cloned()
    {
        return Ok(server);
    }
    let server = Arc::new(Server::start(launch).await?);
    broker
        .servers
        .lock()
        .await
        .insert(launch.clone(), server.clone());
    Ok(server)
}

#[cfg(test)]
mod tests {
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
        let first = server(&broker, &launch).await.unwrap();
        let second = server(&broker, &launch).await.unwrap();
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
        let accepted =
            tokio::spawn(async move { listener.accept().await.map(|(stream, _)| stream) });
        let _connecting = UnixStream::connect(&socket).await.unwrap();
        assert!(own_user(&accepted.await.unwrap().unwrap()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
