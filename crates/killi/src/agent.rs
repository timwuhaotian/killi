//! The agent loop. History is append-only and holds messages exactly as sent; the system
//! prompt and tool list are fixed for the session, so every request reuses the cached prefix.
//! Notes from killi to the model are appended as user messages, never edited into old ones.

mod advisor;
mod ask;
mod background;
mod compact;
mod dispatch;
mod ending;
mod held;
mod history;
mod lifecycle;
mod near;
mod pair;
mod peer_guard;
mod request;
mod restore;
mod step_limit;
mod subagent;
mod task;
mod todos;
mod turn;
mod types;

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use futures_util::{StreamExt, future::join_all, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::config::Thinking;
use crate::hooks::{HooksConfig, tool_payload};
use crate::llm::{
    Client, Delta, ErrorKind, IMAGE_OMITTED, IMAGE_TOKEN_RESERVE, ProviderError, Reply, ToolCall,
    Usage, truncate,
};
use crate::session::Session;
use crate::skills::Skills;
use crate::verbosity::{self, Verbosity};
use crate::{goal, live, plan, swarm, tools};

pub use background::{BgFuture, BgState, BgTask, MAX_BACKGROUND};
pub use pair::{DONE_MARK, MAX_ROUNDS, Outcome, Pair, PairConfig, PairControls, Role, RoleCounts};
pub use types::{
    Aside, Controls, Event, Image, Question, Settings, Sink, Slot, Stats, Steered, Stop, Todo,
};

pub(crate) use subagent::subagent_prompt;
pub(crate) use task::task_agent;

use background::Background;
use compact::CompactMode;
pub(crate) use dispatch::neutralize_reminder_tags;
use dispatch::{invalid_arguments, parse_args};
use ending::{
    ANNOUNCED_AGAIN, CUT_THINKING, MAX_UNFINISHED, answer, bookkeeping, ends_with_question,
    interrupted_note, thought_only, unfinished,
};
use near::{NEAR_BASH_WARN_AT, NEAR_REPEAT_WARN_AT, near_key};
use request::{is_media, is_overflow, provider_limit, reasoning_evidence};
use todos::{STALE_TASKS_AFTER, open_tasks_note, stale_tasks_note, tasks_note};
use types::{
    PartialSwarm, PendingPrompt, Repeat, last_model_state, last_plan_state, model_note, unanswered,
};

#[cfg(test)]
use ask::question_args;
#[cfg(test)]
use compact::{
    COMPACT_INSTRUCTION, DEFAULT_COMPACT_AT, DEFAULT_OUTPUT_RESERVE, REDUCED_RESULT_CHARS,
    REDUCED_TEXT_CHARS, cut_at, estimate_tokens, kept_prompt, reduced, tail_start,
};
#[cfg(test)]
use dispatch::{
    REPEAT_BLOCK_AFTER, REPEAT_STOP, REPEAT_WARNINGS, SKIPPED_FOR_MESSAGE, dispatch_tool,
    hash_call, image_parts, parallel_read, repeat_key,
};
#[cfg(test)]
use ending::{ends_with_colon, ends_with_intent};
#[cfg(test)]
use request::hides_reasoning;
#[cfg(test)]
use subagent::{MAX_SWARM_AGENTS_PER_TURN, reserve_swarm};
#[cfg(test)]
use types::{
    BTW_NOTE, COMPLETE_MAX, COMPLETE_NOTE, RECAP_CHARS, RECAP_MAX, RECAP_NOTE, SUGGEST_MAX,
    SUGGEST_NOTE, fit_completion, fit_recap, fit_suggestion, side_request,
};

/// A backstop in bytes above the tools' own output budget (`tools::MAX_BYTES`): a result a tool
/// already bounded reaches history whole, and only longer text — MCP, hooks, a swarm of
/// reports — is cut by `shorten` and spilled to a file.
const MAX_RESULT_BYTES: usize = tools::MAX_BYTES + 8_192;

/// The session log, set when a session starts, so a panic can note the crash in it.
/// Latest wins: only `Agent::new` and a resume set it, and `/new` or `/resume` in the same
/// process replaces the log a crash belongs in.
static CRASH_LOG: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Where a crash record goes, when a session has started.
pub(crate) fn crash_log() -> Option<PathBuf> {
    CRASH_LOG.lock().ok()?.clone()
}

pub struct Agent {
    client: Rc<Client>,
    /// This session's fallback provider: what takes over when the primary exhausts its retries
    /// on a 4xx, 5xx or network failure. `/backup-model` sets it in the config's `backup_model`, so every
    /// session — new or resumed — starts with it.
    backup: Option<Rc<Client>>,
    /// The model the `advisor` tool asks: the config's `advisor_model`, which `/advisor` sets.
    /// None when no advisor is set, and in subagents, which don't consult one.
    advisor: Option<Rc<Client>>,
    tools: Rc<Vec<Value>>,
    messages: Rc<Vec<Value>>,
    ctx: Rc<tools::Context>,
    pub session: Session,
    /// The id sent as `prompt_cache_key` and as a provider's `session_header`: the session's
    /// id. Its subagents, pair children and the tasks other sessions send it use the same key,
    /// since their requests open with the same system prompt and tools (a fork, with the whole
    /// history), and a provider that routes its cache by the key (Meta does) serves that prefix
    /// only under the key that wrote it. A child's log name (`agent-N`) would be one id every
    /// session's first subagent shares.
    cache_key: String,
    pub stats: Stats,
    pub controls: Controls,
    settings: Rc<Settings>,
    /// Subagents one `agent_swarm` call runs at once; `/swarm parallel <n>` changes it for
    /// this session.
    swarm_parallel: Cell<usize>,
    /// The cap the model was last told, so a changed cap is said again; 0 when none was.
    swarm_cap: Cell<usize>,
    sink: Sink,
    ids: HashSet<String>,
    /// Identical tool calls made in the current turn, by a hash of the call.
    repeats: HashMap<u64, Repeat>,
    /// Calls made in the current turn against one read/search target or bash command, by a
    /// hash of the tool and its target; calls that differ only in tuning arguments or
    /// output filters share it.
    near_repeats: HashMap<u64, u32>,
    /// User messages pushed while the last reply's calls still await results. They enter the
    /// history once the last result is recorded: between a call and its result they read as a
    /// missing result to providers.
    deferred_messages: Vec<(Value, Option<Value>)>,
    /// History length at the last usage report; later messages are the unbilled suffix.
    accounted_messages: usize,
    /// Estimated size of the history right after its last compaction, the growth from which
    /// proactive compaction waits for before it runs again.
    compact_floor: u64,
    /// Subagents started by swarm calls in this turn.
    swarm_used: usize,
    /// The in-flight `agent_swarm` call's finished subagent results, if any, so an
    /// interruption can keep them instead of dropping the whole swarm's work.
    partial_swarm: Option<PartialSwarm>,
    partial_agents: HashMap<String, (String, bool)>,
    /// The log already says that the provider hides its reasoning.
    reasoning_noted: bool,
    todos: Vec<Todo>,
    /// Tool calls finished since the task list last changed; past `STALE_TASKS_AFTER` with an
    /// item open, the model is reminded to keep the list current.
    calls_since_todo: usize,
    plan_announced: bool,
    swarm_announced: bool,
    /// The model was last told ultra-concise mode is on.
    concise_announced: bool,
    /// The model was last told orchestrate mode is on.
    orchestrate_announced: bool,
    /// What the model was last told about the session goal: its text, or None when told
    /// the goal is over. Announced as an appended reminder, like plan and swarm mode.
    goal_announced: Option<String>,
    /// The model id and thinking effort the model was last told about, as a reminder:
    /// skills that report who reviewed a change get these values from it, not by guessing.
    model_announced: Option<String>,
    /// The other sessions in this tree the model was last told about, by pid.
    peers_announced: Vec<u32>,
    interrupted: bool,
    /// The error the last turn stopped on, when it stopped on one rather than by the user:
    /// the note after it says so instead of "the user interrupted".
    failed_on: Option<String>,
    /// Notes queued for the model's next prompt (`note_for_next_prompt`, e.g. after /undo).
    next_notes: Vec<String>,
    /// The turn's prompt while no reply or rewrite has come after it, so `retract` can take
    /// it back.
    pending_prompt: Option<PendingPrompt>,
    /// Hook commands already reported as failed to run, so each shows one notice.
    hook_notified: RefCell<HashSet<String>>,
    /// Keeps the machine from idle-sleeping while a top-level turn works.
    awake: crate::awake::Awake,
    depth: u8,
    /// Where this agent's events go, and the background task it runs as once it moved there.
    route: background::Route,
    /// Background work a resumed log left open, until `resume_background` takes it up.
    paused: std::collections::BTreeMap<u32, crate::session::BgRecord>,
    children: usize,
    /// The pair this session runs (`/pair`), if one stands: the mentor and executor agents.
    pair: Option<Pair>,
    /// A pair's executor: below the main agent, but still allowed its own subagents.
    pair_exec: bool,
    /// Index of the assistant message whose tool calls are running.
    current_reply: usize,
    session_started: bool,
    /// A snapshot started alongside the first request, waited out before the first tool
    /// call runs so its tree still predates every change the turn makes.
    snapshot_wait: Option<oneshot::Receiver<()>>,
    instructions: crate::prompt::Instructions,
}

fn millis(took: Duration) -> u64 {
    took.as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
pub(crate) mod tests;
