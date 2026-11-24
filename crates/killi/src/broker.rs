//! One process that runs stdio MCP servers for every killi on this machine, so sessions share
//! a server instead of each starting its own. killi starts it, as `killi mcp-serve`, the first
//! time a session uses a stdio server; it exits once no killi has been attached for `IDLE`.
//!
//! A client sends one line naming the server to launch, gets `{"ok": true}` or an error back,
//! then speaks MCP as if to the server itself. The broker initializes each server once and
//! answers every client's `initialize` from that, gives requests ids of its own so replies go
//! back to the client that asked, and drops what servers send unasked. A server that exits, or
//! stops taking its input or answering, is killed and started again on the next attach. A
//! client whose request timed out says so when it attaches again, and a server that has said
//! nothing to anyone since is then started afresh rather than handed out again.

mod line;
mod server;

use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, ChildStdin, Command};

use crate::tools::OwnSession;
use tokio::sync::{mpsc, oneshot, watch};

pub(crate) use line::read_line;
use server::{InitTimedOut, Limits, Route, Server};

/// How long the broker and its servers outlive the last attached killi.
const IDLE: Duration = Duration::from_secs(15 * 60);
const INIT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long `attach` waits for the broker's answer: past the broker's own wait for a server
/// to start, so a slow start is reported by the broker rather than cut short here. A broker
/// that doesn't answer even then is still starting the server, so it counts as refusing.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(INIT_TIMEOUT.as_secs() + 5);
/// The longest answer to a launch line `attach` reads.
const MAX_ANSWER: usize = 64 * 1024;
/// How often the broker checks for idleness and stuck servers.
const CHECK_EVERY: Duration = Duration::from_secs(30);

/// A stdio server as the broker starts it; servers with the same launch are shared.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Launch {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
}

/// The line a client attaches with: the launch, and the timeout its last request to this
/// server ran out after, if one did, so the broker can restart a server that is stuck.
#[derive(Serialize, Deserialize)]
struct Attach {
    #[serde(flatten)]
    launch: Launch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stuck_after_ms: Option<u64>,
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
pub(crate) fn ensure_socket_dir(socket: &Path) -> Result<()> {
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

/// The broker's own verdict on a launch: the server started but never finished starting.
/// Starting it directly would wait as long again, so this is reported, not worked around.
#[derive(Debug)]
pub struct BrokerRefused(pub String);

impl std::fmt::Display for BrokerRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BrokerRefused {}

/// Whether the broker socket at `socket`, if there is one, is this user's own socket.
fn check_socket(socket: &Path) -> Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    match std::fs::symlink_metadata(socket) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("could not check {}", socket.display())),
        // SAFETY: getuid has no preconditions and can't fail.
        Ok(meta) if meta.file_type().is_socket() && meta.uid() == unsafe { libc::getuid() } => {
            Ok(())
        }
        Ok(_) => bail!(
            "{} is not this user's broker socket; not using it",
            socket.display()
        ),
    }
}

/// Connects to the broker at `socket` for `launch`, starting the broker when `start` is set and
/// none answers. The socket's folder and the socket must be this user's own before anything,
/// the server's `env` included, is sent. `stuck_after` is the timeout this client's last
/// request to the server ran out after, if one did. An error is a `BrokerRefused` when the
/// broker ran the server and gave up on it; any other, a broker that never answers included,
/// means no usable broker answered, so the caller starts the server itself.
pub async fn attach(
    socket: &Path,
    launch: &Launch,
    start: bool,
    stuck_after: Option<Duration>,
) -> Result<UnixStream> {
    ensure_socket_dir(socket)?;
    check_socket(socket)?;
    let mut stream = match UnixStream::connect(socket).await {
        Ok(stream) => stream,
        Err(_) if start => start_broker(socket).await?,
        Err(e) => return Err(e).with_context(|| format!("no MCP broker at {}", socket.display())),
    };
    if !own_user(&stream) {
        bail!("the MCP broker at {} is another user's", socket.display());
    }
    let line = Attach {
        launch: launch.clone(),
        stuck_after_ms: stuck_after.map(|after| after.as_millis() as u64),
    };
    tokio::time::timeout(ATTACH_TIMEOUT, handshake(&mut stream, &line))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "the MCP broker didn't answer within {}s",
                ATTACH_TIMEOUT.as_secs()
            )
        })??;
    Ok(stream)
}

/// Sends the launch line and reads the broker's answer, byte by byte so nothing after it is
/// taken from the stream.
async fn handshake(stream: &mut UnixStream, line: &Attach) -> Result<()> {
    stream
        .write_all(format!("{}\n", serde_json::to_string(line)?).as_bytes())
        .await?;
    let mut reply = Vec::new();
    loop {
        let mut byte = [0u8];
        if tokio::io::AsyncReadExt::read(stream, &mut byte).await? == 0 || byte[0] == b'\n' {
            break;
        }
        if reply.len() == MAX_ANSWER {
            bail!("the MCP broker's answer is too long");
        }
        reply.push(byte[0]);
    }
    let reply: Value =
        serde_json::from_slice(&reply).context("the MCP broker closed the connection")?;
    match reply["error"].as_str() {
        Some(error) if reply["final"] == true => Err(BrokerRefused(error.to_string()).into()),
        Some(error) => bail!("{error}"),
        None => Ok(()),
    }
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

#[derive(Default)]
struct Broker {
    servers: tokio::sync::Mutex<HashMap<Launch, Arc<Server>>>,
    /// One start at a time per launch, so simultaneous attaches share one spawn instead of
    /// racing two servers (the loser of which could fail on a port or leak its children).
    starts: Mutex<HashMap<Launch, Arc<tokio::sync::Mutex<()>>>>,
    /// When each launch last failed to answer `initialize` in time. Attaches within the init
    /// limit after that get the same final answer at once instead of waiting on another start.
    init_timed_out: Mutex<HashMap<Launch, Instant>>,
    clients: AtomicUsize,
    last_left: Mutex<Option<Instant>>,
    limits: Limits,
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
    let mut check = tokio::time::interval(CHECK_EVERY);
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
                server::sweep(&mut *broker.servers.lock().await);
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
    let mut reader = BufReader::new(reader);
    let Attach {
        launch,
        stuck_after_ms,
    } = serde_json::from_str(
        &read_line(&mut reader)
            .await?
            .ok_or_else(|| anyhow!("no launch line"))?,
    )?;
    let stuck_after = stuck_after_ms.map(Duration::from_millis);
    let server = match server(broker, &launch, stuck_after).await {
        Ok(server) => server,
        Err(e) => {
            let answer = json!({"error": format!("{e:#}"), "final": e.is::<InitTimedOut>()});
            writer.write_all(format!("{answer}\n").as_bytes()).await?;
            return Ok(());
        }
    };
    writer.write_all(b"{\"ok\":true}\n").await?;
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let mut writing = tokio::spawn(async move {
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
    let server_gone = loop {
        let line = tokio::select! {
            line = read_line(&mut reader) => line?,
            _ = exited.wait_for(|gone| *gone) => break true,
        };
        let Some(line) = line else { break false };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = message.get("id").filter(|id| !id.is_null()).cloned() else {
            continue;
        };
        if message["method"] == "initialize" {
            let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "result": server.init}).to_string());
        } else {
            let _ = server
                .forward(message, Route::Client(tx.clone(), id.clone()))
                .await;
        }
    };
    if server_gone {
        // Every waiting request has been answered with the reason; those answers reach the
        // client before it is let go.
        drop(tx);
        let _ = tokio::time::timeout(Duration::from_secs(5), &mut writing).await;
    }
    writing.abort();
    Ok(())
}

/// The running server for `launch`, started on first use and again after it exits. Starts
/// of the same launch are serialized (different launches still start in parallel), and the
/// map lock only guards the lookup and the insert, so one slow server doesn't hold every
/// other attach behind it. With `stuck_after`, a server that looks stuck to a client whose
/// request ran out after that long is stopped first, so a fresh one starts.
async fn server(
    broker: &Broker,
    launch: &Launch,
    stuck_after: Option<Duration>,
) -> Result<Arc<Server>> {
    if let Some(after) = stuck_after
        && let Some(old) = broker.servers.lock().await.get(launch).cloned()
        && old.stuck(after)
    {
        old.stop(&format!(
            "the server answered nothing for over {}s; it was stopped, and the next call starts it again",
            after.as_secs()
        ));
    }
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
    let init = broker.limits.init;
    let failed_at = broker
        .init_timed_out
        .lock()
        .expect("init_timed_out")
        .get(launch)
        .copied();
    if failed_at.is_some_and(|at| at.elapsed() < init) {
        return Err(InitTimedOut(init).into());
    }
    let started = Server::start(launch, broker.limits).await;
    {
        let mut timed_out = broker.init_timed_out.lock().expect("init_timed_out");
        match &started {
            Err(e) if e.is::<InitTimedOut>() => {
                timed_out.insert(launch.clone(), Instant::now());
            }
            _ => {
                timed_out.remove(launch);
            }
        }
    }
    let server = Arc::new(started?);
    broker
        .servers
        .lock()
        .await
        .insert(launch.clone(), server.clone());
    Ok(server)
}

#[cfg(test)]
mod tests;
