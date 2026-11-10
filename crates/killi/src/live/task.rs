//! Tasks as the sessions say them to each other: where one stands, what a session keeps about
//! it, and the lines of its event stream.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;

/// How long a task may run before the session running it gives up on it, in seconds: what
/// `killi send --timeout` defaults to, and what a session applies when a request names none.
pub const TASK_TIMEOUT: u64 = 600;

/// The longest a task may be given, in seconds: a day. `killi send --timeout` refuses more, and
/// a session holds a request that names more to this.
pub const MAX_TASK_TIMEOUT: u64 = 24 * 60 * 60;

/// Where a task another session sent stands. The names are A2A's, so a task reads the same
/// from a killi session as from any agent that speaks that protocol.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    /// Taken, not started yet.
    #[default]
    Submitted,
    /// Its agent is working on it.
    Working,
    /// It stopped on something only the session that sent it can answer; the answer comes as a
    /// new task, as A2A starts a new task in the same context.
    InputRequired,
    Completed,
    Failed,
    Canceled,
    /// The session refused it: it already runs as many tasks as it takes.
    Rejected,
}

impl TaskState {
    /// Whether nothing more will happen to a task in this state.
    pub fn terminal(self) -> bool {
        !matches!(self, TaskState::Submitted | TaskState::Working)
    }

    /// Its name, as the wire and what the user reads spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Submitted => "submitted",
            TaskState::Working => "working",
            TaskState::InputRequired => "input-required",
            TaskState::Completed => "completed",
            TaskState::Failed => "failed",
            TaskState::Canceled => "canceled",
            TaskState::Rejected => "rejected",
        }
    }
}

/// A task as another session sees it: what it asked, where it stands, and its report once it
/// ends. The session holding the task fills this in; `Get`, `List` and `killi task` read it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskView {
    pub id: String,
    /// The session that sent it, when one did.
    pub from: Option<u32>,
    /// The first line of what it asked for.
    pub prompt: String,
    pub state: TaskState,
    /// When it was taken, in ms since the epoch.
    pub since: u64,
    /// Its final report, once it ended.
    pub report: Option<String>,
    /// Its log, to read what it did before it failed or was stopped.
    pub log: Option<PathBuf>,
    /// Why it is not `completed`: what it is blocked on, what failed, or that it was stopped.
    pub message: Option<String>,
}

/// One event of a task's stream, as a JSON line: where it stands, and what it says about the
/// step it is on.
pub fn event_line(state: TaskState, message: Option<&str>) -> String {
    let mut line = json!({"state": state});
    if let Some(message) = message {
        line["message"] = json!(crate::llm::truncate(message, 200));
    }
    line.to_string()
}

/// The last event of a task's stream: its state, and its report or what went wrong.
pub fn view_line(view: &TaskView) -> String {
    let mut line = json!({"state": view.state});
    if let Some(report) = &view.report {
        line["report"] = json!(report);
    }
    if let Some(message) = &view.message {
        line["message"] = json!(message);
    }
    line.to_string()
}

/// The session a task id names: the owner's pid is the id's prefix, so no registry has to be
/// asked which session holds the task.
pub fn task_owner(id: &str) -> Option<u32> {
    id.split('-').next().and_then(|pid| pid.parse().ok())
}
