mod agent;
mod config;
mod hooks;
mod llm;
mod mcp;
mod plan;
mod prompt;
mod session;
mod setup;
mod skills;
mod snapshot;
mod tools;
mod tui;
mod update;

use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
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
  killi import-kimi [--force]                  Copy providers and models from Kimi Code
  killi update                                 Install the latest published version
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
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
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
        Some("import-kimi") => config::import_kimi(args.iter().any(|a| a == "--force")).map(|()| ExitCode::SUCCESS),
        Some("update") => update::update().await.map(|()| ExitCode::SUCCESS),
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
            let config = config::Config::load()?;
            let cwd = std::env::current_dir()?;
            let last = || session::list(&cwd).into_iter().next().map(|s| s.path);
            let resume = match (&options.resume, options.continue_last) {
                (Some(Some(path)), _) => Some(path.clone()),
                (_, true) => Some(last().ok_or_else(|| anyhow::anyhow!("no earlier session in this folder to continue"))?),
                _ => None,
            };
            match options.prompt {
                Some(prompt) => run_once(config, &options.model, resume, prompt, options.json).await,
                None => {
                    let pick_session = matches!(options.resume, Some(None));
                    tui::run(config, tui::Start { model: options.model, resume, pick_session }).await
                }
            }
        }
    }
}

fn parse(args: Vec<String>) -> Result<Options> {
    let mut options = Options::default();
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" | "--print" => options.prompt = Some(args.next().filter(|p| !p.trim().is_empty()).ok_or_else(|| anyhow::anyhow!("-p needs a prompt"))?),
            "-m" | "--model" => options.model = args.next(),
            "--json" => options.json = true,
            "-c" | "--continue" => options.continue_last = true,
            "-r" | "--resume" => {
                let path = args.next_if(|next| !next.starts_with('-')).map(PathBuf::from);
                options.resume = Some(path);
            }
            other => bail!("unknown argument `{other}`\n\n{USAGE}"),
        }
    }
    Ok(options)
}

async fn run_once(config: config::Config, model: &Option<String>, resume: Option<PathBuf>, prompt: String, json_mode: bool) -> Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    let setup = Setup::new(config, &cwd, model.as_deref()).await?;
    let printer = Rc::new(RefCell::new(Printer::new(&cwd)));
    let sink: Sink = if json_mode {
        Rc::new(|_| {})
    } else {
        let printer = printer.clone();
        Rc::new(move |event| printer.borrow_mut().event(event))
    };
    let controls = Controls::default();
    let mut agent = match &resume {
        Some(log) => setup.resume(log, controls, sink)?,
        None => setup.start(controls, sink)?,
    };
    if !json_mode {
        let p = printer.borrow();
        let info = setup.describe();
        let info = if info.is_empty() { String::new() } else { format!(" · {info}") };
        eprintln!("{}killi {} · {}{info}{}\n", p.dim, env!("CARGO_PKG_VERSION"), agent.model_alias(), p.reset);
    }

    let started = Instant::now();
    let result = agent.run(&prompt).await;
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
            "final_text": text,
        });
        println!("{summary}");
    } else {
        let mut p = printer.borrow_mut();
        p.finish();
        if let Some(error) = &error {
            eprintln!("{}killi: {error}{}", p.red, p.reset);
        }
        let usage = &stats.usage;
        let cached = (usage.cached * 100).checked_div(usage.prompt).unwrap_or(0);
        eprintln!(
            "\n{}{stop_reason} · {} requests · {} tool calls · {} tokens in, {cached}% cached · {} out · {:.1}s{}",
            p.dim,
            stats.requests,
            stats.tool_calls,
            usage.prompt,
            usage.completion,
            elapsed.as_secs_f32(),
            p.reset
        );
    }
    Ok(if stop == Some(Stop::Completed) { ExitCode::SUCCESS } else { ExitCode::FAILURE })
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
    /// The model's text is mid-line on stdout.
    line_open: bool,
    /// Newlines at the end of the text so far, printed only if more text follows.
    held_newlines: usize,
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
            line_open: false,
            held_newlines: 0,
            dim: pick("\x1b[2m"),
            accent: pick("\x1b[34m"),
            red: pick("\x1b[31m"),
            reset: pick("\x1b[0m"),
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Text(text) => {
                let text = if self.last == Last::Text { text.as_str() } else { text.trim_start() };
                let body = text.trim_end_matches('\n');
                if body.is_empty() {
                    if self.last == Last::Text {
                        self.held_newlines += text.len();
                    }
                    return;
                }
                if self.last == Last::Tool {
                    println!();
                }
                print!("{}{body}", "\n".repeat(self.held_newlines));
                let _ = std::io::stdout().flush();
                self.held_newlines = text.len() - body.len();
                self.line_open = true;
                self.last = Last::Text;
            }
            Event::Retry { attempt, wait, reason } => {
                self.end_text();
                eprintln!("{}retry {attempt} in {:.1}s: {}{}", self.dim, wait.as_secs_f32(), llm::truncate(&reason, 300), self.reset);
            }
            Event::ToolStart { summary, .. } => {
                if self.last == Last::Text {
                    self.end_text();
                    println!();
                }
                let summary = summary.replace(&format!("{}/", self.cwd), "").replace(&self.cwd, ".");
                eprint!("{}●{} {summary}", self.accent, self.reset);
                self.last = Last::Tool;
            }
            Event::ToolEnd { is_error, .. } => {
                let mark = if is_error { format!(" {}✗{}", self.red, self.reset) } else { String::new() };
                eprintln!("{mark}");
            }
            Event::Notice(text) => {
                self.end_text();
                eprintln!("{}{text}{}", self.dim, self.reset);
            }
            Event::Sub { label, event } => match *event {
                Event::ToolStart { summary, .. } => eprintln!("  {}↳ {label}: {summary}{}", self.dim, self.reset),
                Event::ToolEnd { is_error: true, brief, .. } => eprintln!("  {}↳ {label}: ✗ {brief}{}", self.red, self.reset),
                _ => {}
            },
            Event::Reasoning(_) | Event::Usage(_) | Event::Todos(_) | Event::Ask(_) => {}
        }
    }

    /// Ends the model's text with exactly one newline, dropping any it held back.
    fn end_text(&mut self) {
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
mod tests {
    #[test]
    fn system_prompt_and_tools_fit_the_token_budget() {
        let base = include_str!("prompt/system.md");
        let mut tools = crate::tools::definitions();
        tools.push(crate::skills::definition());
        let tools = serde_json::to_string(&tools).unwrap();
        let chars = base.len() + tools.len();
        assert!(chars <= 12_000, "system prompt + tools = {chars} chars, over the ~3k-token budget");
    }
}
