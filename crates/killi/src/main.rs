mod agent;
mod awake;
mod broker;
mod config;
mod crash;
mod fs_atomic;
mod goal;
mod hooks;
mod later;
mod live;
mod llm;
mod login;
mod mcp;
mod memory;
mod mode;
mod plan;
mod printer;
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
mod verbosity;

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Instant;

use anyhow::{Result, bail};
use serde_json::json;
use tokio::sync::Notify;

use agent::{Controls, Sink, Stop};
use printer::Printer;
use setup::Setup;

const USAGE: &str = "\
killi — a minimal coding agent

Usage:
  killi [-m <provider/model>]                  Start the interactive session
  killi <prompt> [-m <model>] [-c]            Start interactive and send the first prompt
  killi -c | --continue                        Continue the latest session in this folder
  killi -r | --resume [<log>]                  Pick an earlier session (or open a log) and continue it
  killi -p <prompt> [-m <model>] [-c] [--json] Run one task, print the answer and exit
  killi init                                   Write a starting ~/.killi/config.toml, or add [prompt] to yours
  killi import [tool] [--force]                Copy providers, models and MCP servers from another agent
  killi login [<service>]                      Sign in to chatgpt, opencode-go, muse-code, command-code or grok, adding its models
  killi logout [<service>]                     Sign out of one
  killi update                                 Install the latest published version
  killi models sync [<provider>]               Refresh OpenCode Go and Z.ai model lists
  killi sessions [<pid>] [--json]              The killi sessions running on this machine, or what one is doing
  killi ask <pid> <question>                   Ask another session about its work; its model answers
  killi tell <pid> <message>                   Leave another session a message for its next step
  killi card <pid> [--json]                    What another session can be asked to do, and what it is doing now
  killi send <pid> <task> [--async] [--timeout <s>] [--after <id>] Hand another session a task; it answers with a report
  killi tasks [<pid>] [--json]       The tasks the sessions have taken
  killi task <id> [--json] [--follow]  What a task is doing, and its report once it ends
  killi cancel <id>                  Stop a task that is still running
  killi --version

Config: ~/.killi/config.toml";

#[derive(Default)]
struct Options {
    prompt: Option<String>,
    /// A prompt to send as the first turn of an interactive session, from a positional
    /// argument or `--prompt`: what agent managers (Orca et al.) pass when they launch killi.
    initial_prompt: Option<String>,
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
        Err(error) if error.is::<printer::Closed>() => ExitCode::from(printer::CLOSED_EXIT),
        Err(error) => {
            printer::err(&format!("killi: {error:#}\n"));
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(args: Vec<String>) -> Result<ExitCode> {
    match args.first().map(String::as_str) {
        Some("init") => config::init().map(|()| ExitCode::SUCCESS),
        Some("import") => config::import_command(&args[1..]).map(|()| ExitCode::SUCCESS),
        // The original import command, kept as an alias.
        Some("import-kimi") => {
            let mut owned = vec!["kimi".to_string()];
            owned.extend(args.iter().skip(1).cloned());
            config::import_command(&owned).map(|()| ExitCode::SUCCESS)
        }
        Some("update") => update::update().await.map(|()| ExitCode::SUCCESS),
        Some("models") => {
            let only = match args.get(1).map(String::as_str) {
                Some("sync") => args.get(2).map(String::as_str),
                Some(name) => Some(name),
                None => None,
            };
            for line in config::sync_models(only).await? {
                printer::say(line)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Some("login") => login::login(args.get(1).map(String::as_str))
            .await
            .map(|()| ExitCode::SUCCESS),
        Some("logout") => login::logout(args.get(1).map(String::as_str))
            .await
            .map(|()| ExitCode::SUCCESS),
        Some("mcp-serve") => broker::serve(&broker::socket_path())
            .await
            .map(|()| ExitCode::SUCCESS),
        Some("sessions" | "ask" | "tell" | "card" | "send" | "tasks" | "task" | "cancel") => {
            live::command(&args).await
        }
        Some("-V" | "--version") => {
            printer::say(format_args!("killi {}", env!("CARGO_PKG_VERSION")))?;
            Ok(ExitCode::SUCCESS)
        }
        Some("-h" | "--help") => {
            printer::say(USAGE)?;
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            let options = parse(args)?;
            // The first-run guides read stdin with the terminal's own Ctrl+C and end of
            // input, before the signal handlers below replace them.
            if options.prompt.is_none() && options.initial_prompt.is_none() {
                config::start_if_missing()?;
                login::chatgpt::offer(options.model.as_deref()).await?;
                login::commandcode_offer(options.model.as_deref()).await?;
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
                            initial_prompt: options.initial_prompt,
                        },
                    )
                    .await
                }
            }
        }
    }
}

thread_local! {
    /// The one-shot run in progress, which Ctrl+C ends before the process: its request in
    /// flight logs an `abort` and the text it held back is printed.
    static ONE_SHOT: RefCell<Option<Rc<Notify>>> = const { RefCell::new(None) };
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
        // The runtime has one thread, so this is the run's own.
        let ended = ONE_SHOT.with(|run| run.borrow_mut().take().map(|run| run.notify_one()));
        if ended.is_some() {
            // The run ends the process; a second Ctrl+C doesn't wait for it.
            interrupt.recv().await;
        }
        quit(130, "SIGINT");
    });
}

/// Ends what killi started, notes `signal` in the session log, gives the terminal back and
/// exits with `code`.
fn quit(code: i32, signal: &str) -> ! {
    tools::kill_all();
    live::leave();
    crash::note_signal(signal);
    tui::restore_terminal();
    std::process::exit(code);
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
        if byte[0] == libc::SIGHUP as u8 {
            quit(129, "SIGHUP")
        } else {
            quit(143, "SIGTERM")
        }
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
            "--prompt" => {
                options.initial_prompt = Some(
                    args.next()
                        .filter(|p| !p.trim().is_empty())
                        .ok_or_else(|| anyhow::anyhow!("--prompt needs a prompt"))?,
                )
            }
            "--json" => options.json = true,
            "-c" | "--continue" => options.continue_last = true,
            "-r" | "--resume" => {
                let path = args
                    .next_if(|next| !next.starts_with('-'))
                    .map(PathBuf::from);
                options.resume = Some(path);
            }
            other if !other.starts_with('-') => {
                let word = other.trim();
                if word.is_empty() {
                    continue;
                }
                match &mut options.initial_prompt {
                    Some(prompt) => {
                        prompt.push(' ');
                        prompt.push_str(word);
                    }
                    slot @ None => *slot = Some(word.to_string()),
                }
            }
            other => bail!("unknown argument `{other}`\n\n{USAGE}"),
        }
    }
    if options.prompt.is_some() && options.initial_prompt.is_some() {
        bail!("use either -p or a positional prompt, not both\n\n{USAGE}");
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
    let setup = Setup::new(config, &cwd, model.as_deref(), false).await?;
    // Ends the run early: Ctrl+C, or a stdout whose reader went away.
    let halt = Rc::new(Notify::new());
    let printer = Rc::new(RefCell::new(Printer::new(&cwd, halt.clone())));
    let sink: Sink = if json_mode {
        Rc::new(|_| {})
    } else {
        let printer = printer.clone();
        Rc::new(move |event| printer.borrow_mut().event(event))
    };
    let mut controls = Controls::default();
    controls.swarm.set(setup.config().agent.swarm_on());
    controls.verbosity.set(setup.config().agent.verbosity());
    controls.mode.set(setup.config().agent.mode());
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
        let mut p = printer.borrow_mut();
        let info = setup.describe();
        let info = if info.is_empty() {
            String::new()
        } else {
            format!(" · {info}")
        };
        let line = format!(
            "{}killi {} · {}{info}{}\n",
            p.dim,
            env!("CARGO_PKG_VERSION"),
            agent.model_alias(),
            p.reset
        );
        p.note(&line);
    }

    let started = Instant::now();
    ONE_SHOT.with(|run| *run.borrow_mut() = Some(halt.clone()));
    let result = tokio::select! {
        result = agent.run(&prompt, &[]) => Some(result),
        () = halt.notified() => None,
    };
    ONE_SHOT.with(|run| run.borrow_mut().take());
    let closed = printer.borrow().closed();
    let result = match result {
        Some(result) => result,
        // Ctrl+C: the turn was dropped, so its request logged an `abort`; what the reply
        // had so far is printed before the process ends.
        None if !closed => {
            tools::kill_all();
            printer.borrow_mut().finish();
            quit(130, "SIGINT");
        }
        None => Err(printer::Closed.into()),
    };
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
        Some(Stop::StepLimit) => "step_limit",
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
            "memory": setup.memory(),
            "duration_ms": elapsed.as_millis() as u64,
            "stopped": stopped,
            "final_text": text,
        });
        if printer::say(summary).is_err() {
            return Ok(ExitCode::from(printer::CLOSED_EXIT));
        }
    } else {
        let mut p = printer.borrow_mut();
        p.finish();
        if let Some(error) = &error {
            let line = format!("{}killi: {error}{}", p.red, p.reset);
            p.note(&line);
        }
        if let Some(note) = tui::stopped_note(&stopped) {
            let line = format!("{}{note}{}", p.dim, p.reset);
            p.note(&line);
        }
        let usage = &stats.usage;
        let cached = (usage.cached * 100).checked_div(usage.prompt).unwrap_or(0);
        let line = format!(
            "\n{}{stop_reason} · {} · {} · {} tokens in, {cached}% cached · {} out · {:.1}s{}",
            p.dim,
            tools::count(stats.requests as usize, "request"),
            tools::count(stats.tool_calls as usize, "tool call"),
            usage.prompt,
            usage.completion,
            elapsed.as_secs_f32(),
            p.reset
        );
        p.note(&line);
        if p.closed() {
            return Ok(ExitCode::from(printer::CLOSED_EXIT));
        }
    }
    Ok(if stop == Some(Stop::Completed) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests;
