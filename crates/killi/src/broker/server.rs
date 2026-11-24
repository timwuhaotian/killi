//! One stdio server the broker runs, and how the broker tells a live server from one that
//! exited or stopped answering.

use super::*;

/// How long a write to a server's input may go without the server taking in a single byte
/// before the server counts as stuck. A server that reads its input never stalls a write this
/// long, however large the line, since the pipe drains as it reads; a stuck one would hold
/// every client's writes behind it. Waiting behind another client's write doesn't count.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a request whose client has given up may go unanswered, with nothing at all
/// from the server since it was sent, before the server counts as stuck. Longer than a
/// client's default timeout, and a request whose client still waits never counts, so a
/// slow server is left alone while anyone waits on it.
const STALE: Duration = Duration::from_secs(150);

/// When the broker gives up on a server: the constants above, shorter in tests.
#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub init: Duration,
    pub write: Duration,
    pub stale: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            init: INIT_TIMEOUT,
            write: WRITE_TIMEOUT,
            stale: STALE,
        }
    }
}

/// Where a server's reply goes: a client's request under its own id, or the broker's own.
pub(super) enum Route {
    Client(mpsc::UnboundedSender<String>, Value),
    Broker(oneshot::Sender<Value>),
}

/// A request sent to a server and not yet answered.
struct Sent {
    route: Route,
    at: Instant,
}

type Routes = Arc<Mutex<HashMap<u64, Sent>>>;

/// Answers a request with an error: a client's under its own id; the broker's own just ends.
fn fail(route: Route, reason: &str) {
    if let Route::Client(client, id) = route {
        let _ = client.send(
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": reason}})
                .to_string(),
        );
    }
}

/// Answers every waiting client with an error and forgets every request.
fn fail_routes(routes: &Routes, reason: &str) {
    for (_, sent) in routes.lock().expect("routes").drain() {
        fail(sent.route, reason);
    }
}

const EXITED: &str = "the server exited";

/// A server's process group, which it leads, killed at most once: when the server is
/// dropped or found stuck, or right after its leader exits, to end what it left running.
/// Once the leader is reaped its pid may be reused, so the group is never signalled later.
struct Group {
    pgid: Option<u32>,
    killed: AtomicBool,
}

impl Group {
    fn kill(&self) {
        if !self.killed.swap(true, Ordering::SeqCst)
            && let Some(pgid) = self.pgid
        {
            // SAFETY: killpg only sends a signal to the process group the broker started.
            unsafe { libc::killpg(pgid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

/// A server that started but didn't answer `initialize` in time. Starting it again directly
/// would wait as long again, so a client reports this rather than trying.
#[derive(Debug)]
pub(super) struct InitTimedOut(pub Duration);

impl std::fmt::Display for InitTimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "initialize timed out after {}s", self.0.as_secs())
    }
}

impl std::error::Error for InitTimedOut {}

pub(super) struct Server {
    stdin: tokio::sync::Mutex<ChildStdin>,
    routes: Routes,
    next_id: AtomicU64,
    exit: Arc<watch::Sender<bool>>,
    /// Turns true when the server exits or is killed; its clients are then let go, so they
    /// reconnect.
    pub(super) exited: watch::Receiver<bool>,
    group: Arc<Group>,
    /// When the server last wrote anything.
    heard: Arc<Mutex<Instant>>,
    limits: Limits,
    /// The `initialize` result every client gets.
    pub(super) init: Value,
}

impl Server {
    pub(super) async fn start(launch: &Launch, limits: Limits) -> Result<Self> {
        let mut child = launch.spawn()?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stdin = child.stdin.take().expect("stdin is piped");
        let group = Arc::new(Group {
            pgid: child.id(),
            killed: AtomicBool::new(false),
        });
        let routes: Routes = Arc::default();
        let (exit, exited) = watch::channel(false);
        let exit = Arc::new(exit);
        let heard = Arc::new(Mutex::new(Instant::now()));
        tokio::spawn({
            let (routes, exit, heard) = (routes.clone(), exit.clone(), heard.clone());
            async move {
                let mut stdout = BufReader::new(stdout);
                while let Ok(Some(line)) = line::read_line(&mut stdout).await {
                    *heard.lock().expect("heard") = Instant::now();
                    let Ok(mut message) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if message.get("method").is_some() {
                        continue;
                    }
                    let Some(sent) = message["id"]
                        .as_u64()
                        .and_then(|id| routes.lock().expect("routes").remove(&id))
                    else {
                        continue;
                    };
                    match sent.route {
                        Route::Client(client, id) => {
                            message["id"] = id;
                            let _ = client.send(message.to_string());
                        }
                        Route::Broker(reply) => {
                            let _ = reply.send(message);
                        }
                    }
                }
                exit.send_replace(true);
                fail_routes(&routes, EXITED);
            }
        });
        // The leader exiting ends the server even while something it started still holds
        // its output open.
        tokio::spawn({
            let (routes, exit, group) = (routes.clone(), exit.clone(), group.clone());
            async move {
                let _ = child.wait().await;
                group.kill();
                exit.send_replace(true);
                fail_routes(&routes, EXITED);
            }
        });
        let mut server = Self {
            stdin: tokio::sync::Mutex::new(stdin),
            routes,
            next_id: AtomicU64::new(1),
            exit,
            exited,
            group,
            heard,
            limits,
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
        let reply = tokio::time::timeout(limits.init, rx)
            .await
            .map_err(|_| InitTimedOut(limits.init))?
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

    /// Sends a request under a fresh id whose reply goes to `route`. A request that can't be
    /// sent is answered with the error here, once, whether or not the server was stopped for
    /// it, so the caller only passes the error on when it waits on the route itself.
    pub(super) async fn forward(&self, mut message: Value, route: Route) -> Result<u64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        message["id"] = id.into();
        let sent = Sent {
            route,
            at: Instant::now(),
        };
        self.routes.lock().expect("routes").insert(id, sent);
        if let Err(e) = self.write(&message).await {
            if let Some(sent) = self.routes.lock().expect("routes").remove(&id) {
                fail(sent.route, &format!("{e:#}"));
            }
            return Err(e);
        }
        Ok(id)
    }

    /// Writes one line to the server. A server that takes in nothing for the write limit is
    /// killed: it holds every other client's writes behind this one, and a line cut short
    /// would garble the next. A server that drains a long line slowly is left alone.
    async fn write(&self, message: &Value) -> Result<()> {
        let line = format!("{message}\n");
        let mut exited = self.exited.clone();
        let mut stdin = tokio::select! {
            stdin = self.stdin.lock() => stdin,
            _ = exited.wait_for(|gone| *gone) => bail!(EXITED),
        };
        let mut rest = line.as_bytes();
        while !rest.is_empty() {
            match tokio::time::timeout(self.limits.write, stdin.write(rest)).await {
                Ok(Ok(0)) => bail!("the server closed its input"),
                Ok(Ok(taken)) => rest = &rest[taken..],
                Ok(Err(e)) => return Err(e).context("the server closed its input"),
                Err(_) => {
                    let reason = format!(
                        "the server stopped reading its input for {}s; it was stopped, and the next call starts it again",
                        self.limits.write.as_secs()
                    );
                    self.stop(&reason);
                    bail!(reason)
                }
            }
        }
        Ok(())
    }

    /// Whether a request has waited longer than `after` with nothing at all from the server
    /// since it was sent; with `given_up`, only requests whose client has left count.
    fn silent(&self, after: Duration, given_up: bool) -> bool {
        let heard = *self.heard.lock().expect("heard");
        self.routes.lock().expect("routes").values().any(|sent| {
            let left = matches!(&sent.route, Route::Client(client, _) if client.is_closed());
            (left || !given_up) && sent.at.elapsed() > after && heard < sent.at
        })
    }

    /// Whether the server looks stuck: a request its client gave up on has waited longer
    /// than the stale limit, and the server has written nothing since it was sent.
    fn stale(&self) -> bool {
        self.silent(self.limits.stale, true)
    }

    /// Whether a client whose request went unanswered for `after` should get a fresh server
    /// instead of this one: some request has waited that long, never less than the write
    /// limit, and the server has written nothing to anyone since it was sent.
    pub(super) fn stuck(&self, after: Duration) -> bool {
        self.silent(after.max(self.limits.write), false)
    }

    /// Kills the server's whole group and lets its clients go.
    pub(super) fn kill(&self) {
        self.stop(EXITED);
    }

    /// Kills the server's whole group and lets its clients go, answering every request
    /// still waiting with `reason`.
    pub(super) fn stop(&self, reason: &str) {
        self.group.kill();
        self.exit.send_replace(true);
        fail_routes(&self.routes, reason);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.group.kill();
    }
}

/// Kills the servers that look stuck and forgets the ones that are gone, so the next
/// attach starts a fresh one.
pub(super) fn sweep(servers: &mut HashMap<Launch, Arc<Server>>) {
    servers.retain(|_, server| {
        if server.stale() {
            server.kill();
        }
        !*server.exited.borrow()
    });
}
