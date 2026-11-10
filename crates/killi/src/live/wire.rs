//! The socket a session answers on: what one session asks of another, one JSON line each way,
//! and for a task's events one line per event until it ends.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use super::{Entry, MAX_TASK_TIMEOUT, TASK_TIMEOUT};

/// The longest request line a session reads.
const MAX_REQUEST: u64 = 16 * 1024;

/// How long `killi ask` waits: the other session's model reads its whole conversation first.
const ASK_TIMEOUT: Duration = Duration::from_secs(300);

/// How long a status or a message waits for the other session to take it.
const QUICK_TIMEOUT: Duration = Duration::from_secs(10);

/// Seconds a client waits past a task's own limit, so the session's answer that the task ran
/// out of time arrives instead of the client giving up first.
const TASK_SLACK: u64 = 30;

/// What one session asks of another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Request {
    /// What it is doing, answered without the model.
    Status,
    /// A question its model answers from its conversation, without keeping it.
    Ask {
        #[serde(default)]
        from: Option<u32>,
        text: String,
    },
    /// A message for its model's next step, or its user's next prompt.
    Tell {
        #[serde(default)]
        from: Option<u32>,
        text: String,
    },
    /// A task to run to a final report. Without `quick` the reply waits for that report;
    /// with it, the reply only takes the task's id. `after` names a task this one continues,
    /// and `timeout` is how long it may run, in seconds.
    Send {
        #[serde(default)]
        from: Option<u32>,
        text: String,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        after: Option<String>,
        #[serde(default, rename = "async")]
        quick: bool,
    },
    /// What a task stands on now, and its report once it ends.
    Get {
        id: String,
    },
    /// Every task this session has taken, oldest first.
    List,
    /// Stop a task that is still running.
    Cancel {
        id: String,
    },
    /// A task's events as they happen, one JSON object per line, until it ends.
    Follow {
        id: String,
    },
}

#[derive(Serialize, Deserialize)]
pub(super) struct Envelope {
    #[serde(default)]
    pub(super) v: u32,
    #[serde(flatten)]
    pub(super) request: Request,
}

/// Where a session's answer goes: one JSON object, or one line per event until a task
/// ends, which is what a `Follow` reads.
pub enum Reply {
    Once(oneshot::Sender<Value>),
    Stream(mpsc::UnboundedSender<String>),
}

/// A request from another session and where its reply goes: `{"ok": true, …}`, or
/// `{"error": "…"}` to say why not.
pub struct Incoming {
    pub request: Request,
    pub reply: Reply,
}

/// Listens on `socket` for other sessions of this user and hands each request to the
/// receiver; the connection waits for the reply.
pub fn listen(socket: &Path) -> Result<mpsc::UnboundedReceiver<Incoming>> {
    if let Some(dir) = socket.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    let _ = fs::remove_file(socket);
    let listener = UnixListener::bind(socket)
        .with_context(|| format!("could not listen on {}", socket.display()))?;
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let _ = serve(stream, tx).await;
            });
        }
    });
    Ok(rx)
}

async fn serve(stream: UnixStream, session: mpsc::UnboundedSender<Incoming>) -> Result<()> {
    // SAFETY: getuid has no preconditions and can't fail.
    if stream.peer_cred()?.uid() != unsafe { libc::getuid() } {
        bail!("a process of another user connected");
    }
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    BufReader::new(reader.take(MAX_REQUEST))
        .read_line(&mut line)
        .await?;
    let envelope = match serde_json::from_str::<Envelope>(&line) {
        Ok(envelope) => envelope,
        Err(e) => {
            let error = json!({"error": format!("killi {} doesn't understand this request: {e}", env!("CARGO_PKG_VERSION"))});
            writer.write_all(format!("{error}\n").as_bytes()).await?;
            return Ok(());
        }
    };
    // A `Follow` answers with the task's events as they happen, so the connection stays open
    // and ends with the task; every other request takes one reply.
    if matches!(envelope.request, Request::Follow { .. }) {
        let (events, mut lines) = mpsc::unbounded_channel::<String>();
        if session
            .send(Incoming {
                request: envelope.request,
                reply: Reply::Stream(events),
            })
            .is_err()
        {
            let error = json!({"error": "the session is ending"});
            writer.write_all(format!("{error}\n").as_bytes()).await?;
            return Ok(());
        }
        while let Some(event) = lines.recv().await {
            if writer
                .write_all(format!("{event}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
        return Ok(());
    }
    let (reply, answer) = oneshot::channel();
    let answered = match session.send(Incoming {
        request: envelope.request,
        reply: Reply::Once(reply),
    }) {
        Ok(()) => answer
            .await
            .unwrap_or_else(|_| json!({"error": "the session ended before it answered"})),
        Err(_) => json!({"error": "the session is ending"}),
    };
    writer
        .write_all(format!("{answered}\n").as_bytes())
        .await?;
    Ok(())
}

/// Sends `request` to the session `to` and gives back its reply.
pub async fn send(to: &Entry, request: Request) -> Result<Value> {
    let socket = to.socket.as_ref().ok_or_else(|| {
        anyhow!(
            "session {} runs headless (-p) and takes no questions, messages or tasks",
            to.pid
        )
    })?;
    let wait = match &request {
        Request::Ask { .. } => ASK_TIMEOUT,
        // A task answers once it ends, so its own limit plus the slack the session needs to
        // say it ran out of time.
        Request::Send { quick: false, timeout, .. } => Duration::from_secs(
            timeout
                .unwrap_or(TASK_TIMEOUT)
                .min(MAX_TASK_TIMEOUT)
                .saturating_add(TASK_SLACK),
        ),
        _ => QUICK_TIMEOUT,
    };
    let exchange = async {
        let mut stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("could not reach session {}", to.pid))?;
        stream
            .write_all(
                format!("{}\n", serde_json::to_string(&Envelope { v: 2, request })?).as_bytes(),
            )
            .await?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await?;
        serde_json::from_str::<Value>(&line)
            .context("the session closed the connection without answering")
    };
    let reply = tokio::time::timeout(wait, exchange).await.map_err(|_| {
        anyhow!(
            "session {} didn't answer within {} s",
            to.pid,
            wait.as_secs()
        )
    })??;
    if let Some(error) = reply["error"].as_str() {
        bail!("{error}");
    }
    Ok(reply)
}

/// Sends `request` to the session `to` and gives back the event lines its reply streams, for a
/// `Follow`: the connection stays open until the task ends, and the channel closes with it.
pub async fn follow(to: &Entry, request: Request) -> Result<mpsc::UnboundedReceiver<String>> {
    let socket = to
        .socket
        .as_ref()
        .ok_or_else(|| anyhow!("session {} runs headless (-p) and takes no tasks", to.pid))?;
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("could not reach session {}", to.pid))?;
    stream
        .write_all(
            format!("{}\n", serde_json::to_string(&Envelope { v: 2, request })?).as_bytes(),
        )
        .await?;
    let (lines, read) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await? == 0 {
                break;
            }
            if lines.send(line.clone()).is_err() {
                break;
            }
        }
        Ok::<(), std::io::Error>(())
    });
    Ok(read)
}

/// What `entry` says it is doing now, or none when it runs headless and can't be asked.
pub(super) async fn status_of(entry: &Entry) -> Result<Option<Value>> {
    if entry.socket.is_none() {
        return Ok(None);
    }
    send(entry, Request::Status).await.map(Some)
}
