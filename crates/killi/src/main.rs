mod agent;
mod broker;
mod config;
mod fs_atomic;
mod goal;
mod hooks;
mod later;
mod live;
mod llm;
mod login;
mod mcp;
mod plan;
mod prompt;
mod session;
mod setup;
mod skills;
mod snapshot;
mod swarm;
#[cfg(test)]
mod test_temp;
mod tools;
mod tui;
mod update;

use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Instant;

use anyhow::{Result, bail};
use serde_json::json;

use agent::{Controls, Event, Sink, Stop};
use setup::Setup;

const USAGE: &str = "\
killi — a minimal coding agent

Usage:
  killi [-m <provider/model>]                  Start the interactive session
  killi -c | --continue                        Continue the latest session in this folder
  killi -r | --resume [<log>]                  Pick an earlier session (or open a log) and continue it
  killi -p <prompt> [-m <model>] [-c] [--json] Run one task, print the answer and exit
  killi init                                   Write a starting ~/.killi/config.toml, or add [prompt] to yours
  killi import-kimi [--force]                  Copy providers, models and MCP servers from Kimi Code
  killi login [chatgpt | opencode-go]          Sign in to a subscription and add its models
  killi logout [chatgpt | opencode-go]         Sign out of one
  killi update                                 Install the latest published version
  killi sessions [<pid>] [--json]              The killi sessions running on this machine, or what one is doing
  killi ask <pid> <question>                   Ask another session about its work; its model answers
  killi tell <pid> <message>                   Leave another session a message for its next step
  killi --version

Config: ~/.killi/config.toml";

#[derive(Default)]
struct Options {
    prompt: Option<String>,
    model: Option<String>,
    json: bool,
    continue_last: bool,
    resume: Option<Option<PathBuf>>,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match runtime.block_on(dispatch(args)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("killi: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(args: Vec<String>) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        Some("init") => config::init().map(|()| ExitCode::SUCCESS),
        Some("import-kimi") => {
            config::import_kimi(args.iter().any(|a| a == "--force")).map(|()| ExitCode::SUCCESS)
        }
        Some("update") => update::update().await.map(|()| ExitCode::SUCCESS),
        Some("login") => login::login(args.get(1).map(String::as_str))
            .await
            .map(|()| ExitCode::SUCCESS),
        Some("logout") => login::logout(args.get(1).map(String::as_str))
            .await
            .map(|()| ExitCode::SUCCESS),
        Some("mcp-serve") => broker::serve(&broker::socket_path())
            .await
            .map(|()| ExitCode::SUCCESS),
        Some("sessions" | "ask" | "tell") => live::command(&args).await,
        Some("-V" | "--version") => {
            println!("killi {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            let options = parse(args)?;
            // The first-run guides read stdin with the terminal's own Ctrl+C and end of
            // input, before the signal handlers below replace them.
            if options.prompt.is_none() {
                config::set_up_if_missing()?;
                config::guide_model(options.model.as_deref())?;
                login::chatgpt::offer(options.model.as_deref()).await?;
            }
            quit_on_signals();
            let config = config::Config::load()?;
            let cwd = std::env::current_dir()?;
            let last = || newest_available(session::list(&cwd));
            let resume = match (&options.resume, options.continue_last) {
                (Some(Some(path)), _) => Some(path.clone()),
                (_, true) => Some(last().ok_or_else(|| {
                    anyhow::anyhow!("no earlier session in this folder to continue")
                })?),
                _ => None,
            };
            match options.prompt {
                Some(prompt) => {
                    run_once(config, &options.model, resume, prompt, options.json).await
                }
                None => {
                    let pick_session = matches!(options.resume, Some(None));
                    tui::run(
                        config,
                        tui::Start {
                            model: options.model,
                            resume,
                            pick_session,
                        },
                    )
                    .await
                }
            }
        }
    }
}

/// Quitting on a signal runs no destructors, so this first ends what killi started and gives
/// the terminal back: closing the window sends SIGHUP, `kill` SIGTERM, Ctrl+C under `-p` SIGINT.
fn quit_on_signals() {
    quit_on_hangup_or_terminate();
    // SIGINT stays on the runtime: nothing blocks its thread while `-p` runs.
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut interrupt) = signal(SignalKind::interrupt()) else {
            return;
        };
        interrupt.recv().await;
        tools::kill_all();
        live::leave();
        tui::restore_terminal();
        std::process::exit(130);
    });
}

/// The write end of the pipe a hangup/terminate handler pokes.
static QUIT_PIPE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_quit_signal(signal: libc::c_int) {
    let fd = QUIT_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: a one-byte write to a pipe this process opened; async-signal-safe.
        unsafe { libc::write(fd, [signal as u8].as_ptr().cast(), 1) };
    }
}

/// SIGHUP and SIGTERM arrive on a dedicated thread with its own pipe, independent of Tokio:
/// an async handler can't run while a blocking read (startup, a tool, the TUI) holds the
/// runtime's only thread. The handler only writes the signal's number; the thread ends what
/// killi started, gives the terminal back, and exits with the signal's usual code.
fn quit_on_hangup_or_terminate() {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe fills the two descriptors.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    let [reader, writer] = fds;
    // SAFETY: acts on a descriptor this function owns.
    unsafe { libc::fcntl(writer, libc::F_SETFL, libc::O_NONBLOCK) };
    QUIT_PIPE.store(writer, Ordering::Relaxed);
    // SAFETY: a zeroed sigaction is valid; the handler only writes a byte to the pipe.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_quit_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGHUP, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
    }
    std::thread::spawn(move || {
        let mut byte = [0u8; 1];
        // SAFETY: reads into a buffer of the given length.
        let n = loop {
            let n = unsafe { libc::read(reader, byte.as_mut_ptr().cast(), byte.len()) };
            if n >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break n;
            }
        };
        if n != 1 {
            return;
        }
        let code = if byte[0] == libc::SIGHUP as u8 {
            129
        } else {
            143
        };
        tools::kill_all();
        live::leave();
        tui::restore_terminal();
        std::process::exit(code);
    });
}

fn parse(args: Vec<String>) -> Result<Options> {
    let mut options = Options::default();
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" | "--print" => {
                options.prompt = Some(
                    args.next()
                        .filter(|p| !p.trim().is_empty())
                        .ok_or_else(|| anyhow::anyhow!("-p needs a prompt"))?,
                )
            }
            "-m" | "--model" => options.model = args.next(),
            "--json" => options.json = true,
            "-c" | "--continue" => options.continue_last = true,
            "-r" | "--resume" => {
                let path = args
                    .next_if(|next| !next.starts_with('-'))
                    .map(PathBuf::from);
                options.resume = Some(path);
            }
            other => bail!("unknown argument `{other}`\n\n{USAGE}"),
        }
    }
    Ok(options)
}

/// The newest log no live session holds: `-c` continues it, skipping the ones that are
/// still running. A log nobody holds may still belong to a session that ended.
fn newest_available(sessions: Vec<session::Listed>) -> Option<PathBuf> {
    sessions
        .into_iter()
        .find(|s| !session::is_locked(&s.path))
        .map(|s| s.path)
}

async fn run_once(
    config: config::Config,
    model: &Option<String>,
    resume: Option<PathBuf>,
    prompt: String,
    json_mode: bool,
) -> Result<ExitCode> {
    // A panic under `-p` has no TUI to install the crash hook: install it here.
    tui::install_crash_hook();
    let cwd = std::env::current_dir()?;
    let setup = Setup::new(config, &cwd, model.as_deref()).await?;
    let printer = Rc::new(RefCell::new(Printer::new(&cwd)));
    let sink: Sink = if json_mode {
        Rc::new(|_| {})
    } else {
        let printer = printer.clone();
        Rc::new(move |event| printer.borrow_mut().event(event))
    };
    let mut controls = Controls::default();
    controls.swarm.set(setup.config().agent.swarm_on());
    // A headless run is listed, so sessions in its tree leave its work alone, but takes no
    // questions or messages: nothing would read them while it runs.
    let mut live = setup
        .config()
        .agent
        .peers_on()
        .then(|| live::join(&cwd, false))
        .flatten()
        .map(|(registration, _)| registration);
    controls.here = live.as_ref().map(|l| Rc::new(l.here()));
    let mut agent = match &resume {
        Some(log) => setup.resume(log, controls, sink)?,
        None => setup.start(controls, sink)?,
    };
    if let Some(live) = &mut live {
        live.update(|e| {
            e.model = agent.model_alias().to_string();
            e.log = agent.session.log_path.clone();
            e.working = true;
            e.prompt =
                crate::llm::truncate(prompt.lines().next().unwrap_or_default(), 200).to_string();
        });
    }
    if !json_mode {
        let p = printer.borrow();
        let info = setup.describe();
        let info = if info.is_empty() {
            String::new()
        } else {
            format!(" · {info}")
        };
        eprintln!(
            "{}killi {} · {}{info}{}\n",
            p.dim,
            env!("CARGO_PKG_VERSION"),
            agent.model_alias(),
            p.reset
        );
    }

    let started = Instant::now();
    let result = agent.run(&prompt, &[]).await;
    let stopped = agent.stop_tasks();
    let stats = &agent.stats;
    let elapsed = started.elapsed();

    let (text, stop, error) = match result {
        Ok((text, stop)) => (text, Some(stop), None),
        Err(error) => (String::new(), None, Some(format!("{error:#}"))),
    };
    let stop_reason = match stop {
        Some(Stop::Completed) => "completed",
        Some(Stop::MaxTokens) => "max_tokens",
        Some(Stop::RepeatBreaker) => "repeat_breaker",
        None => "error",
    };
    agent.session_end(stop_reason).await;
    if json_mode {
        let summary = json!({
            "stop_reason": stop_reason,
            "error": error,
            "model": agent.model_alias(),
            "session_id": agent.session.id,
            "session_log": agent.session.log_path,
            "requests": stats.requests,
            "tool_calls": stats.tool_calls,
            "prompt_tokens": stats.usage.prompt,
            "cached_tokens": stats.usage.cached,
            "completion_tokens": stats.usage.completion,
            "reasoning_tokens": stats.usage.reasoning,
            "first_prompt_tokens": stats.first_prompt,
            "instruction_tokens": setup.instruction_tokens(),
            "instruction_files": setup.instruction_files(),
            "duration_ms": elapsed.as_millis() as u64,
            "stopped": stopped,
            "final_text": text,
        });
        println!("{summary}");
    } else {
        let mut p = printer.borrow_mut();
        p.finish();
        if let Some(error) = &error {
            eprintln!("{}killi: {error}{}", p.red, p.reset);
        }
        if let Some(note) = tui::stopped_note(&stopped) {
            eprintln!("{}{note}{}", p.dim, p.reset);
        }
        let usage = &stats.usage;
        let cached = (usage.cached * 100).checked_div(usage.prompt).unwrap_or(0);
        eprintln!(
            "\n{}{stop_reason} · {} · {} · {} tokens in, {cached}% cached · {} out · {:.1}s{}",
            p.dim,
            tools::count(stats.requests as usize, "request"),
            tools::count(stats.tool_calls as usize, "tool call"),
            usage.prompt,
            usage.completion,
            elapsed.as_secs_f32(),
            p.reset
        );
    }
    Ok(if stop == Some(Stop::Completed) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[derive(PartialEq)]
enum Last {
    Nothing,
    Text,
    Tool,
}

/// Prints a one-shot run as compact lines: the model's text on stdout, one line per tool call
/// on stderr, a blank line between the two. Colors only when stderr is a terminal.
struct Printer {
    cwd: String,
    last: Last,
    /// The current attempt's text, held back so a retried attempt never reaches stdout: only
    /// the final reply is printed, when the attempt ends.
    attempt: String,
    /// The model's text is mid-line in the attempt.
    line_open: bool,
    /// Newlines at the end of the text so far, printed only if more text follows.
    held_newlines: usize,
    /// A tool line is waiting for its end mark on stderr.
    tool_open: bool,
    dim: &'static str,
    accent: &'static str,
    red: &'static str,
    reset: &'static str,
}

impl Printer {
    fn new(cwd: &std::path::Path) -> Self {
        use std::io::IsTerminal;
        let color = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        let pick = |code: &'static str| if color { code } else { "" };
        Self {
            cwd: cwd.display().to_string(),
            last: Last::Nothing,
            attempt: String::new(),
            line_open: false,
            held_newlines: 0,
            tool_open: false,
            dim: pick("\x1b[2m"),
            accent: pick("\x1b[34m"),
            red: pick("\x1b[31m"),
            reset: pick("\x1b[0m"),
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Text(text) => {
                let text = if self.last == Last::Text {
                    text.as_str()
                } else {
                    text.trim_start()
                };
                let body = text.trim_end_matches('\n');
                if body.is_empty() {
                    if self.last == Last::Text {
                        self.held_newlines += text.len();
                    }
                    return;
                }
                if self.last == Last::Tool {
                    self.attempt.push('\n');
                }
                self.attempt.push_str(&"\n".repeat(self.held_newlines));
                self.attempt.push_str(body);
                self.held_newlines = text.len() - body.len();
                self.line_open = true;
                self.last = Last::Text;
            }
            Event::Retry {
                attempt,
                wait,
                reason,
            } => {
                let dropped = self.line_open || self.held_newlines > 0 || !self.attempt.is_empty();
                self.attempt.clear();
                self.line_open = false;
                self.held_newlines = 0;
                eprintln!(
                    "{}retry {attempt} in {:.1}s: {}{}",
                    self.dim,
                    wait.as_secs_f32(),
                    llm::truncate(&reason, 300),
                    self.reset
                );
                if dropped {
                    eprintln!(
                        "{}the partial reply above was discarded; retrying …{}",
                        self.dim, self.reset
                    );
                }
            }
            Event::ToolStart { summary, .. } => {
                if self.last == Last::Text {
                    self.end_text();
                    println!();
                }
                let summary = summary
                    .replace(&format!("{}/", self.cwd), "")
                    .replace(&self.cwd, ".");
                eprint!("{}●{} {summary}", self.accent, self.reset);
                self.tool_open = true;
                self.last = Last::Tool;
            }
            Event::ToolEnd { is_error, .. } => {
                let mark = if is_error {
                    format!(" {}✗{}", self.red, self.reset)
                } else {
                    String::new()
                };
                if std::mem::take(&mut self.tool_open) || !mark.is_empty() {
                    eprintln!("{mark}");
                }
            }
            Event::Notice(text) => {
                self.end_text();
                eprintln!("{}{text}{}", self.dim, self.reset);
            }
            Event::Sub { label, event, .. } => {
                if matches!(
                    *event,
                    Event::ToolStart { .. }
                        | Event::ToolEnd { is_error: true, .. }
                        | Event::Finished { .. }
                ) && std::mem::take(&mut self.tool_open)
                {
                    eprintln!();
                }
                match *event {
                    Event::ToolStart { summary, .. } => {
                        eprintln!("  {}↳ {label}: {summary}{}", self.dim, self.reset)
                    }
                    Event::ToolEnd {
                        is_error: true,
                        brief,
                        ..
                    } => eprintln!("  {}↳ {label}: ✗ {brief}{}", self.red, self.reset),
                    Event::Finished { is_error } => eprintln!(
                        "  {}↳ {label}: {}{}",
                        self.dim,
                        if is_error { "failed" } else { "done" },
                        self.reset
                    ),
                    _ => {}
                }
            }
            Event::Reasoning(_)
            | Event::Usage(_)
            | Event::Todos(_)
            | Event::Ask(_)
            | Event::Swarm { .. }
            | Event::Started
            | Event::Finished { .. } => {}
        }
    }

    /// Prints the attempt's text so far.
    fn flush_attempt(&mut self) {
        if !self.attempt.is_empty() {
            print!("{}", self.attempt);
            let _ = std::io::stdout().flush();
            self.attempt.clear();
        }
    }

    /// Ends the model's text with exactly one newline, dropping any it held back.
    fn end_text(&mut self) {
        self.flush_attempt();
        if self.line_open {
            println!();
            self.line_open = false;
        }
        self.held_newlines = 0;
    }

    fn finish(&mut self) {
        self.end_text();
    }
}

#[cfg(test)]
mod tests;
