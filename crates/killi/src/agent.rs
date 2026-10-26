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
use futures_util::{StreamExt, future::join_all};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::config::Thinking;
use crate::llm::{Client, Delta, IMAGE_TOKEN_RESERVE, Reply, ToolCall, Usage, truncate};
use crate::session::Session;
use crate::hooks::{HooksConfig, tool_payload};
use crate::skills::Skills;
use crate::{live, plan, swarm, tools};

const MAX_CONTINUATIONS: u32 = 3;
const REPEAT_WARNINGS: [u32; 3] = [3, 5, 7];
const REPEAT_STOP: u32 = 8;
const MAX_RESULT_CHARS: usize = 50_000;
const MAX_PARALLEL_AGENTS: usize = 4;
const MAX_SWARM_AGENTS_PER_TURN: usize = swarm::MAX_ITEMS;
const DEFAULT_OUTPUT_RESERVE: u64 = 4_096;

/// Where compaction aims as a share of the model's context window when `compact_at` is unset.
const COMPACT_AT_PERCENT: u64 = 60;

/// The compaction limit for a model whose context window is unknown.
const DEFAULT_COMPACT_AT: u64 = 256_000;
/// User prompts kept word for word through a compaction, in characters (about 20k tokens).
const KEPT_PROMPTS_CHARS: usize = 80_000;

/// One kept user request, whole when it fits, cut when it embeds a whole skill or document:
/// the words are the user's, but a pasted body is reloadable and its bulk would floor every
/// later summary at a size no compaction can shrink.
const KEPT_PROMPT_CHARS: usize = 4_000;

fn kept_prompt(prompt: &str) -> String {
    let chars = prompt.chars().count();
    if chars <= KEPT_PROMPT_CHARS {
        return prompt.to_string();
    }
    format!("{}\n[… {} characters cut]", truncate(prompt, KEPT_PROMPT_CHARS), chars - KEPT_PROMPT_CHARS)
}

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
    /// An `agent_swarm` call began: its tool call id, its description, and each subagent's item
    /// in order. Subagents are told apart from the call's other work by their `Slot`.
    Swarm { id: String, description: String, items: Vec<String> },
    /// A subagent began its work. Only seen inside `Sub`.
    Started,
    /// A subagent ended, and whether it failed. Only seen inside `Sub`.
    Finished { is_error: bool },
    /// Something a subagent did, labeled with its description, and where it sits in an
    /// `agent_swarm` call when it belongs to one.
    Sub { label: String, slot: Option<Slot>, event: Box<Event> },
}

/// A subagent's place in an `agent_swarm` call: the call's tool id and its item's index.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    pub swarm: String,
    pub index: usize,
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
    /// Where this session works among the live ones, so the model hears about the others in
    /// its tree; none for subagents and with `[agent] peers = false`.
    pub here: Option<Rc<live::Here>>,
    /// Messages from other sessions, as reminders, for the model's next step or prompt.
    pub peer_notes: Rc<RefCell<VecDeque<String>>>,
}

/// What a side question needs: the model, the tools (sent unchanged so the cached prefix
/// hits) and the conversation. It holds the history weakly, so appending to it never copies it.
#[derive(Clone, Default)]
pub struct Aside {
    client: Option<Rc<Client>>,
    tools: Rc<Vec<Value>>,
    messages: Weak<Vec<Value>>,
    /// What the tools are waiting on, for the question to mention.
    ctx: Weak<tools::Context>,
    cache_key: String,
}

impl Aside {
    /// Asks `question` about the conversation so far without adding to it. The answer and the
    /// request's usage come back; tool calls in the reply are ignored.
    pub fn ask(&self, question: &str) -> impl Future<Output = Result<(String, Usage)>> + 'static {
        self.ask_with(BTW_NOTE, question)
    }

    /// Like `ask`, under another note: for a question from another session.
    pub fn ask_with(&self, note: &str, question: &str) -> impl Future<Output = Result<(String, Usage)>> + 'static {
        let answer = self.side(note, question, self.waiting().as_deref());
        async move {
            let (text, usage) = answer.await?;
            Ok((if text.trim().is_empty() { "(No answer: the model tried to use tools.)".to_string() } else { text }, usage))
        }
    }

    /// A recap of the conversation so far, cut to `RECAP_CHARS`, asked the same way as a side
    /// question so it adds nothing to the conversation and the cached prefix still hits.
    pub fn recap(&self) -> impl Future<Output = Result<(String, Usage)>> + 'static {
        let answer = self.side(RECAP_NOTE, "", None);
        async move {
            let (text, usage) = answer.await?;
            let text = fit_recap(&text);
            if text.is_empty() {
                bail!("the model sent no recap");
            }
            Ok((text, usage))
        }
    }

    /// What the calls in flight are waiting on, as `/btw` is told it.
    pub fn waiting(&self) -> Option<String> {
        self.ctx.upgrade().and_then(|ctx| ctx.running.borrow().describe())
    }

    fn side(&self, note: &str, question: &str, running: Option<&str>) -> impl Future<Output = Result<(String, Usage)>> + 'static + use<> {
        let request = self.messages.upgrade().map(|history| side_request(&history, note, question, running));
        let (client, tools, cache_key) = (self.client.clone(), self.tools.clone(), self.cache_key.clone());
        async move {
            let (Some(client), Some(request)) = (client, request) else { bail!("there is no conversation to ask about yet") };
            let reply = client.complete(&request, &tools, &cache_key, &mut |_| {}).await?;
            Ok((reply.text, reply.usage))
        }
    }
}

/// A recap on one line of whitespace-collapsed text, at most `RECAP_CHARS` characters: the
/// limit holds whatever the model sent, so a long one is cut with an ellipsis.
fn fit_recap(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= RECAP_CHARS {
        return text;
    }
    let cut: String = text.chars().take(RECAP_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The history with a side question at the end. Tool calls still running get a placeholder
/// result first, as providers reject a question between a call and its result; `running` says
/// what they are waiting on and for how long, so "is it stuck?" has facts to go on.
fn side_request(history: &[Value], note: &str, question: &str, running: Option<&str>) -> Vec<Value> {
    let mut request = history.to_vec();
    for id in unanswered(history) {
        request.push(json!({"role": "tool", "tool_call_id": id, "content": "Still running; no result yet."}));
    }
    let running = running.map(|r| format!("<system-reminder>Waiting on right now:\n{r}</system-reminder>\n\n")).unwrap_or_default();
    request.push(json!({"role": "user", "content": format!("{note}\n\n{running}{question}").trim_end()}));
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
    /// Compact once billed input, an estimated appended suffix and output reserve pass this:
    /// an explicit limit in tokens, `None` for a share of the model's context window.
    pub compact_at: Option<u64>,
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
    /// Identical tool calls made in the current turn, by a hash of the call.
    repeats: HashMap<u64, Repeat>,
    /// History length at the last usage report; later messages are the unbilled suffix.
    accounted_messages: usize,
    /// Subagents started by swarm calls in this turn.
    swarm_used: usize,
    /// The log already says that the provider hides its reasoning.
    reasoning_noted: bool,
    todos: Vec<Todo>,
    plan_announced: bool,
    swarm_announced: bool,
    /// The other sessions in this tree the model was last told about, by pid.
    peers_announced: Vec<u32>,
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
            "version": env!("CARGO_PKG_VERSION"),
            "cwd": cwd,
            "model": agent.client.model.alias,
            "context_window": agent.client.model.context_window,
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
        agent.accounted_messages = loaded.messages.len();
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
        *ctx.search.borrow_mut() = settings.search.clone();
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
            accounted_messages: 0,
            swarm_used: 0,
            reasoning_noted: false,
            todos: Vec::new(),
            plan_announced: false,
            swarm_announced: false,
            peers_announced: Vec::new(),
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
            ctx: Rc::downgrade(&self.ctx),
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

    /// Points web_search at another service without touching the cached conversation, for
    /// /web-search-config.
    pub fn set_search(&mut self, search: crate::tools::SearchSettings) {
        let mut settings = (*self.settings).clone();
        settings.search = search.clone();
        self.settings = Rc::new(settings);
        *self.ctx.search.borrow_mut() = search;
    }

    /// Replaces the system prompt and settings on purpose, for /reload; the next request is
    /// uncached.
    pub fn reload_system(&mut self, system: &str, settings: Settings) -> Result<()> {
        self.instructions = crate::prompt::Instructions::new(&self.ctx.cwd, &settings.instruction_files);
        let search = settings.search.clone();
        self.settings = Rc::new(settings);
        *self.ctx.search.borrow_mut() = search;
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

    /// Kills the background tasks still running, giving their commands' first lines.
    pub fn stop_tasks(&self) -> Vec<String> {
        self.ctx.tasks.borrow_mut().stop_all()
    }

    /// The user prompts of this session, oldest first, for history and pickers.
    pub fn user_prompts(&self) -> Vec<String> {
        crate::session::user_prompts(&self.messages)
    }

    /// Runs one user turn to completion and returns the final reply text.
    pub async fn run(&mut self, user: &str, images: &[Image]) -> Result<(String, Stop)> {
        // The breaker is for a loop inside one turn; a call the user asks for again is not one.
        self.repeats.clear();
        self.swarm_used = 0;
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
        // Messages left while the session was idle come before the prompt, so it stays last.
        self.push_peer_notes()?;
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
        let mut told_left_running = false;
        let mut told_open_tasks = false;
        loop {
            self.before_request().await?;
            let reply = match self.request().await {
                Ok(reply) => reply,
                Err(error) if !overflow_compacted && is_overflow(&error) => {
                    overflow_compacted = true;
                    self.compact_with(None, CompactMode::Reduced).await?;
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
                // `-p` stops everything as it exits, so only a session that goes on asks.
                if self.depth == 0 && self.controls.interactive && !truncated && !told_left_running {
                    let left = self.ctx.tasks.borrow_mut().left_running();
                    if !left.is_empty() {
                        told_left_running = true;
                        let left = left.join(", ");
                        (self.sink)(Event::Notice(format!("Still running: {left}. Asked the agent to stop what nothing needs.")));
                        self.push(json!({"role": "user", "content": format!("<system-reminder>Still running: {left}. {LEFT_RUNNING}</system-reminder>")}))?;
                        continue;
                    }
                }
                if self.depth == 0
                    && self.controls.interactive
                    && !truncated
                    && !told_open_tasks
                    && let Some(note) = open_tasks_note(&self.todos)
                {
                    told_open_tasks = true;
                    (self.sink)(Event::Notice("The task list still has open items. Asked the agent to update it.".into()));
                    self.push(json!({"role": "user", "content": note}))?;
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
        self.push_peer_notes()?;
        if let Some(note) = self.peers_note() {
            self.push(json!({"role": "user", "content": note}))?;
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
        if self.compaction_due()
            && let Err(error) = self.compact(None).await
        {
            // Proactive compaction is a saving, not a need: the request may still fit, and an
            // overflow compacts the reduced history on the way back.
            (self.sink)(Event::Notice(format!("Compaction failed: {error:#}. Continuing without it.")));
        }
        Ok(())
    }

    /// Appends the messages other sessions left since the last step.
    fn push_peer_notes(&mut self) -> Result<()> {
        let notes: Vec<String> = self.controls.peer_notes.borrow_mut().drain(..).collect();
        for note in notes {
            self.push(json!({"role": "user", "content": note}))?;
        }
        Ok(())
    }

    /// A note for the model when the other sessions working in its tree have changed since it
    /// was last told: someone started or ended. A session alone costs nothing.
    fn peers_note(&mut self) -> Option<String> {
        let here = self.controls.here.as_ref().filter(|_| self.depth == 0)?;
        let peers = here.others();
        let pids: Vec<u32> = peers.iter().map(|e| e.pid).collect();
        if pids == self.peers_announced {
            return None;
        }
        self.peers_announced = pids;
        let shown = match peers.as_slice() {
            [] => "No other killi session works in this repository now; told the agent.".to_string(),
            [one] => format!("Session {} also works in this repository; told the agent to leave its changes alone.", one.pid),
            many => format!("{} other sessions also work in this repository; told the agent to leave their changes alone.", many.len()),
        };
        (self.sink)(Event::Notice(shown));
        Some(live::tree_note(&peers, live::now()))
    }

    fn compaction_due(&self) -> bool {
        self.compaction_pressure().is_some_and(|(context, suffix, limit, reserve)| context.saturating_add(suffix) > limit - reserve)
    }

    /// Why compaction is or isn't due: billed context and estimated suffix against the limit
    /// minus room for one reply (capped at a quarter of the limit, since the output cap already
    /// fits each request into the window). The log keeps these numbers at each compaction so a
    /// session that compacts too often shows which part forced it. `None` at limit 0 (off).
    fn compaction_pressure(&self) -> Option<(u64, u64, u64, u64)> {
        // A share of the window by default, an explicit `compact_at` instead, and never above
        // 85%, where the reply and the framing overhead start to squeeze the request.
        let limit = match self.client.model.context_window {
            Some(window) => self
                .settings
                .compact_at
                .unwrap_or(window * COMPACT_AT_PERCENT / 100)
                .min(window * 85 / 100),
            None => self.settings.compact_at.unwrap_or(DEFAULT_COMPACT_AT),
        };
        if limit == 0 {
            return None;
        }
        let suffix = estimate_tokens(&self.messages[self.accounted_messages.min(self.messages.len())..]);
        let reserve = self.client.model.max_output.unwrap_or(DEFAULT_OUTPUT_RESERVE).min(limit / 4);
        Some((self.stats.context, suffix, limit, reserve))
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
        self.account(&reply.usage, Some(&reply.finish_reason), Some(took), reply.reasoning_kept())?;
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

    /// `reasoning_kept` says whether this reply's reasoning came back in a form the log holds;
    /// billed but not kept is the dialect-agnostic mark of a provider hiding it on this API.
    fn account(&mut self, usage: &Usage, finish_reason: Option<&Option<String>>, took: Option<Duration>, reasoning_kept: bool) -> Result<()> {
        self.stats.first_prompt.get_or_insert(usage.prompt);
        self.stats.context = usage.prompt;
        self.accounted_messages = self.messages.len();
        let total = &mut self.stats.usage;
        total.prompt += usage.prompt;
        total.cached += usage.cached;
        total.completion += usage.completion;
        total.reasoning += usage.reasoning;
        (self.sink)(Event::Usage(usage.clone()));
        self.session.record(&json!({
            "type": "usage",
            "model": self.client.model.alias,
            "prompt": usage.prompt,
            "cached": usage.cached,
            "completion": usage.completion,
            "reasoning": usage.reasoning,
            "reasoning_kept": reasoning_kept,
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

    /// Runs a reply's tool calls in order; consecutive read-only file/search calls and `agent`
    /// calls run in parallel, and an `agent_swarm` call runs its subagents in parallel. Hooks
    /// keep every call serial. A message typed meanwhile waits for the calls running now, not
    /// for the ones after them: those are answered as not run.
    /// Returns true when the repeat breaker trips.
    async fn execute_calls(&mut self, calls: Vec<(String, ToolCall)>, truncated: bool) -> Result<bool> {
        let mut stop = false;
        let mut images = Vec::new();
        let mut i = 0;
        while i < calls.len() {
            if !self.controls.steer.borrow().is_empty() {
                for (id, _) in &calls[i..] {
                    self.push(json!({"role": "tool", "tool_call_id": id, "content": SKIPPED_FOR_MESSAGE}))?;
                }
                break;
            }
            if self.settings.hooks.is_empty() && parallel_read(&calls[i].1) {
                let end = calls[i..].iter().position(|(_, c)| !parallel_read(c)).map_or(calls.len(), |n| i + n);
                let started = Instant::now();
                let outputs = self.execute_reads(&calls[i..end], truncated).await;
                let took = started.elapsed();
                for ((id, call), output) in calls[i..end].iter().zip(outputs) {
                    if let Some(image) = &output.image {
                        images.push((tools::summary(&call.name, &parse_args(call).unwrap_or_default(), &self.ctx.cwd), image.clone()));
                    }
                    stop |= self.finish_call(id, call, output, took)?;
                }
                i = end;
                continue;
            }
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

    /// Runs a consecutive read-only group without a `Send` bound, then returns outputs in call
    /// order. Parsing, summaries and instruction-file discovery stay the same as for `execute`.
    async fn execute_reads(&mut self, group: &[(String, ToolCall)], truncated: bool) -> Vec<tools::Output> {
        let mut pending = Vec::with_capacity(group.len());
        for (id, call) in group {
            self.stats.tool_calls += 1;
            let mut invalid = None;
            let args = match parse_args(call) {
                Ok(args) => {
                    self.tool_start(id, &tools::summary(&call.name, &args, &self.ctx.cwd));
                    if let Some(path) = args["path"].as_str() {
                        self.instructions.touched(&tools::resolve(&self.ctx, path));
                    }
                    args
                }
                Err(why) => {
                    self.tool_start(id, &format!("{} (invalid arguments)", call.name));
                    invalid = Some(invalid_arguments(call, &why, truncated));
                    json!({})
                }
            };
            let ctx = self.ctx.clone();
            let name = call.name.clone();
            let steer = (self.depth == 0).then(|| self.controls.steer.clone());
            pending.push(async move {
                if let Some(output) = invalid {
                    output
                } else {
                    dispatch_tool(&ctx, &name, args, steer).await
                }
            });
        }
        join_all(pending).await
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
            _ => {
                // Only the top-level agent hears the user; a subagent waits out its own commands.
                let steer = (self.depth == 0).then(|| self.controls.steer.clone());
                dispatch_tool(&self.ctx, &call.name, args, steer).await
            }
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
        let (count, same) = self.repeats.entry(repeat_key(call, result)).or_default().note(result);
        if REPEAT_WARNINGS.contains(&count) {
            let warning = if same {
                " and got the same result. Repeating it won't change the result; try a different approach."
            } else {
                ". Its result changed, so this is a repetition warning, not a stale-result loop."
            };
            output.text.push_str(&format!(
                "\n\n<system-reminder>You have made this exact {} call {count} times{warning}</system-reminder>",
                call.name
            ));
        }
        let meta = json!({"name": call.name, "is_error": output.is_error, "brief": output.brief, "ms": millis(took)});
        self.push_meta(json!({"role": "tool", "tool_call_id": id, "content": output.text}), Some(meta))?;
        Ok(same && count >= REPEAT_STOP)
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
        let (text, options) = match question_args(args) {
            Ok(parsed) => parsed,
            Err(why) => return tools::Output::error(why),
        };
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
                jobs.push(self.child(&label, None, args["fork"].as_bool().unwrap_or(false), prompt));
            }
        }
        self.run_children(jobs, MAX_PARALLEL_AGENTS).await
    }

    /// Runs an `agent_swarm` call: one fresh subagent per item, `swarm_parallel` at a time,
    /// bounded by the turn's total swarm budget, with every report in item order.
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
        if let Err(why) = reserve_swarm(self.swarm_used, prompts.len()) {
            return tools::Output::error(why);
        }
        self.swarm_used += prompts.len();
        let description = args["description"].as_str().map(str::trim).filter(|d| !d.is_empty()).unwrap_or("swarm");
        let items: Vec<String> = prompts.iter().map(|(item, _)| item.clone()).collect();
        (self.sink)(Event::Swarm { id: id.into(), description: description.into(), items: items.clone() });
        let mut jobs = Vec::new();
        for (n, (_, prompt)) in prompts.into_iter().enumerate() {
            let slot = Slot { swarm: id.into(), index: n };
            jobs.push(self.child(&format!("{description} #{}", n + 1), Some(slot), false, prompt));
        }
        let outputs = self.run_children(jobs, self.settings.swarm_parallel).await;
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
            (agent.sink)(Event::Started);
            let output = match agent.run(&prompt, &[]).await {
                Ok((text, stop)) if !text.trim().is_empty() && stop != Stop::RepeatBreaker => tools::Output::ok(text),
                Ok((_, stop)) => tools::Output::error(format!("The subagent stopped without a final report ({stop:?}).")),
                Err(e) => tools::Output::error(format!("The subagent failed: {e:#}")),
            };
            (agent.sink)(Event::Finished { is_error: output.is_error });
            // Only its counts outlive it: its conversation and log file are freed here, not
            // when the slowest sibling of a swarm finishes.
            (index, output, Some(std::mem::take(&mut agent.stats)))
        });
        let mut finished: Vec<_> = futures_util::stream::iter(runs).buffer_unordered(limit.max(1)).collect().await;
        finished.sort_by_key(|(index, ..)| *index);
        let mut outputs = Vec::new();
        for (_, output, stats) in finished {
            let Some(stats) = stats else {
                outputs.push(output);
                continue;
            };
            let usage = &stats.usage;
            let total = &mut self.stats.usage;
            total.prompt += usage.prompt;
            total.cached += usage.cached;
            total.completion += usage.completion;
            total.reasoning += usage.reasoning;
            self.stats.requests += stats.requests;
            self.stats.tool_calls += stats.tool_calls;
            outputs.push(if output.is_error {
                output
            } else {
                output.brief(format!("{} requests, {} tool calls", stats.requests, stats.tool_calls))
            });
        }
        outputs
    }

    /// A subagent ready to run `prompt`, or why it couldn't start.
    fn child(&mut self, label: &str, slot: Option<Slot>, fork: bool, prompt: String) -> Job {
        self.subagent(label, slot, fork)
            .map(|agent| (agent, prompt))
            .map_err(|e| tools::Output::error(format!("Could not start the subagent: {e:#}")))
    }

    /// A subagent with the same model, system prompt and tools, so its requests hit the same
    /// cached prefix. A fork also starts with this conversation so far.
    fn subagent(&mut self, label: &str, slot: Option<Slot>, fork: bool) -> Result<Agent> {
        self.children += 1;
        let mut session = self.session.child(self.children)?;
        let history: Vec<Value> =
            if fork { self.messages[..self.current_reply].to_vec() } else { vec![self.messages[0].clone()] };
        session.record(&json!({"type": "session", "id": session.id, "version": env!("CARGO_PKG_VERSION"), "model": self.client.model.alias, "context_window": self.client.model.context_window, "tools": *self.tools}))?;
        session.record(&json!({"type": "compaction", "messages": history}))?;
        let parent = self.sink.clone();
        let label = label.to_string();
        let sink: Sink = Rc::new(move |event| parent(Event::Sub { label: label.clone(), slot: slot.clone(), event: Box::new(event) }));
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
                here: None,
                peer_notes: Rc::default(),
            },
            settings: self.settings.clone(),
            sink,
            ids: self.ids.clone(),
            repeats: HashMap::new(),
            accounted_messages: 0,
            swarm_used: 0,
            reasoning_noted: false,
            todos: Vec::new(),
            plan_announced: self.plan_announced,
            swarm_announced: false,
            peers_announced: Vec::new(),
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
        self.compact_with(focus, CompactMode::Full).await
    }

    async fn compact_with(&mut self, focus: Option<&str>, mode: CompactMode) -> Result<()> {
        (self.sink)(Event::Notice("Compacting the conversation…".into()));
        // Taken before the summary request accounts its own usage over these numbers.
        let (context, suffix, limit, reserve) = self.compaction_pressure().unwrap_or_default();
        let mut request = match mode {
            CompactMode::Full => self.messages.to_vec(),
            CompactMode::Reduced => reduced(&self.messages),
        };
        let mut instruction = COMPACT_INSTRUCTION.to_string();
        if let Some(focus) = focus.filter(|f| !f.trim().is_empty()) {
            instruction.push_str(&format!("\nThe user asked the summary to focus on: {focus}"));
        }
        let instruction = json!({"role": "user", "content": instruction});
        request.push(instruction.clone());
        let reply = match self.client.complete(&request, &self.tools, &self.session.id, &mut |_| {}).await {
            // Too long to be read even to summarize it: summarize it with its bulk cut.
            Err(error) if mode == CompactMode::Full && is_overflow(&error) => {
                request = reduced(&self.messages);
                request.push(instruction);
                self.client.complete(&request, &self.tools, &self.session.id, &mut |_| {}).await?
            }
            reply => reply?,
        };
        self.stats.requests += 1;
        self.account(&reply.usage, None, None, reply.reasoning_kept())?;
        if reply.text.trim().is_empty() {
            let instead = if reply.tool_calls.is_empty() {
                match reply.reasoning.is_empty() {
                    true => "returned an empty summary".to_string(),
                    false => format!("only reasoned, {} characters, and wrote no summary", reply.reasoning.chars().count()),
                }
            } else {
                let names: Vec<&str> = reply.tool_calls.iter().map(|call| call.name.as_str()).collect();
                format!("called {} instead of writing a summary", names.join(", "))
            };
            self.session.record(&json!({"type": "warn", "message": format!("Compaction: the model {instead}.")}))?;
            bail!("the model {instead}, so the conversation was left as it was");
        }
        let mut kept = Vec::new();
        let mut size = 0;
        for prompt in self.user_prompts().into_iter().rev() {
            size += prompt.chars().count().min(KEPT_PROMPT_CHARS);
            if size > KEPT_PROMPTS_CHARS && !kept.is_empty() {
                break;
            }
            kept.push(prompt);
        }
        kept.reverse();
        let requests: String = kept.iter().map(|p| format!("<request>\n{}\n</request>\n", kept_prompt(p))).collect();
        let summary = format!(
            "{mark} to save space. Below are the user's requests so far, word for word, and a summary of the work. Continue from where it left off; don't redo finished work.</system-reminder>\n\n<user-requests>\n{requests}</user-requests>\n\n<summary>\n{}\n</summary>",
            reply.text.trim(),
            mark = crate::session::COMPACTED_MARK,
        );
        self.messages = Rc::new(vec![self.messages[0].clone(), json!({"role": "user", "content": summary})]);
        // The reminders for plan mode, swarm mode and the other sessions went with the old
        // messages: the next step tells the model again what is still on.
        self.plan_announced = false;
        self.swarm_announced = false;
        self.peers_announced.clear();
        self.share();
        self.session.record(&json!({
            "type": "compaction",
            "cause": {"context": context, "suffix": suffix, "limit": limit, "reserve": reserve, "kept_chars": summary.chars().count()},
            "messages": *self.messages,
        }))?;
        self.stats.context = 0;
        self.accounted_messages = self.messages.len();
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

/// Whether a built-in call only reads files or search results and can share a read-only batch.
fn parallel_read(call: &ToolCall) -> bool {
    matches!(call.name.as_str(), "read" | "grep" | "glob")
}

/// Dispatches the tools that need only their shared context, without borrowing the agent.
async fn dispatch_tool(ctx: &tools::Context, name: &str, args: Value, steer: Option<Rc<RefCell<VecDeque<Steered>>>>) -> tools::Output {
    let wake = move || steer.as_ref().is_some_and(|steer| !steer.borrow().is_empty());
    tools::run(ctx, name, args, &wake).await
}

fn reserve_swarm(used: usize, requested: usize) -> Result<(), String> {
    let remaining = MAX_SWARM_AGENTS_PER_TURN.saturating_sub(used);
    if requested > remaining {
        return Err(format!(
            "This turn's swarm budget has {remaining} subagents left, but this call needs {requested}. Use fewer items or finish with the existing reports; the total limit is {MAX_SWARM_AGENTS_PER_TURN}."
        ));
    }
    Ok(())
}

/// A conservative character-based estimate for history appended since the last usage report.
/// The billed fixed prefix stays in `stats.context`; this estimates only the new suffix. An
/// inline image counts as `IMAGE_TOKEN_RESERVE`, not its base64: a 300 KB screenshot bills
/// about 1.5k tokens, and counted as text it compacted conversations of 8k tokens.
fn estimate_tokens(messages: &[Value]) -> u64 {
    messages.iter().fold(0_u64, |total, message| {
        total.saturating_add(estimate_chars(message).div_ceil(2)).saturating_add(4)
    })
}

/// Characters of `value` serialized, with each inline image's data replaced by its reserve.
fn estimate_chars(value: &Value) -> u64 {
    match value {
        Value::String(text) if text.starts_with("data:") && text.contains(";base64,") => IMAGE_TOKEN_RESERVE * 2,
        Value::Array(values) => values.iter().fold(2, |total, value| total.saturating_add(estimate_chars(value)).saturating_add(1)),
        Value::Object(values) => values.iter().fold(2, |total, (key, value)| {
            total.saturating_add(key.chars().count() as u64).saturating_add(4).saturating_add(estimate_chars(value))
        }),
        other => other.to_string().chars().count() as u64,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum CompactMode {
    Full,
    Reduced,
}

/// Characters of a tool result kept in a conversation cut down to be summarized.
const REDUCED_RESULT_CHARS: usize = 2_000;
const REDUCED_TEXT_CHARS: usize = 4_000;
const REDUCED_REASONING_CHARS: usize = 1_000;
const REDUCED_RECENT_MESSAGES: usize = 8;

/// `messages` with their bulk cut for a reduced summary request. The system prompt and recent
/// text stay verbatim; old prose keeps both ends, and old signed reasoning is dropped rather
/// than edited. This reduces the request conservatively, without promising a token-level fit.
fn reduced(messages: &[Value]) -> Vec<Value> {
    let recent_from = messages.len().saturating_sub(REDUCED_RECENT_MESSAGES);
    messages
        .iter()
        .enumerate()
        .map(|(index, original)| {
            let mut message = original.clone();
            let role = message["role"].as_str().unwrap_or_default().to_string();
            if role == "system" {
                return message;
            }
            if role == "tool" {
                if let Some(text) = message["content"].as_str().filter(|t| t.len() > REDUCED_RESULT_CHARS) {
                    message["content"] = format!("{}\n[… cut to fit the context window]", truncate(text, REDUCED_RESULT_CHARS)).into();
                }
                return message;
            }
            if let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) {
                parts.retain(|part| part["type"] != "image_url");
            }
            if index >= recent_from {
                return message;
            }
            let protected_user = role == "user"
                && message["content"].as_str().is_some_and(|text| {
                    text.starts_with(crate::session::COMPACTED_MARK) || text.contains("<system-reminder>")
                });
            if matches!(role.as_str(), "assistant" | "user") && !protected_user {
                match message.get_mut("content") {
                    Some(Value::String(text)) => reduce_text(text, REDUCED_TEXT_CHARS),
                    Some(Value::Array(parts)) => {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                let text = text.to_string();
                                part["text"] = cut_middle(&text, REDUCED_TEXT_CHARS).into();
                            }
                        }
                    }
                    _ => {}
                }
            }
            if role == "assistant" {
                // Reasoning is replayed verbatim. An oversized old reasoning bundle is dropped
                // as one bundle, never truncated or left inconsistent with its signed items.
                let reasoning_size = crate::llm::REASONING_KEYS
                    .iter()
                    .map(|key| message.get(*key).map_or(0, |value| value.to_string().len()))
                    .sum::<usize>()
                    + message.get("reasoning_items").map_or(0, |value| value.to_string().len());
                if reasoning_size > REDUCED_REASONING_CHARS {
                    let object = message.as_object_mut().expect("a message is an object");
                    for key in crate::llm::REASONING_KEYS {
                        object.remove(key);
                    }
                    object.remove("reasoning_items");
                }
            }
            message
        })
        .collect()
}

fn reduce_text(text: &mut String, limit: usize) {
    if text.len() > limit {
        *text = cut_middle(text, limit);
    }
}

/// Cuts the middle so both the request and constraints often placed at the end survive.
fn cut_middle(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    const MARK: &str = "\n[… middle cut for reduced compaction …]\n";
    let budget = limit.saturating_sub(MARK.len());
    let head = truncate(text, budget * 3 / 4);
    let tail_len = budget - head.len();
    let mut start = text.len() - tail_len;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("{head}{MARK}{}", &text[start..])
}

/// Whether a reply was billed for reasoning that never came back, on either channel.
fn hides_reasoning(reply: &Reply) -> bool {
    reply.usage.reasoning > 0 && !reply.reasoning_kept()
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

/// Identifies a tool call by its name and exact arguments, without keeping the arguments.
fn hash_call(call: &ToolCall) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    (&call.name, &call.arguments).hash(&mut hasher);
    hasher.finish()
}

/// What the repeat breaker counts as the same call. A wait on a background task is a poll:
/// asking again is the point, and its result says how long the task has run, so it only repeats
/// when the result does too. Every other call repeats whatever it returns.
fn repeat_key(call: &ToolCall, result: u64) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    if call.name != "task_output" {
        return hash_call(call);
    }
    let mut hasher = DefaultHasher::new();
    (hash_call(call), result).hash(&mut hasher);
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

/// The question and choices of an `ask_user_question` call. Malformed arguments are an error
/// for the model to fix, as asking the user a blank question or one with its choices dropped
/// leaves them nothing to answer.
fn question_args(args: &Value) -> Result<(String, Vec<String>), String> {
    const SHAPE: &str = "ask_user_question takes `question`, a string, and `options`, an array of 2 to 4 strings. Nothing was shown to the user; call it again.";
    let text = args["question"].as_str().map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(format!("`question` is missing or empty. {SHAPE}"));
    }
    let items = args["options"].as_array().map(Vec::as_slice).unwrap_or_default();
    let options: Vec<String> = items.iter().filter_map(option_text).collect();
    if options.len() < items.len() {
        return Err(format!("Every option must be a string, or an object with a `label`. {SHAPE}"));
    }
    Ok((text.to_string(), options))
}

/// An option as the user sees it: a string, or the `label` of an object with its `description`
/// after it, which is the shape other agents' question tools take.
fn option_text(option: &Value) -> Option<String> {
    if let Some(text) = option.as_str() {
        return Some(text.to_string());
    }
    let label = option["label"].as_str().map(str::trim).filter(|l| !l.is_empty())?;
    Some(match option["description"].as_str().map(str::trim).filter(|d| !d.is_empty()) {
        Some(description) => format!("{label} — {description}"),
        None => label.to_string(),
    })
}

/// Told to the agent when it ends a turn with its task list unfinished, which shows on the
/// user's screen as work still going: finished items are marked done, or the rest is carried on.
fn open_tasks_note(todos: &[Todo]) -> Option<String> {
    let open: Vec<&str> = todos.iter().filter(|t| t.status != "completed").map(|t| t.content.as_str()).collect();
    if open.is_empty() {
        return None;
    }
    let list = open.join("; ");
    Some(format!(
        "<system-reminder>Your task list still has {} open: {}. Call todo_write now with what is finished marked completed (and dropped items removed). If work is left, do it. Don't repeat your last reply.</system-reminder>",
        open.len(),
        truncate(&list, 400)
    ))
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

/// Told at the end of a turn that leaves commands running, after the list of them.
/// The result of a call that didn't run because the user sent a message before its turn.
const SKIPPED_FOR_MESSAGE: &str = "Not run: the user sent a message first, which follows. Run it again if it is still needed.";

const LEFT_RUNNING: &str = "Stop with task_stop what nothing needs anymore, such as a server or watcher started for a test or a check. If one should keep running, say so in your reply.";

/// Longest recap, in characters.
pub const RECAP_CHARS: usize = 100;

const RECAP_NOTE: &str = "<system-reminder>The user is back at this session and wants a recap. Reply with the recap only: what they are after, what was done, and the latest result or what is left. Use their language, at most 100 characters, one or two plain sentences, no markdown. Don't call tools. This request and your reply won't be kept in the conversation.</system-reminder>";

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
Be specific: names, paths, commands, error messages. Keep the whole summary under about 1,500 tokens: the newest state matters more than the history, and command transcripts don't belong in it. Write in the user's language.</system-reminder>";

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
    fn a_question_without_text_or_with_unusable_options_is_not_asked() {
        let (text, options) = question_args(&json!({"question": " Which? ", "options": ["a", "b"]})).unwrap();
        assert_eq!((text.as_str(), options), ("Which?", vec!["a".to_string(), "b".to_string()]));
        assert!(question_args(&json!({"question": "Which?"})).unwrap().1.is_empty());
        assert!(question_args(&json!({"options": ["a", "b"]})).is_err());
        assert!(question_args(&json!({"question": "  ", "options": ["a"]})).is_err());
        let unusable = json!({"question": "Which?", "options": [{"description": "a", "question": "b"}, "c"]});
        assert!(question_args(&unusable).unwrap_err().contains("`label`"));
        assert!(question_args(&json!({"question": "Which?", "options": [{"label": " "}, "c"]})).is_err());
    }

    #[test]
    fn options_given_as_labelled_objects_keep_their_description() {
        let options = json!([{"label": "Banner", "description": "smallest change"}, {"label": "Status line"}, "Both"]);
        let (_, options) = question_args(&json!({"question": "Which?", "options": options})).unwrap();
        assert_eq!(options, ["Banner — smallest change", "Status line", "Both"]);
    }

    #[test]
    fn a_poll_repeats_only_when_its_result_does() {
        let wait = ToolCall { id: "a".into(), name: "task_output".into(), arguments: r#"{"id":3,"wait":300}"#.into() };
        assert_ne!(repeat_key(&wait, 1), repeat_key(&wait, 2));
        assert_eq!(repeat_key(&wait, 1), repeat_key(&wait, 1));
        let read = call(r#"{"path":"a"}"#);
        assert_eq!(repeat_key(&read, 1), repeat_key(&read, 2));
    }

    #[test]
    fn changed_results_warn_but_only_unchanged_results_hit_the_repeat_stop() {
        let read = call(r#"{"path":"a"}"#);
        let mut changing = idle_agent("repeat-changing", Controls::default(), Rc::new(|_| {}));
        for n in 0..REPEAT_STOP {
            let output = tools::Output::ok(format!("result {n}"));
            assert!(!changing.finish_call(&format!("c{n}"), &read, output, Duration::ZERO).unwrap());
        }
        let warnings = changing
            .messages
            .iter()
            .filter(|message| message["content"].as_str().is_some_and(|text| text.contains("not a stale-result loop")))
            .count();
        assert_eq!(warnings, REPEAT_WARNINGS.len());
        let mut unchanged = idle_agent("repeat-unchanged", Controls::default(), Rc::new(|_| {}));
        for n in 0..REPEAT_STOP {
            let stop = unchanged
                .finish_call(&format!("c{n}"), &read, tools::Output::ok("same result"), Duration::ZERO)
                .unwrap();
            assert_eq!(stop, n + 1 == REPEAT_STOP);
        }
    }

    #[test]
    fn a_recap_is_one_line_within_the_limit() {
        assert_eq!(fit_recap("  Fixed it.\n\nTests pass. "), "Fixed it. Tests pass.");
        let long = "字".repeat(RECAP_CHARS + 30);
        let cut = fit_recap(&long);
        assert_eq!(cut.chars().count(), RECAP_CHARS);
        assert!(cut.ends_with('…'));
        assert_eq!(fit_recap(&"字".repeat(RECAP_CHARS)).chars().count(), RECAP_CHARS);
        assert!(RECAP_NOTE.contains(&format!("{RECAP_CHARS} characters")));
    }

    #[test]
    fn a_recap_request_ends_with_its_note_alone() {
        let history = vec![json!({"role": "user", "content": "fix it"}), json!({"role": "assistant", "content": "done"})];
        let request = side_request(&history, RECAP_NOTE, "", None);
        assert_eq!(request[..2], history[..]);
        assert_eq!(request[2]["content"], RECAP_NOTE);
        assert_eq!(request.len(), 3);
    }

    #[test]
    fn a_finished_task_list_is_not_mentioned_but_an_open_one_is() {
        let todo = |content: &str, status: &str| Todo { content: content.into(), status: status.into() };
        assert!(open_tasks_note(&[]).is_none());
        assert!(open_tasks_note(&[todo("a", "completed")]).is_none());
        let note = open_tasks_note(&[todo("a", "completed"), todo("b", "in_progress"), todo("c", "pending")]).unwrap();
        assert!(note.contains("2 open: b; c") && note.starts_with("<system-reminder>"));
    }

    #[test]
    fn a_conversation_cut_down_to_be_summarized_keeps_its_shape() {
        let long = "x".repeat(10_000);
        let history = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "fix it"}),
            json!({"role": "assistant", "tool_calls": [{"id": "a"}]}),
            json!({"role": "tool", "tool_call_id": "a", "content": long}),
            json!({"role": "tool", "tool_call_id": "b", "content": "short"}),
            json!({"role": "user", "content": image_parts("Image from a.png:", "data:image/png;base64,AAAA")}),
        ];
        let cut = reduced(&history);
        assert_eq!(cut.len(), history.len());
        assert_eq!(cut[..3], history[..3]);
        assert_eq!(cut[3]["tool_call_id"], "a");
        let text = cut[3]["content"].as_str().unwrap();
        assert!(text.starts_with(&"x".repeat(REDUCED_RESULT_CHARS)) && text.ends_with("[… cut to fit the context window]"), "{}", text.len());
        assert_eq!(cut[4], history[4]);
        assert_eq!(cut[5]["content"], json!([{"type": "text", "text": "Image from a.png:"}]));
    }

    #[test]
    fn reduced_history_bounds_old_prose_and_reasoning_but_protects_recent_context() {
        let long = |tail: &str| format!("{}{tail}", "x".repeat(12_000));
        let history = vec![
            json!({"role": "system", "content": long("SYSTEM")}),
            json!({"role": "user", "content": long("USER CONSTRAINT AT THE END")}),
            json!({
                "role": "assistant",
                "content": long("ASSISTANT END"),
                "reasoning_content": long("REASONING END"),
                "reasoning_items": [{"type": "thinking", "thinking": "x".repeat(2_000), "signature": "signed"}]
            }),
            json!({"role": "user", "content": format!("{} keep requests", crate::session::COMPACTED_MARK)}),
            json!({"role": "assistant", "content": "one"}),
            json!({"role": "assistant", "content": "two"}),
            json!({"role": "assistant", "content": "three"}),
            json!({"role": "assistant", "content": "four"}),
            json!({"role": "assistant", "content": "five"}),
            json!({"role": "assistant", "content": "six"}),
            json!({"role": "assistant", "content": "seven"}),
            json!({"role": "assistant", "content": long("RECENT END"), "reasoning_items": [{"encrypted_content": "keep"}]}),
        ];
        let cut = reduced(&history);
        assert_eq!(cut[0], history[0]);
        for (index, tail) in [(1, "USER CONSTRAINT AT THE END"), (2, "ASSISTANT END")] {
            let text = cut[index]["content"].as_str().unwrap();
            assert!(text.len() <= REDUCED_TEXT_CHARS, "{}", text.len());
            assert!(text.ends_with(tail));
        }
        assert!(cut[2].get("reasoning_content").is_none(), "oversized reasoning is dropped as one bundle");
        assert!(cut[2].get("reasoning_items").is_none(), "signed reasoning is dropped, not edited");
        assert_eq!(cut[3], history[3], "compacted user requests and constraints stay verbatim");
        assert_eq!(cut[11], history[11], "recent messages stay verbatim");
    }

    #[test]
    fn identical_calls_share_a_key_and_others_do_not() {
        let call = |name: &str, arguments: &str| ToolCall { id: "c".into(), name: name.into(), arguments: arguments.into() };
        assert_eq!(hash_call(&call("read", "{\"path\":\"a\"}")), hash_call(&call("read", "{\"path\":\"a\"}")));
        assert_ne!(hash_call(&call("read", "{\"path\":\"a\"}")), hash_call(&call("read", "{\"path\":\"b\"}")));
        assert_ne!(hash_call(&call("read", "x")), hash_call(&call("write", "x")));
        assert_ne!(hash_call(&call("ab", "c")), hash_call(&call("a", "bc")));
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
        let request = side_request(&history, BTW_NOTE, "what are you doing?", None);
        assert_eq!(request[..4], history[..]);
        assert_eq!(request[4], json!({"role": "tool", "tool_call_id": "b", "content": "Still running; no result yet."}));
        assert_eq!(request[5]["role"], "user");
        assert!(request[5]["content"].as_str().unwrap().ends_with("\n\nwhat are you doing?"));
        assert_eq!(request.len(), 6);
        assert_eq!(side_request(&history[..2], BTW_NOTE, "q", None).len(), 3);
    }

    /// An agent that has never asked a model anything: enough to run a reply's tool calls.
    fn idle_agent(name: &str, controls: Controls, sink: Sink) -> Agent {
        let dir = std::env::temp_dir().join(format!("killi-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("session.jsonl");
        std::fs::write(&log, "").unwrap();
        let model = crate::config::ResolvedModel {
            alias: "test".into(),
            model: "test".into(),
            protocol: crate::config::Protocol::OpenAi,
            base_url: String::new(),
            api_key: String::new(),
            headers: Default::default(),
            session_header: None,
            max_output: None,
            context_window: None,
            images: false,
            thinking: Thinking::Off,
            cache_control: false,
            chatgpt: false,
        };
        let client = Client::new(model).unwrap();
        Agent::assemble(client, &dir, Vec::new(), Settings::default(), controls, sink, Session::open(&log).unwrap())
    }

    #[test]
    fn only_read_only_file_and_search_calls_form_parallel_groups() {
        for name in ["read", "grep", "glob"] {
            assert!(parallel_read(&ToolCall { id: "c".into(), name: name.into(), arguments: "{}".into() }));
        }
        for name in ["write", "edit", "bash", "ask_user_question", "task_output"] {
            assert!(!parallel_read(&ToolCall { id: "c".into(), name: name.into(), arguments: "{}".into() }));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_batches_start_together_and_write_results_in_call_order() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let seen = events.clone();
        let sink: Sink = Rc::new(move |event| {
            seen.borrow_mut().push(match event {
                Event::ToolStart { id, .. } => format!("start:{id}"),
                Event::ToolEnd { id, .. } => format!("end:{id}"),
                _ => return,
            });
        });
        let mut agent = idle_agent("parallel-reads", Controls::default(), sink);
        for (name, text) in [("a.txt", "alpha"), ("b.txt", "beta"), ("c.txt", "gamma")] {
            std::fs::write(agent.ctx.cwd.join(name), text).unwrap();
        }
        let calls: Vec<_> = [("a", "a.txt"), ("b", "b.txt"), ("c", "c.txt")]
            .into_iter()
            .map(|(id, path)| {
                (id.to_string(), ToolCall { id: id.into(), name: "read".into(), arguments: json!({"path": path}).to_string() })
            })
            .collect();
        assert!(!agent.execute_calls(calls, false).await.unwrap());
        assert_eq!(
            *events.borrow(),
            ["start:a", "start:b", "start:c", "end:a", "end:b", "end:c"]
        );
        let results: Vec<_> = agent.messages.iter().map(|m| (m["tool_call_id"].as_str().unwrap(), m["content"].as_str().unwrap())).collect();
        assert_eq!(results.iter().map(|(id, _)| *id).collect::<Vec<_>>(), ["a", "b", "c"]);
        for ((_, text), expected) in results.into_iter().zip(["alpha", "beta", "gamma"]) {
            assert!(text.contains(expected), "{text}");
        }
        assert_eq!(agent.stats.tool_calls, 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn configured_hooks_keep_read_calls_serial() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let seen = events.clone();
        let sink: Sink = Rc::new(move |event| {
            seen.borrow_mut().push(match event {
                Event::ToolStart { id, .. } => format!("start:{id}"),
                Event::ToolEnd { id, .. } => format!("end:{id}"),
                _ => return,
            });
        });
        let mut agent = idle_agent("serial-hooks", Controls::default(), sink);
        let hooks = toml::from_str("[[PreToolUse]]\ncommand = ':'\n").unwrap();
        agent.settings = Rc::new(Settings { hooks: Rc::new(hooks), ..Settings::default() });
        std::fs::write(agent.ctx.cwd.join("a.txt"), "alpha").unwrap();
        let calls: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|id| {
                (id.to_string(), ToolCall { id: id.into(), name: "read".into(), arguments: r#"{"path":"a.txt"}"#.into() })
            })
            .collect();
        agent.execute_calls(calls, false).await.unwrap();
        assert_eq!(*events.borrow(), ["start:a", "end:a", "start:b", "end:b"]);
    }

    #[test]
    fn compaction_estimates_only_the_appended_suffix_and_reserves_output() {
        let mut agent = idle_agent("compaction-estimate", Controls::default(), Rc::new(|_| {}));
        agent.settings = Rc::new(Settings { compact_at: Some(10_000), ..Settings::default() });
        Rc::get_mut(&mut agent.client).unwrap().model.max_output = Some(1_000);
        agent.stats.context = 9_000;
        assert!(!agent.compaction_due(), "the exact input-plus-output boundary is still allowed");
        agent.stats.context = 9_001;
        assert!(agent.compaction_due(), "the output reserve counts before the next request");

        agent.stats.context = 100;
        agent.push(json!({"role": "user", "content": "old ".repeat(20_000)})).unwrap();
        agent.accounted_messages = agent.messages.len();
        assert!(!agent.compaction_due(), "the fixed prefix is not estimated a second time");
        agent.push(json!({"role": "user", "content": "new ".repeat(5_000)})).unwrap();
        assert!(agent.compaction_due(), "the unbilled appended suffix counts conservatively");
    }

    #[test]
    fn an_inline_image_counts_as_its_reserve_not_its_base64() {
        let mut agent = idle_agent("compaction-image", Controls::default(), Rc::new(|_| {}));
        agent.settings = Rc::new(Settings { compact_at: Some(10_000), ..Settings::default() });
        Rc::get_mut(&mut agent.client).unwrap().model.max_output = Some(1_000);
        agent.stats.context = 100;
        agent.accounted_messages = agent.messages.len();
        let data = format!("data:image/jpeg;base64,{}", "A".repeat(400_000));
        let image = json!({"role": "user", "content": [
            {"type": "text", "text": "Image from read shot.png:"},
            {"type": "image_url", "image_url": {"url": data}},
        ]});
        let estimate = estimate_tokens(std::slice::from_ref(&image));
        assert!((IMAGE_TOKEN_RESERVE..IMAGE_TOKEN_RESERVE + 100).contains(&estimate), "{estimate}");
        agent.push(image).unwrap();
        assert!(!agent.compaction_due(), "a screenshot in an 8k-token conversation is not a reason to compact");
        agent.push(json!({"role": "user", "content": "A".repeat(400_000)})).unwrap();
        assert!(agent.compaction_due(), "the unbilled appended suffix counts conservatively");
    }

    #[test]
    fn compaction_aims_at_sixty_percent_of_the_window_by_default() {
        let mut agent = idle_agent("compaction-window", Controls::default(), Rc::new(|_| {}));
        let client = Rc::get_mut(&mut agent.client).unwrap();
        client.model.context_window = Some(1_000_000);
        client.model.max_output = Some(128_000);
        agent.accounted_messages = agent.messages.len();
        // limit = 600k, reserve = min(128k, 150k): a quarter of the limit caps the reserve.
        agent.stats.context = 472_000;
        assert!(!agent.compaction_due(), "the boundary is still allowed");
        agent.stats.context = 472_001;
        assert!(agent.compaction_due(), "the capped reserve counts before the next request");
    }

    #[test]
    fn an_explicit_compact_at_overrides_the_window_share_and_zero_disables() {
        let mut agent = idle_agent("compaction-explicit", Controls::default(), Rc::new(|_| {}));
        let client = Rc::get_mut(&mut agent.client).unwrap();
        client.model.context_window = Some(1_000_000);
        client.model.max_output = Some(128_000);
        agent.accounted_messages = agent.messages.len();
        agent.stats.context = 8_000;
        assert!(!agent.compaction_due(), "without a setting the window share applies");
        agent.settings = Rc::new(Settings { compact_at: Some(10_000), ..Settings::default() });
        // reserve = min(128k, 2.5k), so the smaller explicit limit still leaves reply room.
        agent.stats.context = 7_500;
        assert!(!agent.compaction_due());
        agent.stats.context = 7_501;
        assert!(agent.compaction_due(), "the explicit limit wins over the window share");
        agent.settings = Rc::new(Settings { compact_at: Some(0), ..Settings::default() });
        agent.stats.context = 900_000;
        assert!(!agent.compaction_due(), "zero keeps proactive compaction off");
    }

    #[test]
    fn an_unknown_window_falls_back_to_a_fixed_limit() {
        let mut agent = idle_agent("compaction-unknown", Controls::default(), Rc::new(|_| {}));
        Rc::get_mut(&mut agent.client).unwrap().model.max_output = None;
        agent.accounted_messages = agent.messages.len();
        agent.stats.context = DEFAULT_COMPACT_AT - DEFAULT_OUTPUT_RESERVE;
        assert!(!agent.compaction_due());
        agent.stats.context += 1;
        assert!(agent.compaction_due(), "the default reserve applies without an output cap");
    }

    #[test]
    fn a_kept_prompt_that_embeds_a_whole_document_is_cut() {
        let long = "x".repeat(9_000);
        let kept = kept_prompt(&long);
        assert!(kept.starts_with(&"x".repeat(4_000)) && kept.contains("[… 5000 characters cut]"), "{}", kept.len());
        assert_eq!(kept_prompt("plain"), "plain");
    }

    #[test]
    fn swarm_budget_is_total_per_turn_not_just_concurrency() {
        assert!(reserve_swarm(0, MAX_SWARM_AGENTS_PER_TURN).is_ok());
        assert!(reserve_swarm(1, MAX_SWARM_AGENTS_PER_TURN).is_err());
        assert!(reserve_swarm(MAX_SWARM_AGENTS_PER_TURN, 2).is_err());
        assert!(reserve_swarm(MAX_SWARM_AGENTS_PER_TURN - 2, 2).is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_message_typed_during_a_call_goes_ahead_of_the_calls_after_it() {
        let controls = Controls::default();
        let steer = controls.steer.clone();
        let sink: Sink = Rc::new(move |event| {
            if let Event::ToolStart { id, .. } = event
                && id == "a"
            {
                steer.borrow_mut().push_back(("stop".into(), Vec::new()));
            }
        });
        let mut agent = idle_agent("steer", controls, sink);
        let calls: Vec<_> = ["a", "b", "c"]
            .iter()
            .map(|id| (id.to_string(), ToolCall { id: id.to_string(), name: "todo_write".into(), arguments: r#"{"todos":[]}"#.into() }))
            .collect();
        let ids: Vec<_> = calls.iter().map(|(id, _)| json!({"id": id})).collect();
        agent.push(json!({"role": "assistant", "tool_calls": ids})).unwrap();
        agent.execute_calls(calls, false).await.unwrap();
        let results: Vec<_> = agent.messages[1..].iter().map(|m| (m["tool_call_id"].as_str().unwrap(), m["content"].as_str().unwrap())).collect();
        assert!(results[0].1.starts_with("Task list updated"), "{results:?}");
        assert_eq!(results[1..], [("b", SKIPPED_FOR_MESSAGE), ("c", SKIPPED_FOR_MESSAGE)]);
        assert!(unanswered(&agent.messages).is_empty());
        assert_eq!(agent.controls.steer.borrow().len(), 1, "the message waits for the next request");
        agent.before_request().await.unwrap();
        assert_eq!(agent.messages.last().unwrap(), &json!({"role": "user", "content": "stop"}));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_model_hears_once_when_the_sessions_in_its_tree_change_and_gets_their_messages() {
        let dir = std::env::temp_dir().join(format!("killi-peers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let entry = |pid, tree: &str| live::Entry { pid, tree: tree.into(), cwd: tree.into(), ..live::Entry::default() };
        let me = live::Registration::open_in(&dir, entry(1, "/w")).unwrap();
        let other = live::Registration::open_in(&dir, entry(2, "/w")).unwrap();
        let _elsewhere = live::Registration::open_in(&dir, entry(3, "/x")).unwrap();
        let notices = Rc::new(RefCell::new(Vec::new()));
        let seen = notices.clone();
        let sink: Sink = Rc::new(move |event| {
            if let Event::Notice(text) = event {
                seen.borrow_mut().push(text);
            }
        });
        let controls = Controls { here: Some(Rc::new(me.here())), ..Controls::default() };
        let mut agent = idle_agent("peers", controls.clone(), sink);
        let last = |agent: &Agent| agent.messages.last().and_then(|m| m["content"].as_str()).unwrap_or_default().to_string();

        agent.before_request().await.unwrap();
        assert!(last(&agent).starts_with("<system-reminder>Another killi session works in this repository too: 2 · idle"), "{}", last(&agent));
        assert_eq!(notices.borrow().len(), 1);
        agent.before_request().await.unwrap();
        assert_eq!(agent.messages.len(), 1, "nothing new while nothing changes");

        controls.peer_notes.borrow_mut().push_back(live::tell_note("killi session 2", Some(2), "pushed"));
        drop(other);
        agent.before_request().await.unwrap();
        assert!(agent.messages[1]["content"].as_str().unwrap().contains("<message>\npushed\n</message>"));
        assert!(last(&agent).contains("No other killi session works in this repository now"));
        assert!(controls.peer_notes.borrow().is_empty());

        let mut child = agent.subagent("child", None, false).unwrap();
        let _again = live::Registration::open_in(&dir, entry(4, "/w")).unwrap();
        child.before_request().await.unwrap();
        assert_eq!(child.messages.len(), 1, "a subagent is never told: only its system prompt");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Runs one `bash` call while a message from the user is typed, for an agent at `depth`.
    async fn bash_with_a_message(command: &str, depth: u8) -> (String, Duration) {
        let controls = Controls::default();
        let steer = controls.steer.clone();
        let sink: Sink = Rc::new(move |event| {
            if let Event::ToolStart { .. } = event {
                steer.borrow_mut().push_back(("are you stuck?".into(), Vec::new()));
            }
        });
        let mut agent = idle_agent("wake", controls, sink);
        agent.depth = depth;
        let call = ToolCall { id: "a".into(), name: "bash".into(), arguments: json!({"command": command, "timeout": 60}).to_string() };
        let started = Instant::now();
        let output = agent.execute("a", &call, false).await;
        (output.text, started.elapsed())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_message_typed_during_a_long_command_does_not_wait_for_it() {
        let (text, took) = bash_with_a_message("sleep 20", 0).await;
        assert!(took < Duration::from_secs(5), "waited {took:?}");
        assert!(text.contains("A message from the user is waiting, so it moved to background task 1"), "{text}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_subagent_waits_out_its_command_whatever_the_user_types() {
        let (text, _) = bash_with_a_message("sleep 1; echo done", 1).await;
        assert_eq!(text, "done");
    }

    #[test]
    fn a_side_question_is_told_what_the_running_call_waits_on() {
        let history = vec![json!({"role": "user", "content": "wait for ci"})];
        let request = side_request(&history, BTW_NOTE, "stuck?", Some("bash `sleep 5` has run 4m12s, no output yet."));
        let note = request[1]["content"].as_str().unwrap();
        assert!(note.contains("Waiting on right now:\nbash `sleep 5` has run 4m12s, no output yet.</system-reminder>"), "{note}");
        assert!(note.ends_with("\n\nstuck?"));
    }
}
