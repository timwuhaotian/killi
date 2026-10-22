//! The agent loop. History is append-only and holds messages exactly as sent; the system
//! prompt and tool list are fixed for the session, so every request reuses the cached prefix.
//! Notes from killi to the model are appended as user messages, never edited into old ones.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::future::Future;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::config::Thinking;
use crate::llm::{Client, Delta, Reply, ToolCall, Usage, truncate};
use crate::session::Session;
use crate::hooks::{HooksConfig, tool_payload};
use crate::skills::Skills;
use crate::{plan, swarm, tools};

const MAX_CONTINUATIONS: u32 = 3;
const REPEAT_WARNINGS: [u32; 3] = [3, 5, 7];
const REPEAT_STOP: u32 = 8;
const MAX_RESULT_CHARS: usize = 50_000;
const MAX_PARALLEL_AGENTS: usize = 4;
/// User prompts kept word for word through a compaction, in characters (about 20k tokens).
const KEPT_PROMPTS_CHARS: usize = 80_000;

/// A subagent with its prompt, or the result of a call that couldn't start one.
type Job = std::result::Result<(Agent, String), tools::Output>;

/// Receives everything the agent does, for a terminal to show.
pub type Sink = Rc<dyn Fn(Event)>;

pub enum Event {
    Text(String),
    Reasoning(String),
    Retry { attempt: u32, wait: Duration, reason: String },
    ToolStart { id: String, summary: String },
    ToolEnd { id: String, is_error: bool, brief: String, display: Option<String>, output: String },
    /// After each request: that request's usage, and the context it used.
    Usage(Usage),
    Todos(Vec<Todo>),
    Ask(Question),
    Notice(String),
    /// Something a subagent did, labeled with its description.
    Sub { label: String, event: Box<Event> },
}

/// A question for the user. `plan` marks a plan waiting for approval.
pub struct Question {
    pub text: String,
    pub options: Vec<String>,
    pub plan: bool,
    pub reply: oneshot::Sender<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Todo {
    pub content: String,
    pub status: String,
}

/// An image pasted into a prompt: the `[Image #N]` placeholder that stands for it in the
/// text, and the image as a data URL.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub label: String,
    pub url: String,
}

/// A message typed during a turn, with the images pasted into it.
pub type Steered = (String, Vec<Image>);

/// What the user can change while the agent works, and the conversation they can ask about.
#[derive(Clone, Default)]
pub struct Controls {
    /// Messages typed during a turn, sent at the next step.
    pub steer: Rc<RefCell<VecDeque<Steered>>>,
    pub plan: Rc<Cell<bool>>,
    /// Swarm mode: the agent is told to split its work across `agent_swarm` subagents.
    pub swarm: Rc<Cell<bool>>,
    /// Whether someone can answer questions; headless runs and subagents can't.
    pub interactive: bool,
    /// The conversation as it stands, for /btw while the agent works.
    pub aside: Rc<RefCell<Aside>>,
}

/// What a side question needs: the model, the tools (sent unchanged so the cached prefix
/// hits) and the conversation. It holds the history weakly, so appending to it never copies it.
#[derive(Clone, Default)]
pub struct Aside {
    client: Option<Rc<Client>>,
    tools: Rc<Vec<Value>>,
    messages: Weak<Vec<Value>>,
    cache_key: String,
}

impl Aside {
    /// Asks `question` about the conversation so far without adding to it. The answer and the
    /// request's usage come back; tool calls in the reply are ignored.
    pub fn ask(&self, question: &str) -> impl Future<Output = Result<(String, Usage)>> + 'static {
        let request = self.messages.upgrade().map(|history| side_request(&history, question));
        let (client, tools, cache_key) = (self.client.clone(), self.tools.clone(), self.cache_key.clone());
        async move {
            let (Some(client), Some(request)) = (client, request) else { bail!("there is no conversation to ask about yet") };
            let reply = client.complete(&request, &tools, &cache_key, &mut |_| {}).await?;
            let text = if reply.text.trim().is_empty() { "(No answer: the model tried to use tools.)".to_string() } else { reply.text };
            Ok((text, reply.usage))
        }
    }
}

/// The history with a side question at the end. Tool calls still running get a placeholder
/// result first, as providers reject a question between a call and its result.
fn side_request(history: &[Value], question: &str) -> Vec<Value> {
    let mut request = history.to_vec();
    for id in unanswered(history) {
        request.push(json!({"role": "tool", "tool_call_id": id, "content": "Still running; no result yet."}));
    }
    request.push(json!({"role": "user", "content": format!("{BTW_NOTE}\n\n{question}")}));
    request
}

/// IDs of the last assistant message's tool calls that have no result yet.
fn unanswered(messages: &[Value]) -> Vec<String> {
    let Some(index) = messages.iter().rposition(|m| m["role"] == "assistant") else { return Vec::new() };
    let answered: HashSet<&str> = messages[index + 1..].iter().filter_map(|m| m["tool_call_id"].as_str()).collect();
    messages[index]["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["id"].as_str())
        .filter(|id| !answered.contains(id))
        .map(String::from)
        .collect()
}

#[derive(Clone, Default)]
pub struct Settings {
    /// Compact once a request's input passes this many tokens.
    pub compact_at: u64,
    /// Extra command prefixes bash may run in plan mode.
    pub plan_allow: Vec<String>,
    /// Subagents an `agent_swarm` call runs at once.
    pub swarm_parallel: usize,
    pub search: crate::tools::SearchSettings,
    pub skills: Skills,
    pub hooks: Rc<HooksConfig>,
    pub mcp: Rc<crate::mcp::Mcp>,
    /// Instruction files already in the system prompt.
    pub instruction_files: Vec<std::path::PathBuf>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum Stop {
    #[default]
    Completed,
    MaxTokens,
    RepeatBreaker,
}

/// How often one exact tool call was made, and a hash of what its latest result said.
#[derive(Default)]
struct Repeat {
    count: u32,
    result: u64,
}

impl Repeat {
    /// Counts another identical call. Returns how many there have been, and whether this one
    /// got the same result as the one before it.
    fn note(&mut self, result: u64) -> (u32, bool) {
        let same = self.count > 0 && self.result == result;
        self.count += 1;
        self.result = result;
        (self.count, same)
    }
}

#[derive(Default)]
pub struct Stats {
    pub requests: u32,
    pub tool_calls: u32,
    pub usage: Usage,
    /// Prompt tokens of the first request: the system prompt, tools and first message.
    pub first_prompt: Option<u64>,
    /// Input tokens of the latest request: how full the context is.
    pub context: u64,
}

pub struct Agent {
    client: Rc<Client>,
    tools: Rc<Vec<Value>>,
    messages: Rc<Vec<Value>>,
    ctx: Rc<tools::Context>,
    pub session: Session,
    pub stats: Stats,
    pub controls: Controls,
    settings: Rc<Settings>,
    sink: Sink,
    ids: HashSet<String>,
    repeats: HashMap<String, Repeat>,
    /// The log already says that the provider hides its reasoning.
    reasoning_noted: bool,
    todos: Vec<Todo>,
    plan_announced: bool,
    swarm_announced: bool,
    interrupted: bool,
    depth: u8,
    children: usize,
    /// Index of the assistant message whose tool calls are running.
    current_reply: usize,
    session_started: bool,
    instructions: crate::prompt::Instructions,
}

impl Agent {
    pub fn new(
        client: Client,
        cwd: &Path,
        system: &str,
        tools: Vec<Value>,
        settings: Settings,
        controls: Controls,
        sink: Sink,
    ) -> Result<Self> {
        let session = Session::create(cwd)?;
        let mut agent = Self::assemble(client, cwd, tools, settings, controls, sink, session);
        agent.session.record(&json!({
            "type": "session",
            "id": agent.session.id,
            "cwd": cwd,
            "model": agent.client.model.alias,
            "effort": agent.client.model.thinking.label(),
            "tools": *agent.tools,
        }))?;
        agent.push(json!({"role": "system", "content": system}))?;
        Ok(agent)
    }

    /// Continues a logged session with its original system prompt, so its cached prefix hits.
    pub fn resume(
        client: Client,
        cwd: &Path,
        log: &Path,
        tools: Vec<Value>,
        settings: Settings,
        controls: Controls,
        sink: Sink,
    ) -> Result<Self> {
        let loaded = crate::session::load(log)?;
        let session = Session::open(log)?;
        let mut agent = Self::assemble(client, cwd, tools, settings, controls, sink, session);
        for message in &loaded.messages {
            for call in message["tool_calls"].as_array().into_iter().flatten() {
                if let Some(id) = call["id"].as_str() {
                    agent.ids.insert(id.to_string());
                }
            }
        }
        agent.messages = Rc::new(loaded.messages);
        agent.stats.context = loaded.context;
        agent.recover()?;
        agent.share();
        Ok(agent)
    }

    fn assemble(
        client: Client,
        cwd: &Path,
        tools: Vec<Value>,
        settings: Settings,
        controls: Controls,
        sink: Sink,
        session: Session,
    ) -> Self {
        let settings_files = settings.instruction_files.clone();
        let mut ctx = tools::Context::new(cwd.to_path_buf(), session.dir.clone());
        ctx.images = client.model.images;
        ctx.search = settings.search.clone();
        Self {
            client: Rc::new(client),
            tools: Rc::new(tools),
            messages: Rc::default(),
            ctx: Rc::new(ctx),
            session,
            stats: Stats::default(),
            controls,
            settings: Rc::new(settings),
            sink,
            ids: HashSet::new(),
            repeats: HashMap::new(),
            reasoning_noted: false,
            todos: Vec::new(),
            plan_announced: false,
            swarm_announced: false,
            interrupted: false,
            depth: 0,
            children: 0,
            current_reply: 0,
            session_started: false,
            instructions: crate::prompt::Instructions::new(cwd, &settings_files),
        }
    }

    pub fn sink(&self) -> Sink {
        self.sink.clone()
    }

    /// Continues the conversation on another model; the next request starts the cache over.
    /// The log notes it, so a resume continues on this model.
    pub fn switch_model(&mut self, client: Client) -> Result<()> {
        self.ctx_images(client.model.images);
        self.client = Rc::new(client);
        self.stats.context = 0;
        self.share();
        self.record_model()
    }

    /// Reasons at another effort from the next request on, on the same model.
    pub fn set_thinking(&mut self, thinking: Thinking) -> Result<()> {
        let mut client = (*self.client).clone();
        client.model.thinking = thinking;
        self.client = Rc::new(client);
        self.share();
        self.record_model()
    }

    /// Lets /btw see the conversation and model as they are now.
    fn share(&self) {
        *self.controls.aside.borrow_mut() = Aside {
            client: Some(self.client.clone()),
            tools: self.tools.clone(),
            messages: Rc::downgrade(&self.messages),
            cache_key: self.session.id.clone(),
        };
    }

    fn record_model(&mut self) -> Result<()> {
        let model = &self.client.model;
        self.session.record(&json!({"type": "model", "model": model.alias, "effort": model.thinking.label()}))
    }

    /// The model and effort this session runs on now, for a fresh session to start on.
    pub fn client(&self) -> Client {
        (*self.client).clone()
    }

    pub fn thinking(&self) -> &Thinking {
        &self.client.model.thinking
    }

    fn ctx_images(&mut self, images: bool) {
        if let Some(ctx) = Rc::get_mut(&mut self.ctx) {
            ctx.images = images;
        }
    }

    /// Replaces the system prompt and settings on purpose, for /reload; the next request is
    /// uncached.
    pub fn reload_system(&mut self, system: &str, settings: Settings) -> Result<()> {
        self.instructions = crate::prompt::Instructions::new(&self.ctx.cwd, &settings.instruction_files);
        self.settings = Rc::new(settings);
        Rc::make_mut(&mut self.messages)[0] = json!({"role": "system", "content": system});
        self.share();
        self.session.record(&json!({"type": "compaction", "messages": *self.messages}))
    }

    pub fn model_alias(&self) -> &str {
        &self.client.model.alias
    }

    /// Whether the model takes images (`image_in` in its capabilities).
    pub fn sees_images(&self) -> bool {
        self.client.model.images
    }

    pub fn context_window(&self) -> Option<u64> {
        self.client.model.context_window
    }

    /// Deletes this session's log if nothing was ever said in it, so opening killi and leaving
    /// doesn't leave a file behind.
    pub fn discard_if_unused(&self) {
        if self.messages.len() <= 1 {
            self.session.discard();
        }
    }

    /// The user prompts of this session, oldest first, for history and pickers.
    pub fn user_prompts(&self) -> Vec<String> {
        crate::session::user_prompts(&self.messages)
    }

    /// Runs one user turn to completion and returns the final reply text.
    pub async fn run(&mut self, user: &str, images: &[Image]) -> Result<(String, Stop)> {
        let mut notes = Vec::new();
        if std::mem::take(&mut self.interrupted) {
            notes.push("The user interrupted your previous turn.".to_string());
        }
        notes.extend(self.settings.mcp.take_notes());
        let changed = self.instructions.changed();
        if !changed.is_empty() {
            notes.push(format!(
                "These instruction files changed on disk since your instructions were loaded; read them again and follow the current version: {}",
                changed.join(", ")
            ));
        }
        if !std::mem::replace(&mut self.session_started, true) {
            notes.extend(self.hook("SessionStart", None, json!({"source": "startup"})).await.context);
        }
        let submitted = self.hook("UserPromptSubmit", None, json!({"prompt": user})).await;
        if let Some(reason) = submitted.block {
            return Err(anyhow!("a hook blocked this prompt: {reason}"));
        }
        notes.extend(submitted.context);
        let content = if notes.is_empty() {
            user.to_string()
        } else {
            format!("<system-reminder>{}</system-reminder>\n\n{user}", notes.join("\n"))
        };
        self.push(json!({"role": "user", "content": content}))?;
        self.push_images(images)?;
        let mut continuations = 0;
        let mut overflow_compacted = false;
        let mut stop_blocks = 0;
        loop {
            self.before_request().await?;
            let reply = match self.request().await {
                Ok(reply) => reply,
                Err(error) if !overflow_compacted && is_overflow(&error) => {
                    overflow_compacted = true;
                    self.compact(None).await?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let truncated = reply.finish_reason.as_deref() == Some("length");
            let calls = self.record_reply(&reply)?;
            if calls.is_empty() {
                if truncated && continuations < MAX_CONTINUATIONS {
                    continuations += 1;
                    self.push(json!({"role": "user", "content": "<system-reminder>Your reply was cut off at the output token limit. Continue exactly where it stopped.</system-reminder>"}))?;
                    continue;
                }
                if !self.controls.steer.borrow().is_empty() {
                    continue;
                }
                let event = if self.depth == 0 { "Stop" } else { "SubagentStop" };
                let stop = self.hook(event, None, json!({"stop_hook_active": stop_blocks > 0})).await;
                if let Some(reason) = stop.block.filter(|_| stop_blocks < 3) {
                    stop_blocks += 1;
                    self.push(json!({"role": "user", "content": format!("<system-reminder>A hook asked you to keep going: {reason}</system-reminder>")}))?;
                    continue;
                }
                return Ok((reply.text, if truncated { Stop::MaxTokens } else { Stop::Completed }));
            }
            if self.execute_calls(calls, truncated).await? {
                return Ok((String::new(), Stop::RepeatBreaker));
            }
        }
    }

    /// Everything that goes into the conversation between steps: messages typed meanwhile,
    /// a plan- or swarm-mode change, finished background tasks, and compaction when it's due.
    async fn before_request(&mut self) -> Result<()> {
        let steered: Vec<_> = self.controls.steer.borrow_mut().drain(..).collect();
        for (text, images) in steered {
            self.push(json!({"role": "user", "content": text}))?;
            self.push_images(&images)?;
        }
        let plan = self.controls.plan.get();
        if plan != self.plan_announced && self.depth == 0 {
            self.plan_announced = plan;
            self.push(json!({"role": "user", "content": if plan { plan::ON } else { plan::OFF }}))?;
        }
        let swarming = self.controls.swarm.get();
        if swarming != self.swarm_announced && self.depth == 0 {
            self.swarm_announced = swarming;
            self.push(json!({"role": "user", "content": if swarming { swarm::ON } else { swarm::OFF }}))?;
        }
        let finished = self.ctx.tasks.borrow_mut().newly_finished();
        if !finished.is_empty() && self.depth == 0 {
            let text = format!("<system-reminder>{} Read the output with task_output.</system-reminder>", finished.join(" "));
            self.push(json!({"role": "user", "content": text}))?;
        }
        let found = self.instructions.take_found();
        if !found.is_empty() {
            let list: String = found.iter().map(|p| format!("\n- {p}")).collect();
            self.push(json!({"role": "user", "content": format!("<system-reminder>These AGENTS.md files apply to paths you just used but aren't in your instructions. Read them before changing anything in those folders:{list}</system-reminder>")}))?;
        }
        if self.compaction_due() {
            self.compact(None).await?;
        }
        Ok(())
    }

    fn compaction_due(&self) -> bool {
        let mut limit = self.settings.compact_at;
        if let Some(window) = self.client.model.context_window {
            limit = limit.min(window * 85 / 100);
        }
        limit > 0 && self.stats.context > limit
    }

    async fn request(&mut self) -> Result<Reply> {
        let sink = self.sink.clone();
        let mut on_delta = |delta: Delta| {
            sink(match delta {
                Delta::Text(text) => Event::Text(text.to_string()),
                Delta::Reasoning(text) => Event::Reasoning(text.to_string()),
                Delta::Retry { attempt, wait, reason } => Event::Retry { attempt, wait, reason: reason.to_string() },
            })
        };
        let started = Instant::now();
        let reply = self.client.complete(&self.messages, &self.tools, &self.session.id, &mut on_delta).await;
        let took = started.elapsed();
        self.stats.requests += 1;
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                self.session.record(&json!({"type": "error", "message": format!("{error:#}"), "ms": millis(took)}))?;
                return Err(error);
            }
        };
        self.account(&reply.usage, Some(&reply.finish_reason), Some(took))?;
        self.note_hidden_reasoning(&reply)?;
        Ok(reply)
    }

    /// Some providers count reasoning tokens but don't send the reasoning on their chat API, so
    /// it can't be kept or replayed and the model starts each step without its own thinking.
    /// The log says so once, to explain a model that forgets what it just did.
    fn note_hidden_reasoning(&mut self, reply: &Reply) -> Result<()> {
        if hides_reasoning(reply) && !std::mem::replace(&mut self.reasoning_noted, true) {
            self.session.record(&json!({
                "type": "warn",
                "message": format!(
                    "The provider counted {} reasoning tokens but sent no reasoning, so it can't be kept. If it offers the Responses API, set `type = \"openai_responses\"` on its provider in config.toml.",
                    reply.usage.reasoning
                ),
            }))?;
        }
        Ok(())
    }

    fn account(&mut self, usage: &Usage, finish_reason: Option<&Option<String>>, took: Option<Duration>) -> Result<()> {
        self.stats.first_prompt.get_or_insert(usage.prompt);
        self.stats.context = usage.prompt;
        let total = &mut self.stats.usage;
        total.prompt += usage.prompt;
        total.cached += usage.cached;
        total.completion += usage.completion;
        total.reasoning += usage.reasoning;
        (self.sink)(Event::Usage(usage.clone()));
        self.session.record(&json!({
            "type": "usage",
            "prompt": usage.prompt,
            "cached": usage.cached,
            "completion": usage.completion,
            "reasoning": usage.reasoning,
            "finish_reason": finish_reason.cloned().flatten(),
            "ms": took.map(millis),
        }))
    }

    /// Appends the assistant message and returns its tool calls under their final IDs.
    fn record_reply(&mut self, reply: &Reply) -> Result<Vec<(String, ToolCall)>> {
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
        self.current_reply = self.messages.len();
        self.push(message)?;
        Ok(calls)
    }

    /// Runs a reply's tool calls in order; consecutive `agent` calls run in parallel, and an
    /// `agent_swarm` call runs its subagents in parallel. Returns true when the repeat breaker trips.
    async fn execute_calls(&mut self, calls: Vec<(String, ToolCall)>, truncated: bool) -> Result<bool> {
        let mut stop = false;
        let mut images = Vec::new();
        let mut i = 0;
        while i < calls.len() {
            if calls[i].1.name == "agent" && self.depth == 0 {
                let end = calls[i..].iter().position(|(_, c)| c.name != "agent").map_or(calls.len(), |n| i + n);
                let started = Instant::now();
                let outputs = self.run_subagents(&calls[i..end]).await;
                let took = started.elapsed();
                for ((id, call), output) in calls[i..end].iter().zip(outputs) {
                    stop |= self.finish_call(id, call, output, took)?;
                }
                i = end;
                continue;
            }
            let (id, call) = &calls[i];
            if call.name == "agent_swarm" && self.depth == 0 {
                let started = Instant::now();
                let output = self.run_swarm(id, call).await;
                stop |= self.finish_call(id, call, output, started.elapsed())?;
                i += 1;
                continue;
            }
            let started = Instant::now();
            let output = self.execute(id, call, truncated).await;
            let took = started.elapsed();
            if let Some(image) = &output.image {
                images.push((tools::summary(&call.name, &parse_args(call).unwrap_or_default(), &self.ctx.cwd), image.clone()));
            }
            stop |= self.finish_call(id, call, output, took)?;
            i += 1;
        }
        if !images.is_empty() {
            let mut parts = Vec::new();
            for (label, url) in images {
                parts.extend(image_parts(&format!("Image from {label}:"), &url));
            }
            self.push(json!({"role": "user", "content": parts}))?;
        }
        Ok(stop)
    }

    /// The images pasted into a prompt, as a user message right after it with each image
    /// behind its placeholder, so the prompt itself stays a plain text message.
    fn push_images(&mut self, images: &[Image]) -> Result<()> {
        if images.is_empty() {
            return Ok(());
        }
        let parts: Vec<Value> = images.iter().flat_map(|image| image_parts(&format!("{}:", image.label), &image.url)).collect();
        self.push(json!({"role": "user", "content": parts}))
    }

    async fn execute(&mut self, id: &str, call: &ToolCall, truncated: bool) -> tools::Output {
        self.stats.tool_calls += 1;
        let args = match parse_args(call) {
            Ok(args) => args,
            Err(why) => {
                let output = invalid_arguments(call, &why, truncated);
                self.tool_start(id, &format!("{} (invalid arguments)", call.name));
                return output;
            }
        };
        self.tool_start(id, &tools::summary(&call.name, &args, &self.ctx.cwd));
        let before = self.hook("PreToolUse", Some(&call.name), tool_payload(&call.name, &args, None)).await;
        if let Some(reason) = before.block {
            return tools::Output::error(format!("A hook blocked this call: {reason}"));
        }
        if let Some(path) = args["path"].as_str() {
            self.instructions.touched(&tools::resolve(&self.ctx, path));
        }
        let mut output = self.dispatch(call, &args).await;
        let after = self.hook("PostToolUse", Some(&call.name), tool_payload(&call.name, &args, Some(&output.text))).await;
        if let Some(note) = after.block.or_else(|| (!after.context.is_empty()).then(|| after.context.join("\n"))) {
            output.text.push_str(&format!("\n\n<system-reminder>A hook says: {note}</system-reminder>"));
        }
        output
    }

    async fn dispatch(&mut self, call: &ToolCall, args: &Value) -> tools::Output {
        let plan = self.controls.plan.get();
        let args = args.clone();
        match call.name.as_str() {
            "skill" => self.settings.skills.run(&args),
            "mcp" => self.settings.mcp.run(&args, self.ctx.images).await,
            name if self.settings.mcp.is_native(name) => self.settings.mcp.call_native(name, args, self.ctx.images).await,
            "todo_write" => self.todo_write(&args),
            "later" => self.later(&args),
            "ask_user_question" => self.ask(&args).await,
            "exit_plan_mode" => self.exit_plan(&args).await,
            "agent" | "agent_swarm" => tools::Output::error("Subagents can't start other subagents."),
            "write" | "edit" if plan => tools::Output::error(format!(
                "Plan mode is on, so {} is blocked. Finish the plan and call exit_plan_mode.",
                call.name
            )),
            "bash" if plan
                && !plan::bash_allowed(args["command"].as_str().unwrap_or_default(), &self.settings.plan_allow) =>
            {
                tools::Output::error("Plan mode is on: bash runs only read-only commands (ls, rg, git log, cargo check, …), without redirects. Finish the plan and call exit_plan_mode.")
            }
            _ => tools::run(&self.ctx, &call.name, args).await,
        }
    }

    async fn hook(&self, event: &str, tool: Option<&str>, payload: Value) -> crate::hooks::Outcome {
        if self.settings.hooks.is_empty() {
            return crate::hooks::Outcome::default();
        }
        self.settings.hooks.run(event, tool, &self.ctx.cwd, &self.session.id, payload).await
    }

    fn tool_start(&self, id: &str, summary: &str) {
        (self.sink)(Event::ToolStart { id: id.into(), summary: summary.into() });
    }

    /// Reports and records a call's result. Returns true when the repeat breaker trips.
    fn finish_call(&mut self, id: &str, call: &ToolCall, mut output: tools::Output, took: Duration) -> Result<bool> {
        (self.sink)(Event::ToolEnd {
            id: id.into(),
            is_error: output.is_error,
            brief: output.brief.clone(),
            display: output.display.take(),
            output: output.text.clone(),
        });
        if output.text.len() > MAX_RESULT_CHARS {
            output.text = self.shorten(&output.text);
        }
        let result = hash_text(&output.text);
        let (count, same) = self.repeats.entry(format!("{}\0{}", call.name, call.arguments)).or_default().note(result);
        if REPEAT_WARNINGS.contains(&count) {
            let same = if same { " and got the same result" } else { "" };
            output.text.push_str(&format!(
                "\n\n<system-reminder>You have made this exact {} call {count} times{same}. Repeating it won't change the result; try a different approach.</system-reminder>",
                call.name
            ));
        }
        let meta = json!({"is_error": output.is_error, "brief": output.brief, "ms": millis(took)});
        self.push_meta(json!({"role": "tool", "tool_call_id": id, "content": output.text}), Some(meta))?;
        Ok(count >= REPEAT_STOP)
    }

    /// Notes something for later, or crosses an item off, in the project's `later.md`.
    fn later(&mut self, args: &Value) -> tools::Output {
        if let Some(number) = args["done"].as_u64() {
            return match crate::later::done(&self.ctx.cwd, number as usize) {
                Ok(item) => tools::Output::ok(format!("Crossed off: {item}")).brief("done"),
                Err(e) => tools::Output::error(format!("{e:#}")),
            };
        }
        let Some(note) = args["note"].as_str().map(str::trim).filter(|note| !note.is_empty()) else {
            return tools::Output::error("later needs `note` (what, and where) or `done` (an item's number).");
        };
        match crate::later::add(&self.ctx.cwd, note) {
            Ok(open) => tools::Output::ok(format!("Noted for later; {open} open. The user sees the list with /later.")).brief(format!("{open} open")),
            Err(e) => tools::Output::error(format!("{e:#}")),
        }
    }

    fn todo_write(&mut self, args: &Value) -> tools::Output {
        let Ok(todos) = serde_json::from_value::<Vec<Todo>>(args["todos"].clone()) else {
            return tools::Output::error("todo_write needs `todos`: a list of {content, status}.");
        };
        if let Some(bad) = todos.iter().find(|t| !matches!(t.status.as_str(), "pending" | "in_progress" | "completed")) {
            return tools::Output::error(format!("Unknown status `{}`; use pending, in_progress or completed.", bad.status));
        }
        self.todos = todos.clone();
        let open = todos.iter().filter(|t| t.status != "completed").count();
        (self.sink)(Event::Todos(todos));
        tools::Output::ok(format!("Task list updated: {open} open, {} done.", self.todos.len() - open))
            .brief(format!("{open} open"))
    }

    async fn ask(&mut self, args: &Value) -> tools::Output {
        if !self.controls.interactive {
            return tools::Output::error(
                "No one can answer questions in this session. Decide yourself, and say in your final message what you assumed.",
            );
        }
        let text = args["question"].as_str().unwrap_or_default().to_string();
        let options: Vec<String> =
            args["options"].as_array().into_iter().flatten().filter_map(|o| o.as_str().map(String::from)).collect();
        match self.wait_for_answer(text, options, false).await {
            Some(answer) => tools::Output::ok(format!("The user answered: {answer}")).brief(answer),
            None => tools::Output::error("The question was dismissed without an answer."),
        }
    }

    async fn exit_plan(&mut self, args: &Value) -> tools::Output {
        if !self.controls.plan.get() {
            return tools::Output::error("Plan mode is off; go ahead without exit_plan_mode.");
        }
        if self.depth > 0 {
            return tools::Output::error("Only the main agent can leave plan mode; report your plan instead.");
        }
        let plan = args["plan"].as_str().unwrap_or_default().to_string();
        if !self.controls.interactive {
            self.controls.plan.set(false);
            self.plan_announced = false;
            return tools::Output::ok("No one is here to approve, so plan mode is off. Carry out the plan.");
        }
        let options = vec!["Approve and start".to_string(), "Keep planning".to_string()];
        match self.wait_for_answer(plan, options, true).await.as_deref() {
            Some("Approve and start") => {
                self.controls.plan.set(false);
                self.plan_announced = false;
                tools::Output::ok("The user approved the plan. Plan mode is off; carry it out now.").brief("approved")
            }
            Some(feedback) if feedback != "Keep planning" => {
                tools::Output::ok(format!("The user wants changes to the plan: {feedback}\nStay in plan mode and revise it."))
                    .brief("changes requested")
            }
            _ => tools::Output::ok("The user isn't ready to approve. Stay in plan mode, refine the plan, and ask again.")
                .brief("not yet"),
        }
    }

    async fn wait_for_answer(&self, text: String, options: Vec<String>, plan: bool) -> Option<String> {
        let (reply, answer) = oneshot::channel();
        (self.sink)(Event::Ask(Question { text, options, plan, reply }));
        answer.await.ok().filter(|a| !a.trim().is_empty())
    }

    /// Runs a group of `agent` calls in parallel and returns each one's result as a tool output.
    async fn run_subagents(&mut self, group: &[(String, ToolCall)]) -> Vec<tools::Output> {
        let mut jobs = Vec::new();
        for (id, call) in group {
            self.stats.tool_calls += 1;
            let args = match parse_args(call) {
                Ok(args) => args,
                Err(why) => {
                    self.tool_start(id, "agent (invalid arguments)");
                    jobs.push(Err(invalid_arguments(call, &why, false)));
                    continue;
                }
            };
            let label = args["description"].as_str().unwrap_or("subagent").to_string();
            self.tool_start(id, &tools::summary("agent", &args, &self.ctx.cwd));
            let prompt = args["prompt"].as_str().unwrap_or_default().to_string();
            if prompt.trim().is_empty() {
                jobs.push(Err(tools::Output::error("agent needs a `prompt`.")));
            } else {
                jobs.push(self.child(&label, args["fork"].as_bool().unwrap_or(false), prompt));
            }
        }
        self.run_children(jobs, MAX_PARALLEL_AGENTS).await
    }

    /// Runs an `agent_swarm` call: one fresh subagent per item, `swarm_parallel` at a time,
    /// and one result with every report in item order.
    async fn run_swarm(&mut self, id: &str, call: &ToolCall) -> tools::Output {
        self.stats.tool_calls += 1;
        let args = match parse_args(call) {
            Ok(args) => args,
            Err(why) => {
                self.tool_start(id, "agent_swarm (invalid arguments)");
                return invalid_arguments(call, &why, false);
            }
        };
        self.tool_start(id, &tools::summary("agent_swarm", &args, &self.ctx.cwd));
        let prompts = match swarm::prompts(&args) {
            Ok(prompts) => prompts,
            Err(why) => return tools::Output::error(why),
        };
        let description = args["description"].as_str().map(str::trim).filter(|d| !d.is_empty()).unwrap_or("swarm");
        let mut jobs = Vec::new();
        for (n, (_, prompt)) in prompts.iter().enumerate() {
            jobs.push(self.child(&format!("{description} #{}", n + 1), false, prompt.clone()));
        }
        let outputs = self.run_children(jobs, self.settings.swarm_parallel).await;
        let items: Vec<String> = prompts.into_iter().map(|(item, _)| item).collect();
        swarm::report(&self.ctx, &items, &outputs, MAX_RESULT_CHARS)
    }

    /// Runs subagents, at most `limit` at once, the next starting as soon as one finishes, and
    /// returns each one's result as a tool output, in order. One that couldn't start reports why.
    async fn run_children(&mut self, jobs: Vec<Job>, limit: usize) -> Vec<tools::Output> {
        let runs = jobs.into_iter().enumerate().map(|(index, job)| async move {
            let (mut agent, prompt) = match job {
                Ok(job) => job,
                Err(output) => return (index, output, None),
            };
            let output = match agent.run(&prompt, &[]).await {
                Ok((text, stop)) if !text.trim().is_empty() && stop != Stop::RepeatBreaker => tools::Output::ok(text),
                Ok((_, stop)) => tools::Output::error(format!("The subagent stopped without a final report ({stop:?}).")),
                Err(e) => tools::Output::error(format!("The subagent failed: {e:#}")),
            };
            (index, output, Some(agent))
        });
        let mut finished: Vec<_> = futures_util::stream::iter(runs).buffer_unordered(limit.max(1)).collect().await;
        finished.sort_by_key(|(index, ..)| *index);
        let mut outputs = Vec::new();
        for (_, output, agent) in finished {
            let Some(agent) = agent else {
                outputs.push(output);
                continue;
            };
            let usage = &agent.stats.usage;
            let total = &mut self.stats.usage;
            total.prompt += usage.prompt;
            total.cached += usage.cached;
            total.completion += usage.completion;
            total.reasoning += usage.reasoning;
            self.stats.requests += agent.stats.requests;
            self.stats.tool_calls += agent.stats.tool_calls;
            outputs.push(if output.is_error {
                output
            } else {
                output.brief(format!("{} requests, {} tool calls", agent.stats.requests, agent.stats.tool_calls))
            });
        }
        outputs
    }

    /// A subagent ready to run `prompt`, or why it couldn't start.
    fn child(&mut self, label: &str, fork: bool, prompt: String) -> Job {
        self.subagent(label, fork)
            .map(|agent| (agent, prompt))
            .map_err(|e| tools::Output::error(format!("Could not start the subagent: {e:#}")))
    }

    /// A subagent with the same model, system prompt and tools, so its requests hit the same
    /// cached prefix. A fork also starts with this conversation so far.
    fn subagent(&mut self, label: &str, fork: bool) -> Result<Agent> {
        self.children += 1;
        let mut session = self.session.child(self.children)?;
        let history: Vec<Value> =
            if fork { self.messages[..self.current_reply].to_vec() } else { vec![self.messages[0].clone()] };
        session.record(&json!({"type": "session", "id": session.id, "model": self.client.model.alias, "tools": *self.tools}))?;
        session.record(&json!({"type": "compaction", "messages": history}))?;
        let parent = self.sink.clone();
        let label = label.to_string();
        let sink: Sink = Rc::new(move |event| parent(Event::Sub { label: label.clone(), event: Box::new(event) }));
        Ok(Agent {
            client: self.client.clone(),
            tools: self.tools.clone(),
            messages: Rc::new(history),
            ctx: self.ctx.clone(),
            session,
            stats: Stats::default(),
            controls: Controls {
                steer: Rc::default(),
                plan: self.controls.plan.clone(),
                swarm: Rc::default(),
                interactive: false,
                aside: Rc::default(),
            },
            settings: self.settings.clone(),
            sink,
            ids: self.ids.clone(),
            repeats: HashMap::new(),
            reasoning_noted: false,
            todos: Vec::new(),
            plan_announced: self.plan_announced,
            swarm_announced: false,
            interrupted: false,
            depth: self.depth + 1,
            children: 0,
            current_reply: 0,
            session_started: true,
            instructions: self.instructions.clone(),
        })
    }

    /// Replaces the conversation with the user's prompts, word for word, and a summary of
    /// the work. The only time earlier messages change.
    pub async fn compact(&mut self, focus: Option<&str>) -> Result<()> {
        (self.sink)(Event::Notice("Compacting the conversation…".into()));
        let mut request = self.messages.to_vec();
        let mut instruction = COMPACT_INSTRUCTION.to_string();
        if let Some(focus) = focus.filter(|f| !f.trim().is_empty()) {
            instruction.push_str(&format!("\nThe user asked the summary to focus on: {focus}"));
        }
        request.push(json!({"role": "user", "content": instruction}));
        let reply = self.client.complete(&request, &self.tools, &self.session.id, &mut |_| {}).await?;
        self.stats.requests += 1;
        self.account(&reply.usage, None, None)?;
        if reply.text.trim().is_empty() {
            bail!("the model returned an empty summary, so the conversation was left as it was");
        }
        let mut kept = Vec::new();
        let mut size = 0;
        for prompt in self.user_prompts().into_iter().rev() {
            size += prompt.len();
            if size > KEPT_PROMPTS_CHARS && !kept.is_empty() {
                break;
            }
            kept.push(prompt);
        }
        kept.reverse();
        let requests: String = kept.iter().map(|p| format!("<request>\n{p}\n</request>\n")).collect();
        let summary = format!(
            "{mark} to save space. Below are the user's requests so far, word for word, and a summary of the work. Continue from where it left off; don't redo finished work.</system-reminder>\n\n<user-requests>\n{requests}</user-requests>\n\n<summary>\n{}\n</summary>",
            reply.text.trim(),
            mark = crate::session::COMPACTED_MARK,
        );
        self.messages = Rc::new(vec![self.messages[0].clone(), json!({"role": "user", "content": summary})]);
        self.share();
        self.session.record(&json!({"type": "compaction", "messages": *self.messages}))?;
        self.stats.context = 0;
        (self.sink)(Event::Notice("Compacted.".into()));
        Ok(())
    }

    /// After an interrupted turn, answers every tool call left without a result so the
    /// history stays valid, and notes the interruption for the next prompt.
    pub fn recover(&mut self) -> Result<()> {
        let missing = unanswered(&self.messages);
        for id in &missing {
            self.push(json!({"role": "tool", "tool_call_id": id, "content": "Interrupted by the user before this finished."}))?;
        }
        if !missing.is_empty() || self.messages.last().is_some_and(|m| m["role"] == "user") {
            self.interrupted = true;
        }
        Ok(())
    }

    /// Marks the next prompt as following an interruption.
    pub fn mark_interrupted(&mut self) {
        self.interrupted = true;
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
        format!("{head}\n\n[… {} characters omitted. {saved}]\n\n{}", tail_start - head.len(), &text[tail_start..])
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
        self.push_meta(message, None)
    }

    /// Appends a message. The log keeps `meta` beside a tool result, outside the message itself:
    /// how it ended and how long it took, which a resumed session shows again.
    fn push_meta(&mut self, message: Value, meta: Option<Value>) -> Result<()> {
        let mut entry = json!({"type": "message", "message": message});
        if let Some(meta) = meta {
            entry["tool"] = meta;
        }
        self.session.record(&entry)?;
        Rc::make_mut(&mut self.messages).push(message);
        self.share();
        Ok(())
    }
}

/// Whether a reply was billed for reasoning that never came back.
fn hides_reasoning(reply: &Reply) -> bool {
    reply.usage.reasoning > 0 && reply.reasoning.is_empty() && reply.reasoning_items.is_empty()
}

fn millis(took: Duration) -> u64 {
    took.as_millis().try_into().unwrap_or(u64::MAX)
}

fn hash_text(text: &str) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// An image in a user message: a line of text that says what it is, then the image.
fn image_parts(label: &str, url: &str) -> [Value; 2] {
    [json!({"type": "text", "text": label}), json!({"type": "image_url", "image_url": {"url": url}})]
}

fn parse_args(call: &ToolCall) -> Result<Value, String> {
    match call.arguments.trim() {
        "" => Ok(json!({})),
        raw => match serde_json::from_str::<Value>(raw) {
            Ok(args) if args.is_object() => Ok(args),
            Ok(_) => Err("arguments must be a JSON object".into()),
            Err(e) => match repair_json(raw).and_then(|fixed| serde_json::from_str::<Value>(&fixed).ok()) {
                Some(args) if args.is_object() => Ok(args),
                _ => Err(e.to_string()),
            },
        },
    }
}

/// Fixes common slips in tool-call JSON: a code fence around it, or trailing commas.
fn repair_json(raw: &str) -> Option<String> {
    let mut text = raw.trim();
    if let Some(inner) = text.strip_prefix("```json").or_else(|| text.strip_prefix("```")) {
        text = inner.strip_suffix("```").unwrap_or(inner).trim();
    }
    let fixed = regex::Regex::new(r",\s*([}\]])").ok()?.replace_all(text, "$1").into_owned();
    (fixed != raw).then_some(fixed)
}

fn invalid_arguments(call: &ToolCall, why: &str, truncated: bool) -> tools::Output {
    let cut = if truncated {
        " Your reply hit the output token limit, which cut the arguments off; send smaller pieces."
    } else {
        ""
    };
    tools::Output::error(format!("Invalid arguments for {}: {why}.{cut} You sent: {}", call.name, truncate(&call.arguments, 2000)))
}

/// Whether a provider error says the conversation no longer fits the context window.
fn is_overflow(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_ascii_lowercase();
    ["context length", "context_length", "maximum context", "context window", "prompt is too long", "too many tokens", "input is too long"]
        .iter()
        .any(|needle| text.contains(needle))
}

const BTW_NOTE: &str = "<system-reminder>A side question from the user. Answer it briefly from what this conversation already shows. Don't call tools. The question and your answer won't be kept in the conversation.</system-reminder>";

const COMPACT_INSTRUCTION: &str = "<system-reminder>The conversation is about to be compacted to save space. Write a summary that lets you continue the work without the earlier messages. Don't call tools. Use these sections:
## Goal
What the user wants, with their constraints and preferences.
## Progress
What is done and what is in progress.
## Decisions
Choices made and why.
## Files
Files read or changed, and what changed in each.
## Next steps
Exactly what to do next.
Be specific: names, paths, commands, error messages. Write in the user's language.</system-reminder>";

#[cfg(test)]
mod tests {
    use super::*;

    fn call(arguments: &str) -> ToolCall {
        ToolCall { id: "c".into(), name: "read".into(), arguments: arguments.into() }
    }

    #[test]
    fn reasoning_that_was_billed_but_never_sent_is_noticed() {
        let mut reply = Reply::default();
        assert!(!hides_reasoning(&reply));
        reply.usage.reasoning = 40;
        assert!(hides_reasoning(&reply));
        reply.reasoning = "thinking".into();
        assert!(!hides_reasoning(&reply));
        reply.reasoning.clear();
        reply.reasoning_items.push(json!({"type": "reasoning"}));
        assert!(!hides_reasoning(&reply));
    }

    #[test]
    fn repeats_count_and_notice_an_unchanged_result() {
        let mut repeat = Repeat::default();
        assert_eq!(repeat.note(1), (1, false));
        assert_eq!(repeat.note(1), (2, true));
        assert_eq!(repeat.note(2), (3, false));
        assert!(REPEAT_WARNINGS.iter().all(|&n| n < REPEAT_STOP));
    }

    #[test]
    fn arguments_with_common_slips_are_repaired() {
        assert_eq!(parse_args(&call("{\"path\": \"a\",}")).unwrap(), json!({"path": "a"}));
        assert_eq!(parse_args(&call("```json\n{\"path\": \"a\"}\n```")).unwrap(), json!({"path": "a"}));
        assert_eq!(parse_args(&call("")).unwrap(), json!({}));
        assert!(parse_args(&call("{\"path\": ")).is_err());
        assert!(parse_args(&call("[1]")).is_err());
    }

    #[test]
    fn overflow_errors_are_recognized() {
        assert!(is_overflow(&anyhow::anyhow!("HTTP 400: This model's maximum context length is 128000 tokens")));
        assert!(is_overflow(&anyhow::anyhow!("prompt is too long: 210000 tokens > 200000 maximum")));
        assert!(!is_overflow(&anyhow::anyhow!("HTTP 401: invalid key")));
    }

    #[test]
    fn a_side_question_mid_turn_answers_running_calls_first() {
        let history = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "fix it"}),
            json!({"role": "assistant", "tool_calls": [{"id": "a"}, {"id": "b"}]}),
            json!({"role": "tool", "tool_call_id": "a", "content": "done"}),
        ];
        let request = side_request(&history, "what are you doing?");
        assert_eq!(request[..4], history[..]);
        assert_eq!(request[4], json!({"role": "tool", "tool_call_id": "b", "content": "Still running; no result yet."}));
        assert_eq!(request[5]["role"], "user");
        assert!(request[5]["content"].as_str().unwrap().ends_with("\n\nwhat are you doing?"));
        assert_eq!(request.len(), 6);
        assert_eq!(side_request(&history[..2], "q").len(), 3);
    }
}
