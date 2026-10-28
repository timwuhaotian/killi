//! The agent loop. History is append-only and holds messages exactly as sent; the system
//! prompt and tool list are fixed for the session, so every request reuses the cached prefix.
//! Notes from killi to the model are appended as user messages, never edited into old ones.

mod ask;
mod compact;
mod dispatch;
mod lifecycle;
mod peer_guard;
mod request;
mod subagent;
mod turn;
mod types;

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::future::Future;
use std::rc::{Rc, Weak};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use futures_util::{StreamExt, future::join_all, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::config::Thinking;
use crate::llm::{Client, Delta, ErrorKind, IMAGE_OMITTED, IMAGE_TOKEN_RESERVE, ProviderError, Reply, ToolCall, Usage, truncate};
use crate::session::Session;
use crate::hooks::{HooksConfig, tool_payload};
use crate::skills::Skills;
use crate::{live, plan, swarm, tools};

pub use types::{Aside, Controls, Event, Image, Question, Settings, Sink, Slot, Stats, Steered, Stop, Todo};

use compact::CompactMode;
use dispatch::{invalid_arguments, neutralize_reminder_tags, parse_args};
use request::{is_media, is_overflow, reasoning_evidence};
use types::{PartialSwarm, Repeat, last_plan_state, unanswered};

#[cfg(test)]
use ask::question_args;
#[cfg(test)]
use compact::{COMPACT_INSTRUCTION, DEFAULT_COMPACT_AT, DEFAULT_OUTPUT_RESERVE, REDUCED_RESULT_CHARS, REDUCED_TEXT_CHARS, estimate_tokens, kept_prompt, reduced};
#[cfg(test)]
use dispatch::{REPEAT_STOP, REPEAT_WARNINGS, SKIPPED_FOR_MESSAGE, dispatch_tool, hash_call, image_parts, parallel_read, repeat_key};
#[cfg(test)]
use request::hides_reasoning;
#[cfg(test)]
use subagent::{MAX_SWARM_AGENTS_PER_TURN, reserve_swarm};
#[cfg(test)]
use turn::{STALE_TASKS_AFTER, ends_with_colon, ends_with_question, open_tasks_note, stale_tasks_note};
#[cfg(test)]
use types::{BTW_NOTE, RECAP_CHARS, RECAP_MAX, RECAP_NOTE, fit_recap, side_request};

/// A backstop in bytes above the tools' own output budget (`tools::MAX_BYTES`): a result a tool
/// already bounded reaches history whole, and only longer text — MCP, hooks, a swarm of
/// reports — is cut by `shorten` and spilled to a file.
const MAX_RESULT_BYTES: usize = tools::MAX_BYTES + 8_192;

/// The session log, set when a session starts, so a panic can note the crash in it.
/// Latest wins: only main sessions are assembled, and `/new` or `/resume` in the same
/// process replaces the log a crash belongs in.
static CRASH_LOG: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Where a crash record goes, when a session has started.
pub(crate) fn crash_log() -> Option<PathBuf> {
    CRASH_LOG.lock().ok()?.clone()
}

pub struct Agent {
    client: Rc<Client>,
    /// This session's fallback provider: what takes over when the primary exhausts its retries
    /// on a rate limit. Set with `/backup-model`; a new session starts without one, so a later
    /// session never inherits it.
    backup: Option<Rc<Client>>,
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
    /// Estimated size of the history right after its last compaction, the growth from which
    /// proactive compaction waits for before it runs again.
    compact_floor: u64,
    /// Subagents started by swarm calls in this turn.
    swarm_used: usize,
    /// The in-flight `agent_swarm` call's finished subagent results, if any, so an
    /// interruption can keep them instead of dropping the whole swarm's work.
    partial_swarm: Option<PartialSwarm>,
    /// The log already says that the provider hides its reasoning.
    reasoning_noted: bool,
    todos: Vec<Todo>,
    /// Tool calls finished since the task list last changed; past `STALE_TASKS_AFTER` with an
    /// item open, the model is reminded to keep the list current.
    calls_since_todo: usize,
    plan_announced: bool,
    swarm_announced: bool,
    /// The other sessions in this tree the model was last told about, by pid.
    peers_announced: Vec<u32>,
    interrupted: bool,
    /// Notes queued for the model's next prompt (`note_for_next_prompt`, e.g. after /undo).
    next_notes: Vec<String>,
    /// Hook commands already reported as failed to run, so each shows one notice.
    hook_notified: RefCell<HashSet<String>>,
    depth: u8,
    children: usize,
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
mod tests;
