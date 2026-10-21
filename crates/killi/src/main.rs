mod agent;
mod config;
mod llm;
mod prompt;
mod session;
mod tools;
mod update;

use std::io::Write;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Result, bail};
use serde_json::json;

use agent::{Agent, Event, Stop};
use llm::Delta;

const USAGE: &str = "\
killi — a minimal coding agent

Usage:
  killi -p <prompt> [-m <provider/model>] [--json]   Run one task and print the answer
  killi import-kimi [--force]                         Copy providers and models from Kimi Code
  killi update                                        Install the latest published version
  killi --version

Config: ~/.killi/config.toml. The interactive TUI isn't built yet.";

struct Options {
    prompt: Option<String>,
    model: Option<String>,
    json: bool,
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
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        _ => run(parse(args)?).await,
    }
}

fn parse(args: Vec<String>) -> Result<Options> {
    let mut options = Options { prompt: None, model: None, json: false };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" | "--print" => options.prompt = args.next(),
            "-m" | "--model" => options.model = args.next(),
            "--json" => options.json = true,
            other => bail!("unknown argument `{other}`\n\n{USAGE}"),
        }
    }
    Ok(options)
}

async fn run(options: Options) -> Result<ExitCode> {
    let Some(prompt) = options.prompt.filter(|p| !p.trim().is_empty()) else {
        bail!("nothing to do: pass a task with -p\n\n{USAGE}");
    };
    let config = config::Config::load()?;
    let model = config.resolve(options.model.as_deref())?;
    let cwd = std::env::current_dir()?;
    let system = prompt::build(&cwd);
    let mut agent = Agent::new(llm::Client::new(model)?, &cwd, &system)?;
    let alias = options.model.or(config.default_model).unwrap_or_default();
    let mut printer = Printer::new(&cwd);
    if !options.json {
        let instructions = match system.instruction_files.len() {
            0 => String::new(),
            n => format!(" · {n} instruction file(s), ~{} tokens", system.instruction_tokens),
        };
        eprintln!("{}killi {} · {alias}{instructions}{}\n", printer.dim, env!("CARGO_PKG_VERSION"), printer.reset);
    }

    let started = Instant::now();
    let json_mode = options.json;
    let mut on_event = |event: Event| {
        if !json_mode {
            printer.event(event);
        }
    };
    let result = agent.run(&prompt, &mut on_event).await;
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
            "model": alias,
            "session_id": agent.session.id,
            "session_log": agent.session.log_path,
            "requests": stats.requests,
            "tool_calls": stats.tool_calls,
            "prompt_tokens": stats.usage.prompt,
            "cached_tokens": stats.usage.cached,
            "completion_tokens": stats.usage.completion,
            "reasoning_tokens": stats.usage.reasoning,
            "first_prompt_tokens": stats.first_prompt,
            "instruction_tokens": system.instruction_tokens,
            "duration_ms": elapsed.as_millis() as u64,
            "final_text": text,
        });
        println!("{summary}");
    } else {
        printer.finish();
        if let Some(error) = &error {
            eprintln!("{}killi: {error}{}", printer.red, printer.reset);
        }
        let usage = &stats.usage;
        let cached = (usage.cached * 100).checked_div(usage.prompt).unwrap_or(0);
        eprintln!(
            "\n{}{stop_reason} · {} requests · {} tool calls · {} tokens in, {cached}% cached · {} out · {:.1}s{}",
            printer.dim,
            stats.requests,
            stats.tool_calls,
            usage.prompt,
            usage.completion,
            elapsed.as_secs_f32(),
            printer.reset
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

/// Prints a run as compact lines: the model's text on stdout, one line per tool call on
/// stderr, a blank line between the two. Colors only when stderr is a terminal.
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
            Event::Llm(Delta::Text(text)) => {
                let text = if self.last == Last::Text { text } else { text.trim_start() };
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
            Event::Llm(Delta::Reasoning(_)) => {}
            Event::Llm(Delta::Retry { attempt, wait, reason }) => {
                self.end_text();
                eprintln!(
                    "{}retry {attempt} in {:.1}s: {}{}",
                    self.dim,
                    wait.as_secs_f32(),
                    llm::truncate(reason, 300),
                    self.reset
                );
            }
            Event::ToolStart(summary) => {
                if self.last == Last::Text {
                    self.end_text();
                    println!();
                }
                let summary = summary.replace(&format!("{}/", self.cwd), "").replace(&self.cwd, ".");
                eprint!("{}●{} {summary}", self.accent, self.reset);
                self.last = Last::Tool;
            }
            Event::ToolEnd { is_error } => {
                let mark = if is_error { format!(" {}✗{}", self.red, self.reset) } else { String::new() };
                eprintln!("{mark}");
            }
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
        let tools = serde_json::to_string(&crate::tools::definitions()).unwrap();
        let chars = base.len() + tools.len();
        assert!(chars <= 12_000, "system prompt + tools = {chars} chars, over the ~3k-token budget");
    }
}
