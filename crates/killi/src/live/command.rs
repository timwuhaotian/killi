//! The `killi` commands that reach the live sessions — `sessions`, `card`, `ask`, `tell`,
//! `send`, `tasks`, `task` and `cancel` — and the text they print.

use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{
    Entry, MAX_TASK_TIMEOUT, MAX_TEXT, PID_VAR, Request, card, card_text, describe, follow, list,
    now, send, span, status_of, task_owner,
};

/// `killi sessions [<pid>] [--json]`, `killi card <pid> [--json]`, `killi ask <pid> <question>`
/// and `killi tell <pid> <message>`.
pub async fn command(args: &[String]) -> Result<ExitCode> {
    let me = std::env::var(PID_VAR)
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok());
    let sessions = list();
    // The session with `pid`, without the check that refuses a session asking itself: a task
    // id names the session holding it, and reading your own task is not talking to yourself.
    let locate = |pid: u32| -> Result<Entry> {
        sessions
            .iter()
            .find(|e| e.pid == pid)
            .cloned()
            .ok_or_else(|| {
                anyhow!("no killi session with pid {pid} is running; `killi sessions` lists them")
            })
    };
    let find = |pid: Option<&String>| -> Result<Entry> {
        let pid: u32 = pid
            .and_then(|p| p.parse().ok())
            .ok_or_else(|| anyhow!("say which session, by its pid; `killi sessions` lists them"))?;
        if Some(pid) == me && args[0] != "sessions" {
            bail!("session {pid} is this session");
        }
        sessions
            .iter()
            .find(|e| e.pid == pid)
            .cloned()
            .ok_or_else(|| {
                anyhow!("no killi session with pid {pid} is running; `killi sessions` lists them")
            })
    };
    let text = || -> Result<String> {
        let text = args
            .get(2..)
            .unwrap_or_default()
            .join(" ")
            .trim()
            .to_string();
        if text.is_empty() {
            bail!("say what to send: killi {} <pid> \"…\"", args[0]);
        }
        if text.len() > MAX_TEXT {
            bail!("that is {} bytes; the limit is {MAX_TEXT}", text.len());
        }
        Ok(text)
    };
    match args[0].as_str() {
        "sessions" => {
            let json_mode = args.iter().any(|a| a == "--json");
            let pid = args.iter().skip(1).find(|a| !a.starts_with('-'));
            match pid {
                Some(pid) => {
                    let entry = find(Some(pid))?;
                    let status = status_of(&entry).await?;
                    if json_mode {
                        println!("{}", json!({"session": entry, "status": status}));
                    } else {
                        println!("{}", status_text(&entry, status.as_ref(), now()));
                    }
                }
                None if json_mode => println!("{}", serde_json::to_string(&sessions)?),
                None => println!("{}", list_text(&sessions, me, now())),
            }
        }
        "card" => {
            let json_mode = args.iter().any(|a| a == "--json");
            let entry = find(args.get(1))?;
            let status = status_of(&entry).await?;
            if json_mode {
                println!("{}", serde_json::to_string(&card(&entry, status.as_ref()))?);
            } else {
                println!("{}", card_text(&entry, status.as_ref(), now()));
            }
        }
        "ask" => {
            let to = find(args.get(1))?;
            let reply = send(
                &to,
                Request::Ask {
                    from: me,
                    text: text()?,
                },
            )
            .await?;
            println!("{}", reply["answer"].as_str().unwrap_or_default());
        }
        "tell" => {
            let to = find(args.get(1))?;
            let reply = send(
                &to,
                Request::Tell {
                    from: me,
                    text: text()?,
                },
            )
            .await?;
            println!("{}", reply["delivered"].as_str().unwrap_or("Delivered."));
        }
        "send" => {
            let to = find(args.get(1))?;
            let (text, quick, timeout, after) = send_args(args)?;
            let reply = send(
                &to,
                Request::Send {
                    from: me,
                    text,
                    timeout,
                    after,
                    quick,
                },
            )
            .await?;
            let id = reply["id"].as_str().unwrap_or_default();
            if quick {
                println!("{id}");
            } else if reply["state"].as_str() == Some("completed") {
                println!("{}", reply["report"].as_str().unwrap_or_default());
            } else {
                if let Some(report) = reply["report"].as_str() {
                    println!("{report}");
                }
                let why = reply["message"].as_str().unwrap_or("it gave no report");
                eprintln!(
                    "killi: task {id} {}: {why}",
                    reply["state"].as_str().unwrap_or("failed")
                );
                return Ok(ExitCode::FAILURE);
            }
        }
        "tasks" => {
            let json_mode = args.iter().any(|a| a == "--json");
            let named = args.iter().skip(1).find(|a| !a.starts_with('-'));
            // Reading a session's tasks is not talking to it, so this session's own are read
            // like any other's.
            let asked: Vec<Entry> = match named {
                Some(pid) => vec![locate(
                    pid.parse()
                        .map_err(|_| anyhow!("say which session, by its pid"))?,
                )?],
                None => sessions.clone(),
            };
            let mut tasks: Vec<Value> = Vec::new();
            for entry in &asked {
                match send(entry, Request::List).await {
                    Ok(reply) => tasks
                        .extend(reply["tasks"].as_array().cloned().unwrap_or_default()),
                    // A session of another release takes no tasks, or one that ended between
                    // the list and the ask: it simply has nothing to say.
                    Err(e) => {
                        if named.is_some() {
                            return Err(e);
                        }
                    }
                }
            }
            if json_mode {
                println!("{}", serde_json::to_string(&tasks)?);
            } else {
                println!("{}", tasks_text(&tasks, now()));
            }
        }
        "task" => {
            let json_mode = args.iter().any(|a| a == "--json");
            let watching = args.iter().any(|a| a == "--follow");
            let id = args
                .iter()
                .skip(1)
                .find(|a| !a.starts_with('-'))
                .ok_or_else(|| anyhow!("say which task, by its id; `killi tasks` lists them"))?;
            let entry = locate(task_owner(id).ok_or_else(|| anyhow!("`{id}` is not a task id"))?)?;
            if watching {
                let mut lines = follow(&entry, Request::Follow { id: id.clone() }).await?;
                let mut state = String::new();
                while let Some(event) = next_event(&mut lines).await? {
                    if json_mode {
                        println!("{event}");
                    } else {
                        println!("{}", event_text(&event));
                    }
                    state = event["state"].as_str().unwrap_or_default().to_string();
                }
                // The stream ends with the task, so its last event says how it ended; like
                // `killi send`, anything but a finished task is a failure.
                match state.as_str() {
                    "completed" => {}
                    "submitted" | "working" | "" => {
                        bail!("session {} stopped answering before task {id} ended", entry.pid)
                    }
                    _ => return Ok(ExitCode::FAILURE),
                }
            } else {
                let reply = send(&entry, Request::Get { id: id.clone() }).await?;
                if json_mode {
                    println!("{}", reply["task"]);
                } else {
                    println!("{}", task_detail(&reply["task"], now()));
                }
            }
        }
        "cancel" => {
            let id = args
                .get(1)
                .filter(|id| !id.starts_with('-'))
                .ok_or_else(|| anyhow!("say which task, by its id; `killi tasks` lists them"))?;
            let entry = locate(task_owner(id).ok_or_else(|| anyhow!("`{id}` is not a task id"))?)?;
            let reply = send(&entry, Request::Cancel { id: id.clone() }).await?;
            println!(
                "Task {id} {}.",
                reply["state"].as_str().unwrap_or("canceled")
            );
        }
        other => bail!("unknown command `{other}`"),
    }
    Ok(ExitCode::SUCCESS)
}

/// `killi send`'s own words: the task's text with its flags taken out, whether it only takes the
/// task's id back, how long the task may run, and the task it continues.
pub(super) fn send_args(args: &[String]) -> Result<(String, bool, Option<u64>, Option<String>)> {
    let mut words = args.iter().skip(2);
    let mut text: Vec<&str> = Vec::new();
    let mut quick = false;
    let mut timeout = None;
    let mut after = None;
    while let Some(word) = words.next() {
        match word.as_str() {
            "--async" => quick = true,
            "--timeout" => {
                let seconds = words
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|seconds| (1..=MAX_TASK_TIMEOUT).contains(seconds))
                    .ok_or_else(|| {
                        anyhow!("--timeout takes a number of seconds, from 1 to {MAX_TASK_TIMEOUT}")
                    })?;
                timeout = Some(seconds);
            }
            "--after" => {
                let id = words.next().ok_or_else(|| {
                    anyhow!("--after takes the id of the task to continue; `killi tasks` lists them")
                })?;
                after = Some(id.clone());
            }
            other => text.push(other),
        }
    }
    let text = text.join(" ").trim().to_string();
    if text.is_empty() {
        bail!("say what to send: killi send <pid> \"…\" [--async] [--timeout <s>] [--after <id>]");
    }
    if text.len() > MAX_TEXT {
        bail!("that is {} bytes; the limit is {MAX_TEXT}", text.len());
    }
    Ok((text, quick, timeout, after))
}

/// The next event of a task's stream, or none once it ended: the session closes the connection
/// with the task. It waits as long as the task takes between two steps — the session ends the
/// task when its time is up, and the connection closes if the session goes. A session that
/// can't follow tasks answers with an error instead of an event.
pub(super) async fn next_event(lines: &mut mpsc::UnboundedReceiver<String>) -> Result<Option<Value>> {
    let Some(line) = lines.recv().await else {
        return Ok(None);
    };
    let event: Value =
        serde_json::from_str(&line).context("the session sent an event killi can't read")?;
    if let Some(error) = event["error"].as_str() {
        bail!("{error}");
    }
    Ok(Some(event))
}

/// One event of a task's stream: its state and what it says about the step it is on or why it
/// ended, then its report once it ended.
pub(super) fn event_text(event: &Value) -> String {
    let state = event["state"].as_str().unwrap_or_default();
    let mut text = match event["message"].as_str() {
        Some(message) => format!("{state}: {message}"),
        None => state.to_string(),
    };
    if let Some(report) = event["report"].as_str() {
        text.push('\n');
        text.push_str(report);
    }
    text
}

/// `killi tasks`: one line per task, oldest first.
fn tasks_text(tasks: &[Value], now: u64) -> String {
    if tasks.is_empty() {
        return "No killi session has taken a task.".into();
    }
    tasks
        .iter()
        .map(|task| task_line(task, now))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A task in a line: its id, state, who sent it, how long ago, and what it asked for.
fn task_line(task: &Value, now: u64) -> String {
    let from = match task["from"].as_u64() {
        Some(pid) => format!("session {pid}"),
        None => "a shell".into(),
    };
    let ago = span(now.saturating_sub(task["since"].as_u64().unwrap_or_default()));
    format!(
        "{} · {} · from {from} · {ago} ago · \u{201c}{}\u{201d}",
        task["id"].as_str().unwrap_or_default(),
        task["state"].as_str().unwrap_or_default(),
        crate::llm::truncate(task["prompt"].as_str().unwrap_or_default(), 60)
    )
}

/// `killi task <id>`: the line from the list, then its report or what went wrong.
fn task_detail(task: &Value, now: u64) -> String {
    let mut text = task_line(task, now);
    for key in ["report", "message"] {
        if let Some(body) = task[key].as_str() {
            text.push_str(&format!("\n{body}"));
        }
    }
    if let Some(log) = task["log"].as_str() {
        text.push_str(&format!("\nLog: {log}"));
    }
    text
}

/// `killi sessions`: one line for each session, this one marked.
pub(super) fn list_text(sessions: &[Entry], me: Option<u32>, now: u64) -> String {
    if sessions.is_empty() {
        return "No killi session is running.".into();
    }
    sessions
        .iter()
        .map(|e| {
            let mut line = describe(e, now);
            if e.socket.is_none() {
                line.push_str(" · headless (-p)");
            }
            if Some(e.pid) == me {
                line.push_str(" · this session");
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `killi sessions <pid>`: the line from the list, then what it is doing now.
pub(super) fn status_text(entry: &Entry, status: Option<&Value>, now: u64) -> String {
    let mut text = format!(
        "{} · {} · killi {}",
        describe(entry, now),
        entry.model,
        entry.version
    );
    let Some(status) = status else {
        return text + "\nIt runs headless (-p) and can't say more.";
    };
    if status["plan"].as_bool() == Some(true) {
        text.push_str("\nIn plan mode.");
    }
    let todos = status["todos"].as_array().cloned().unwrap_or_default();
    if !todos.is_empty() {
        let done = todos.iter().filter(|t| t["status"] == "completed").count();
        text.push_str(&format!("\nTasks {done}/{}:", todos.len()));
        for todo in &todos {
            let mark = match todo["status"].as_str() {
                Some("completed") => "✓",
                Some("in_progress") => "◐",
                _ => "○",
            };
            text.push_str(&format!(
                "\n  {mark} {}",
                todo["content"].as_str().unwrap_or_default()
            ));
        }
    }
    if let Some(waiting) = status["waiting"].as_str() {
        text.push_str(&format!("\nWaiting on:\n{waiting}"));
    }
    text
}
