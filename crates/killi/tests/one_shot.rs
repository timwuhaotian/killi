//! What `killi -p` writes, run as the built binary against local providers: stdout carries
//! only the reply that finished, and a reader that stops early (`killi -p … | head -1`) ends
//! the run quietly, with the code a process killed by SIGPIPE has, never with a panic.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A scratch folder, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("killi-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An OpenAI-compatible provider that streams `events` as its answer to every request;
/// returns its base URL.
fn provider(events: Vec<serde_json::Value>) -> String {
    serve(events, None)
}

/// `provider`, but the answer never ends: the connection stays open after `events`, and
/// `sent` hears when they were written.
fn serve(events: Vec<serde_json::Value>, sent: Option<mpsc::Sender<TcpStream>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap_or(0);
                }
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let mut reply = String::from(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            );
            for event in &events {
                reply.push_str(&format!("data: {event}\n\n"));
            }
            let _ = stream.write_all(reply.as_bytes());
            if let Some(sent) = &sent {
                let _ = sent.send(stream);
            }
        }
    });
    url
}

/// A streamed piece of the reply.
fn text(content: &str) -> serde_json::Value {
    serde_json::json!({"choices": [{"index": 0, "delta": {"content": content}, "finish_reason": null}]})
}

/// A reply streamed in full: `content`, its end, its usage.
fn answer(content: &str) -> Vec<serde_json::Value> {
    vec![
        text(content),
        serde_json::json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        serde_json::json!({"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 10}}),
        serde_json::json!("[DONE]"),
    ]
}

/// `lines` lines of text: more than a pipe holds.
fn long_answer(lines: usize) -> Vec<serde_json::Value> {
    answer(
        &(0..lines)
            .map(|i| format!("line {i} of the answer\n"))
            .collect::<String>(),
    )
}

/// A reply cut by an error the provider won't retry: `content` streamed first.
fn cut(content: &str) -> Vec<serde_json::Value> {
    vec![
        text(content),
        serde_json::json!({"error": {"code": "usage_limit", "message": "you are out of credit"}}),
    ]
}

/// `killi -p` under a scratch HOME whose model is `url`'s, with `backup`'s as its backup,
/// its stdout and stderr piped.
fn one_shot(home: &Path, url: &str, backup: Option<&str>) -> Child {
    let config = home.join(".killi");
    std::fs::create_dir_all(&config).unwrap();
    let model = |name: &str, url: &str| {
        format!(
            "[providers.{name}]\ntype = \"openai\"\nbase_url = \"{url}\"\napi_key = \"x\"\n\
             [models.\"{name}/m\"]\nprovider = \"{name}\"\nmodel = \"m\"\n\
             max_context_size = 200000\ncapabilities = [\"tool_use\"]\n"
        )
    };
    let mut toml = String::from("default_model = \"main/m\"\n");
    if backup.is_some() {
        toml.push_str("backup_model = \"spare/m\"\n");
    }
    toml.push_str(&model("main", url));
    if let Some(backup) = backup {
        toml.push_str(&model("spare", backup));
    }
    std::fs::write(config.join("config.toml"), toml).unwrap();
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();
    Command::new(env!("CARGO_BIN_EXE_killi"))
        .args(["-p", "say something long"])
        .current_dir(&project)
        .env("HOME", home)
        .env("KILLI_NO_UPDATE_CHECK", "1")
        .env_remove("KILLI_PID")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("killi -p didn't end");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_reader_that_stops_after_one_line_ends_the_run_without_a_panic() {
    let scratch = Scratch::new("pipe-head");
    let url = provider(long_answer(40_000));
    let mut child = one_shot(&scratch.0, &url, None);
    let stderr = child.stderr.take().unwrap();
    let drained = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut text);
        text
    });
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    assert_eq!(first, "line 0 of the answer\n");
    let status = wait(&mut child);
    let stderr = drained.join().unwrap();
    assert_eq!(status.signal(), None, "{status:?}: {stderr}");
    assert_eq!(status.code(), Some(141), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn closed_stdout_and_stderr_end_the_run_without_a_panic() {
    let scratch = Scratch::new("pipe-closed");
    let url = provider(long_answer(40_000));
    let mut child = one_shot(&scratch.0, &url, None);
    drop(child.stdout.take());
    drop(child.stderr.take());
    let status = wait(&mut child);
    assert_eq!(status.signal(), None, "{status:?}");
    assert_eq!(status.code(), Some(141), "{status:?}");
}

/// Runs `child` to its end: its exit, stdout and stderr.
fn finished(child: Child) -> (ExitStatus, String, String) {
    let output = child.wait_with_output().unwrap();
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn a_backup_after_a_cut_reply_prints_only_the_backup_reply() {
    let scratch = Scratch::new("discard-backup");
    let primary = provider(cut("The primary began to"));
    let backup = provider(answer("The backup's whole answer."));
    let child = one_shot(&scratch.0, &primary, Some(&backup));
    let (status, stdout, stderr) = finished(child);
    assert!(status.success(), "{status:?}: {stderr}");
    assert_eq!(stdout, "The backup's whole answer.\n", "{stderr}");
    assert!(
        stderr.contains("the partial reply was discarded"),
        "{stderr}"
    );
    assert!(stderr.contains("switched to its backup"), "{stderr}");
}

#[test]
fn a_cut_reply_with_no_backup_prints_nothing_on_stdout() {
    let scratch = Scratch::new("discard-error");
    let primary = provider(cut("The primary began to"));
    let child = one_shot(&scratch.0, &primary, None);
    let (status, stdout, stderr) = finished(child);
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert_eq!(stdout, "", "{stderr}");
    assert!(
        stderr.contains("the partial reply was discarded"),
        "{stderr}"
    );
}

/// Starts a run whose reply never ends, sends it `signal` once part of the reply arrived, and
/// returns its exit, stdout and stderr, and its session log's records.
fn signaled(
    name: &str,
    signal: libc::c_int,
) -> (ExitStatus, String, String, Vec<serde_json::Value>) {
    let scratch = Scratch::new(name);
    let (sent, written) = mpsc::channel();
    let primary = serve(vec![text("Half of an answer")], Some(sent));
    let child = one_shot(&scratch.0, &primary, None);
    let _open = written.recv_timeout(Duration::from_secs(30)).unwrap();
    // The text is on the wire; give killi a moment to read it.
    std::thread::sleep(Duration::from_millis(300));
    // SAFETY: signals a child this test started and still owns.
    unsafe { libc::kill(child.id() as libc::pid_t, signal) };
    let (status, stdout, stderr) = finished(child);
    let sessions = scratch.0.join(".killi/sessions");
    let log = std::fs::read_dir(&sessions)
        .unwrap()
        .flatten()
        .flat_map(|dir| {
            std::fs::read_dir(dir.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .unwrap_or_else(|| panic!("no session log under {}", sessions.display()));
    let records = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    (status, stdout, stderr, records)
}

fn kinds(records: &[serde_json::Value]) -> Vec<&str> {
    records.iter().filter_map(|r| r["type"].as_str()).collect()
}

#[test]
fn ctrl_c_prints_what_the_reply_had_and_logs_why_it_ended() {
    let (status, stdout, stderr, records) = signaled("interrupt", libc::SIGINT);
    assert_eq!(status.code(), Some(130), "{status:?}: {stderr}");
    assert_eq!(stdout, "Half of an answer\n", "{stderr}");
    let kinds = kinds(&records);
    assert!(
        kinds.contains(&"abort"),
        "the request in flight is logged: {kinds:?}"
    );
    assert_eq!(kinds.last(), Some(&"signal"), "{kinds:?}");
    assert_eq!(records.last().unwrap()["signal"], "SIGINT");
}

#[test]
fn a_terminate_signal_is_logged_before_the_run_quits() {
    let (status, _, stderr, records) = signaled("terminate", libc::SIGTERM);
    assert_eq!(status.code(), Some(143), "{status:?}: {stderr}");
    assert_eq!(kinds(&records).last(), Some(&"signal"), "{records:?}");
    assert_eq!(records.last().unwrap()["signal"], "SIGTERM");
}
