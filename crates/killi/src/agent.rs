//! The agent loop. History is append-only and holds messages exactly as sent; the system
//! prompt and tool list are fixed for the session, so every request reuses the cached prefix.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};

use crate::llm::{Client, Delta, Reply, Usage, truncate};
use crate::prompt::SystemPrompt;
use crate::session::Session;
use crate::tools;

const MAX_CONTINUATIONS: u32 = 3;
const REPEAT_WARNINGS: [u32; 3] = [3, 5, 8];
const REPEAT_STOP: u32 = 12;
const MAX_RESULT_CHARS: usize = 50_000;

pub enum Event<'a> {
    Llm(Delta<'a>),
    ToolStart(String),
    ToolEnd { is_error: bool },
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum Stop {
    #[default]
    Completed,
    MaxTokens,
    RepeatBreaker,
}

#[derive(Default)]
pub struct Stats {
    pub requests: u32,
    pub tool_calls: u32,
    pub usage: Usage,
    /// Prompt tokens of the first request: the system prompt, tools and first message.
    pub first_prompt: Option<u64>,
}

pub struct Agent {
    client: Client,
    tools: Vec<Value>,
    messages: Vec<Value>,
    ctx: tools::Context,
    pub session: Session,
    pub stats: Stats,
    ids: HashSet<String>,
    repeats: HashMap<String, u32>,
}

impl Agent {
    pub fn new(client: Client, cwd: &Path, prompt: &SystemPrompt) -> Result<Self> {
        let mut session = Session::create(cwd)?;
        let tools = tools::definitions();
        session.record(&json!({
            "type": "session",
            "id": session.id,
            "cwd": cwd,
            "model": client.model.alias,
            "tools": tools,
        }))?;
        let ctx = tools::Context { cwd: cwd.to_path_buf(), spill_dir: session.dir.clone() };
        let mut agent = Self {
            client,
            tools,
            messages: Vec::new(),
            ctx,
            session,
            stats: Stats::default(),
            ids: HashSet::new(),
            repeats: HashMap::new(),
        };
        agent.push(json!({"role": "system", "content": prompt.text}))?;
        Ok(agent)
    }

    /// Runs one user turn to completion and returns the final reply text.
    pub async fn run(&mut self, user: &str, on_event: &mut dyn FnMut(Event)) -> Result<(String, Stop)> {
        self.push(json!({"role": "user", "content": user}))?;
        let mut continuations = 0;
        loop {
            let reply = self.request(on_event).await?;
            let truncated = reply.finish_reason.as_deref() == Some("length");
            let calls = self.record_reply(&reply)?;
            if calls.is_empty() {
                if truncated && continuations < MAX_CONTINUATIONS {
                    continuations += 1;
                    self.push(json!({"role": "user", "content": "<system-reminder>Your reply was cut off at the output token limit. Continue exactly where it stopped.</system-reminder>"}))?;
                    continue;
                }
                return Ok((reply.text, if truncated { Stop::MaxTokens } else { Stop::Completed }));
            }
            let mut stop = false;
            for (id, call) in calls {
                stop |= self.execute(&id, &call, truncated, on_event).await?;
            }
            if stop {
                return Ok((String::new(), Stop::RepeatBreaker));
            }
        }
    }

    async fn request(&mut self, on_event: &mut dyn FnMut(Event)) -> Result<Reply> {
        let mut on_delta = |delta: Delta| on_event(Event::Llm(delta));
        let reply = self.client.complete(&self.messages, &self.tools, &self.session.id, &mut on_delta).await;
        self.stats.requests += 1;
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                self.session.record(&json!({"type": "error", "message": format!("{error:#}")}))?;
                return Err(error);
            }
        };
        let usage = &reply.usage;
        self.stats.first_prompt.get_or_insert(usage.prompt);
        let total = &mut self.stats.usage;
        total.prompt += usage.prompt;
        total.cached += usage.cached;
        total.completion += usage.completion;
        total.reasoning += usage.reasoning;
        self.session.record(&json!({
            "type": "usage",
            "prompt": usage.prompt,
            "cached": usage.cached,
            "completion": usage.completion,
            "reasoning": usage.reasoning,
            "finish_reason": reply.finish_reason,
        }))?;
        Ok(reply)
    }

    /// Appends the assistant message and returns its tool calls under their final IDs.
    fn record_reply(&mut self, reply: &Reply) -> Result<Vec<(String, crate::llm::ToolCall)>> {
        let calls: Vec<_> = reply.tool_calls.iter().map(|call| (self.unique_id(&call.id), call.clone())).collect();
        let mut message = json!({"role": "assistant"});
        if !reply.text.is_empty() || calls.is_empty() {
            message["content"] = reply.text.clone().into();
        }
        if let Some(key) = reply.reasoning_key.filter(|_| !reply.reasoning.is_empty()) {
            message[key] = reply.reasoning.clone().into();
        }
        if !reply.reasoning_items.is_empty() {
            message["reasoning_items"] = reply.reasoning_items.clone().into();
        }
        if !calls.is_empty() {
            message["tool_calls"] = calls
                .iter()
                .map(|(id, call)| {
                    json!({"id": id, "type": "function", "function": {"name": call.name, "arguments": call.arguments}})
                })
                .collect();
        }
        self.push(message)?;
        Ok(calls)
    }

    /// Runs one tool call and appends its result. Returns true when the repeat breaker trips.
    async fn execute(
        &mut self,
        id: &str,
        call: &crate::llm::ToolCall,
        truncated: bool,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<bool> {
        self.stats.tool_calls += 1;
        let parsed = match call.arguments.trim() {
            "" => Ok(json!({})),
            raw => serde_json::from_str::<Value>(raw),
        };
        let mut output = match parsed {
            Ok(args) if args.is_object() => {
                on_event(Event::ToolStart(tools::summary(&call.name, &args)));
                tools::run(&self.ctx, &call.name, args).await
            }
            other => {
                on_event(Event::ToolStart(format!("{} (invalid arguments)", call.name)));
                let why = match other {
                    Err(e) => e.to_string(),
                    Ok(_) => "arguments must be a JSON object".into(),
                };
                let cut = if truncated {
                    " Your reply hit the output token limit, which cut the arguments off; send smaller pieces."
                } else {
                    ""
                };
                tools::Output::error(format!(
                    "Invalid arguments for {}: {why}.{cut} You sent: {}",
                    call.name,
                    truncate(&call.arguments, 2000)
                ))
            }
        };
        on_event(Event::ToolEnd { is_error: output.is_error });

        if output.text.len() > MAX_RESULT_CHARS {
            output.text = self.shorten(&output.text);
        }
        let count = self.repeats.entry(format!("{}\0{}", call.name, call.arguments)).or_insert(0);
        *count += 1;
        if REPEAT_WARNINGS.contains(count) {
            output.text.push_str(&format!(
                "\n\n<system-reminder>You have made this exact {} call {count} times. Repeating it won't change the result; try a different approach.</system-reminder>",
                call.name
            ));
        }
        let stop = *count >= REPEAT_STOP;
        self.push(json!({"role": "tool", "tool_call_id": id, "content": output.text}))?;
        Ok(stop)
    }

    /// Keeps the first 4096 and last 1024 characters and saves the whole result to a file.
    fn shorten(&self, text: &str) -> String {
        let head = truncate(text, 4096);
        let mut tail_start = text.len() - 1024;
        while !text.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        let saved = match tools::spill(&self.ctx, "result", text) {
            Some(path) => format!("The full result is saved at {}; read it with offset and limit.", path.display()),
            None => "The full result could not be saved.".into(),
        };
        format!("{head}\n\n[… {} characters omitted. {saved}]\n\n{}", text.len() - head.len() - (text.len() - tail_start), &text[tail_start..])
    }

    /// Tool-call IDs limited to `[A-Za-z0-9_-]` and 64 characters, unique in the session.
    fn unique_id(&mut self, raw: &str) -> String {
        let mut id: String =
            raw.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).take(64).collect();
        if id.is_empty() {
            id = format!("call_{}", self.ids.len() + 1);
        }
        let base = id.chars().take(58).collect::<String>();
        let mut n = 2;
        while !self.ids.insert(id.clone()) {
            id = format!("{base}__{n}");
            n += 1;
        }
        id
    }

    fn push(&mut self, message: Value) -> Result<()> {
        self.session.record(&json!({"type": "message", "message": message}))?;
        self.messages.push(message);
        Ok(())
    }
}
