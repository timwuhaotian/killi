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
    if !options.json {
        eprintln!(
            "killi · {alias} · instructions ~{} tokens from {} file(s) · session {}",
            system.instruction_tokens,
            system.instruction_files.len(),
            agent.session.id
        );
    }

    let started = Instant::now();
    let json_mode = options.json;
    let mut on_event = |event: Event| {
        if json_mode {
            return;
        }
        match event {
            Event::Llm(Delta::Text(text)) => {
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            Event::Llm(Delta::Retry { attempt, wait, reason }) => {
                eprintln!("\n[retry {attempt} in {:.1}s: {}]", wait.as_secs_f32(), llm::truncate(reason, 300));
            }
            Event::ToolStart(summary) => eprint!("\n● {summary}"),
            Event::ToolEnd { is_error } => eprintln!("{}", if is_error { "  ✗" } else { "" }),
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
        println!();
        if let Some(error) = &error {
            eprintln!("killi: {error}");
        }
        let usage = &stats.usage;
        let cached = (usage.cached * 100).checked_div(usage.prompt).unwrap_or(0);
        eprintln!(
            "· {stop_reason} · {} requests · {} tool calls · {} in ({cached}% cached) · {} out · {:.1}s",
            stats.requests,
            stats.tool_calls,
            usage.prompt,
            usage.completion,
            elapsed.as_secs_f32()
        );
    }
    Ok(if stop == Some(Stop::Completed) { ExitCode::SUCCESS } else { ExitCode::FAILURE })
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
