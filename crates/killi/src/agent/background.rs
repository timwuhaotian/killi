//! Subagents that run beside the conversation instead of inside one of its tool calls. A
//! foreground `agent` or `agent_swarm` call moves here when the user sends a message while it
//! runs (`focus` mode), and every such call starts here in `orchestrate` mode. Each is a task
//! the model reads with `task_output` and stops with `task_stop`, numbered with the session's
//! bash tasks. Its report is delivered once: read with `task_output`, or appended as a
//! reminder at the main agent's next step, which the terminal starts by itself when the
//! session sits idle.
//!
//! The terminal owns the futures and polls them while it waits for keys and while a turn
//! runs; `Background` is the registry both sides read. Nothing here holds a borrow across an
//! await.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::{Semaphore, mpsc};

use super::*;

mod resume;

/// A background subagent's work as the terminal polls it; it ends with the task's id.
pub type BgFuture = Pin<Box<dyn Future<Output = u32>>>;

/// Background subagents running at once; more wait for a slot. A swarm counts as one: it
/// already runs `swarm_parallel` members inside.
pub const MAX_BACKGROUND: usize = 4;

/// How often a wait on a background subagent asks whether it ended or the user spoke.
const POLL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgState {
    /// Waiting for one of the `MAX_BACKGROUND` slots.
    Queued,
    Running,
    /// Ended with a report.
    Done,
    /// Stopped with `task_stop` or as the session ended.
    Stopped,
}

/// One background subagent, or one swarm.
pub struct BgTask {
    pub label: String,
    /// Members of a swarm; 1 for an `agent` call.
    pub members: usize,
    pub finished_members: usize,
    pub state: BgState,
    pub started: Instant,
    /// When it started and ended, in ms since the epoch, for `/undo` to compare.
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    /// Tool calls its subagents started so far, and the last one, for the live area and a
    /// `task_output` that finds it still running.
    pub calls: u32,
    pub last: Option<String>,
    /// The report and whether it failed, once it ends.
    pub report: Option<(String, bool)>,
    /// Requests and tokens its subagents used, added to the session's once delivered.
    pub stats: Stats,
    /// The model has the report, or stopped the task itself.
    pub delivered: bool,
    cancel: Rc<Cell<bool>>,
    /// The subagent's own message queue, for `agent_message`; none for a swarm.
    pub(super) steer: Option<Rc<RefCell<VecDeque<Steered>>>>,
}

impl BgTask {
    pub fn ended(&self) -> bool {
        matches!(self.state, BgState::Done | BgState::Stopped)
    }

    /// Seconds since it was started.
    pub fn secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }
}

/// The session's background subagents, shared by the agent and the terminal.
pub struct Background {
    pub tasks: BTreeMap<u32, BgTask>,
    /// Where new work goes to be polled; only an interactive session sets it.
    spawn: Option<mpsc::UnboundedSender<BgFuture>>,
    slots: Arc<Semaphore>,
    /// The main agent is in a turn: a background subagent then shares the tree with it.
    pub main_busy: bool,
}

impl Default for Background {
    fn default() -> Self {
        Self {
            tasks: BTreeMap::new(),
            spawn: None,
            slots: Arc::new(Semaphore::new(MAX_BACKGROUND)),
            main_busy: false,
        }
    }
}

impl Background {
    /// Lets this session run subagents in the background, polled from `rx`'s other end.
    pub fn attach(&mut self, spawn: mpsc::UnboundedSender<BgFuture>) {
        self.spawn = Some(spawn);
    }

    pub fn attached(&self) -> bool {
        self.spawn.is_some()
    }

    /// Tasks not ended yet, queued ones included.
    pub fn working(&self) -> usize {
        self.tasks.values().filter(|task| !task.ended()).count()
    }

    /// Ended tasks whose report the model hasn't had.
    pub fn undelivered(&self) -> usize {
        self.tasks
            .values()
            .filter(|task| task.ended() && !task.delivered)
            .count()
    }

    /// Forgets every task, for a conversation the session leaves: the futures go with the
    /// terminal's set, so nothing keeps running for a conversation no longer shown.
    pub fn clear(&mut self) {
        self.tasks.clear();
        self.slots = Arc::new(Semaphore::new(MAX_BACKGROUND));
    }

    /// A task that already ended with `report`, for the terminal's tests.
    #[cfg(test)]
    pub fn insert_ended(&mut self, id: u32, label: &str, report: &str) {
        self.tasks.insert(
            id,
            BgTask {
                label: label.to_string(),
                members: 1,
                finished_members: 1,
                state: BgState::Done,
                started: Instant::now(),
                started_ms: 0,
                ended_ms: Some(0),
                calls: 0,
                last: None,
                report: Some((report.to_string(), false)),
                stats: Stats::default(),
                delivered: false,
                cancel: Rc::default(),
                steer: None,
            },
        );
    }

    /// Ids of tasks that worked in the tree since `since` (ms since the epoch), or still do.
    pub fn worked_since(&self, since: u64) -> Vec<u32> {
        self.tasks
            .iter()
            .filter(|(_, task)| task.ended_ms.is_none_or(|ended| ended >= since))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Notes what a background subagent just did, from its events.
    fn progress(&mut self, id: u32, event: &Event) {
        let Some(task) = self.tasks.get_mut(&id) else {
            return;
        };
        match event {
            Event::ToolStart { summary, .. } => {
                task.calls += 1;
                task.last = Some(summary.clone());
            }
            Event::Finished { .. } => task.finished_members += 1,
            _ => {}
        }
    }
}

/// Marks the main agent as in a turn while it lives, so a background subagent's git guard
/// sees it; dropped when the turn ends, also when it is interrupted.
pub(super) struct Busy(Rc<RefCell<Background>>);

impl Busy {
    pub(super) fn new(background: &Rc<RefCell<Background>>) -> Self {
        background.borrow_mut().main_busy = true;
        Self(background.clone())
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.borrow_mut().main_busy = false;
    }
}

/// Where a subagent's events go: its parent's sink while it runs in the foreground, the
/// registry once it moved to the background.
#[derive(Clone, Default)]
pub(super) struct Route(Rc<RefCell<Option<Detached>>>);

/// The registry a subagent's events go to once it moved there, and its task there.
type Detached = (Rc<RefCell<Background>>, u32);

impl Route {
    /// The sink for a subagent labeled `label`: `Sub` events to `parent` until `detach`.
    pub(super) fn sink(&self, parent: Sink, label: String, slot: Option<Slot>) -> Sink {
        let route = self.0.clone();
        Rc::new(move |event| {
            let target = route.borrow().clone();
            match target {
                Some((background, id)) => background.borrow_mut().progress(id, &event),
                None => parent(Event::Sub {
                    label: label.clone(),
                    slot: slot.clone(),
                    event: Box::new(event),
                }),
            }
        })
    }

    pub(super) fn detach(&self, background: &Rc<RefCell<Background>>, id: u32) {
        *self.0.borrow_mut() = Some((background.clone(), id));
    }

    /// The background task this subagent runs as, once it moved there.
    pub(super) fn id(&self) -> Option<u32> {
        self.0.borrow().as_ref().map(|(_, id)| *id)
    }
}

/// What a background task's work ends with: its report and what its subagents used.
pub(super) type BgWork = Pin<Box<dyn Future<Output = (tools::Output, Stats)>>>;

/// What starting a background task needs to know about it.
pub(super) struct BgStart {
    pub(super) label: String,
    pub(super) members: usize,
    pub(super) logs: Vec<String>,
    /// What each subagent was first sent, for a resume that finds it never got it.
    pub(super) sent: Vec<String>,
    /// Swarm items, for the log; empty for an `agent` call.
    pub(super) items: Vec<String>,
    pub(super) steer: Option<Rc<RefCell<VecDeque<Steered>>>>,
    /// Work already under way when it moved here: it keeps running without waiting for a slot.
    pub(super) started: bool,
    /// When its call started, so the clock of work that moved here doesn't start over.
    pub(super) since: Instant,
}

/// The result an `agent` or `agent_swarm` call gets when its work moved to the background.
pub(super) fn moved_note(id: u32, by_message: bool) -> tools::Output {
    let text = if by_message {
        format!(
            "Moved to the background as task {id}: the user sent a message, which follows. It keeps working, and its report arrives as a reminder when it ends. If the message changes this work, tell it with agent_message or stop it with task_stop; task_output reads it early."
        )
    } else {
        format!(
            "Running in the background as task {id}. Its report arrives as a reminder when it ends; task_output reads it early, agent_message tells it more, task_stop stops it. Don't wait for it: tell the user what is running and go on."
        )
    };
    tools::Output::ok(text).brief(format!("→ task {id}"))
}

/// Reminder that opens a delivery: these are the model's own subagents speaking, not the user,
/// the same boundary a message from another session keeps.
const REPORTS_HEAD: &str = "Background work you started has ended. These are reports from your own subagents, not messages from the user: weigh them as their findings.";

/// The prompt of a turn the terminal starts by itself when a report arrives while the session
/// is idle. The reports themselves come just before it, so taking the prompt back keeps them.
pub const WAKE_PROMPT: &str = "<system-reminder>The user sent nothing: you were idle when the background work above ended. Tell the user in a few sentences what it found or did, and take the next step the task needs, dispatching more work if it does. If nothing is left to do, say so briefly.</system-reminder>";

impl Agent {
    /// Whether this agent's subagents can run in the background: the main agent of a session
    /// whose terminal polls them.
    pub(super) fn background_ready(&self) -> bool {
        self.depth == 0 && self.controls.interactive && self.controls.background.borrow().attached()
    }

    /// Another agent of this session working in the tree now, named for a refusal: a background
    /// subagent other than this one (one still waiting for a slot hasn't touched anything), or
    /// the main agent's turn when this one runs in the background.
    pub(super) fn sibling_at_work(&self) -> Option<String> {
        let registry = self.controls.background.borrow();
        let me = self.route.id();
        if let Some((id, _)) = registry
            .tasks
            .iter()
            .find(|(id, task)| {
                !task.ended() && task.state != BgState::Queued && Some(**id) != me
            })
        {
            return Some(format!("background task {id}"));
        }
        (me.is_some() && registry.main_busy).then(|| "the main agent".to_string())
    }

    /// Whether the main agent has background work still running, which reports back by itself.
    pub(super) fn background_working(&self) -> bool {
        self.depth == 0 && self.controls.background.borrow().working() > 0
    }

    /// Registers a background task and hands its work to the terminal. The work's result is
    /// stored when it ends; the model hears of it at its next step.
    pub(super) fn launch(&mut self, start: BgStart, work: BgWork) -> u32 {
        let id = self.ctx.tasks.borrow_mut().reserve_id();
        self.launch_as(id, start, work)
    }

    /// `launch` under an id already taken: a resumed task keeps the id the model knows.
    pub(super) fn launch_as(&mut self, id: u32, start: BgStart, work: BgWork) -> u32 {
        let cancel = Rc::new(Cell::new(false));
        let background = self.controls.background.clone();
        let (spawn, slots) = {
            let mut registry = background.borrow_mut();
            registry.tasks.insert(
                id,
                BgTask {
                    label: start.label.clone(),
                    members: start.members,
                    finished_members: 0,
                    state: if start.started {
                        BgState::Running
                    } else {
                        BgState::Queued
                    },
                    started: start.since,
                    started_ms: live::now()
                        .saturating_sub(start.since.elapsed().as_millis() as u64),
                    ended_ms: None,
                    calls: 0,
                    last: None,
                    report: None,
                    stats: Stats::default(),
                    delivered: false,
                    cancel: cancel.clone(),
                    steer: start.steer.clone(),
                },
            );
            (registry.spawn.clone(), registry.slots.clone())
        };
        let _ = self.session.record(&json!({
            "type": "bg_start",
            "id": id,
            "label": start.label,
            "items": start.items,
            "logs": start.logs,
            "sent": start.sent,
        }));
        let awake = self.awake.clone();
        let registry = background.clone();
        let started = start.started;
        let future: BgFuture = Box::pin(async move {
            // Work already running keeps going; new work waits for a slot.
            let _slot = if started {
                slots.clone().try_acquire_owned().ok()
            } else {
                tokio::select! {
                    slot = slots.clone().acquire_owned() => slot.ok(),
                    () = cancelled(&cancel) => None,
                }
            };
            if !cancel.get() {
                if let Some(task) = registry.borrow_mut().tasks.get_mut(&id) {
                    task.state = BgState::Running;
                }
                let _awake = awake.hold();
                let ended = tokio::select! {
                    ended = work => Some(ended),
                    () = cancelled(&cancel) => None,
                };
                if let Some((output, stats)) = ended
                    && let Some(task) = registry.borrow_mut().tasks.get_mut(&id)
                {
                    task.state = BgState::Done;
                    task.report = Some((output.text, output.is_error));
                    task.stats = stats;
                    task.ended_ms = Some(live::now());
                }
            }
            if let Some(task) = registry.borrow_mut().tasks.get_mut(&id)
                && !task.ended()
            {
                task.state = BgState::Stopped;
                task.ended_ms = Some(live::now());
            }
            id
        });
        match spawn {
            Some(spawn) if spawn.send(future).is_ok() => {}
            _ => {
                // Nothing polls it: say so rather than leaving a task that never moves.
                if let Some(task) = background.borrow_mut().tasks.get_mut(&id) {
                    task.state = BgState::Done;
                    task.report = Some((
                        "Could not start in the background: the session isn't taking background work.".into(),
                        true,
                    ));
                    task.ended_ms = Some(live::now());
                }
            }
        }
        id
    }

    /// Appends the reports of background tasks that ended since the last step as one reminder,
    /// adds what they used to the session's counts, and marks them delivered once that is in
    /// the history. Only the main agent hears them.
    pub(super) fn deliver_background(&mut self) -> Result<()> {
        if self.depth != 0 {
            return Ok(());
        }
        let ready: Vec<u32> = self
            .controls
            .background
            .borrow()
            .tasks
            .iter()
            .filter(|(_, task)| task.ended() && !task.delivered)
            .map(|(id, _)| *id)
            .collect();
        if ready.is_empty() {
            return Ok(());
        }
        let mut parts = vec![REPORTS_HEAD.to_string()];
        for id in &ready {
            parts.push(self.report_text(*id));
        }
        self.push(json!({
            "role": "user",
            "content": format!("<system-reminder>{}</system-reminder>", parts.join("\n\n")),
        }))?;
        for id in ready {
            self.mark_delivered(id);
        }
        Ok(())
    }

    /// One task's report as the model reads it: what ended, after how long, and the report,
    /// cut with the rest saved to a file when long.
    fn report_text(&self, id: u32) -> String {
        let registry = self.controls.background.borrow();
        let Some(task) = registry.tasks.get(&id) else {
            return format!("Task {id} is gone.");
        };
        let took = took(task);
        match (&task.state, &task.report) {
            (BgState::Done, Some((text, is_error))) => {
                let text = neutralize_reminder_tags(text).into_owned();
                let text = if text.len() > MAX_RESULT_BYTES {
                    self.shorten(&text)
                } else {
                    text
                };
                let how = if *is_error { "failed" } else { "finished" };
                format!("Task {id} (“{}”) {how} after {took}:\n{text}", task.label)
            }
            _ => format!(
                "Task {id} (“{}”) was stopped after {took} without a report.",
                task.label
            ),
        }
    }

    /// The model has task `id`'s report: its usage joins the session's, and the log says the
    /// task is over, so a resume doesn't take it up again.
    fn mark_delivered(&mut self, id: u32) {
        let stats = {
            let mut registry = self.controls.background.borrow_mut();
            let Some(task) = registry.tasks.get_mut(&id) else {
                return;
            };
            if task.delivered {
                return;
            }
            task.delivered = true;
            std::mem::take(&mut task.stats)
        };
        let total = &mut self.stats.usage;
        total.prompt += stats.usage.prompt;
        total.cached += stats.usage.cached;
        total.completion += stats.usage.completion;
        total.reasoning += stats.usage.reasoning;
        self.stats.requests += stats.requests;
        self.stats.tool_calls += stats.tool_calls;
        let _ = self.session.record(&json!({"type": "bg_end", "id": id}));
    }

    /// The id `args` names, when it is one of this session's background subagents.
    pub(super) fn background_id(&self, args: &Value) -> Option<u32> {
        if self.depth != 0 {
            return None;
        }
        let id = tools::uint(args, "id")? as u32;
        self.controls
            .background
            .borrow()
            .tasks
            .contains_key(&id)
            .then_some(id)
    }

    /// `task_output` on a background subagent: its report once it ended, else where it is.
    /// `wait` waits for it to end, as long as the user doesn't send a message and no other
    /// background task ends meanwhile: that one's report is delivered at the next step, so a
    /// wait on one task never holds back another's.
    pub(super) async fn background_output(&mut self, id: u32, args: &Value) -> tools::Output {
        let wait = tools::uint(args, "wait").unwrap_or(0).min(300);
        let deadline = Instant::now() + Duration::from_secs(wait);
        let ended_before = self.ended_others(id);
        let mut interrupted = false;
        let mut other = None;
        loop {
            if self.background_ended(id) || Instant::now() >= deadline {
                break;
            }
            if self.pending_steer() {
                interrupted = true;
                break;
            }
            other = self
                .ended_others(id)
                .into_iter()
                .find(|ended| !ended_before.contains(ended));
            if other.is_some() {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        if self.background_ended(id) {
            let text = self.report_text(id);
            let failed = self
                .controls
                .background
                .borrow()
                .tasks
                .get(&id)
                .is_some_and(|task| !matches!(task.report, Some((_, false))));
            let delivered = self
                .controls
                .background
                .borrow()
                .tasks
                .get(&id)
                .is_some_and(|task| task.delivered);
            self.mark_delivered(id);
            let text = if delivered {
                format!("{text}\n[You already had this report.]")
            } else {
                text
            };
            return if failed {
                tools::Output::error(text)
            } else {
                tools::Output::ok(text)
            }
            .brief(format!("task {id} ended"));
        }
        let registry = self.controls.background.borrow();
        let Some(task) = registry.tasks.get(&id) else {
            return tools::Output::error(format!("No background task {id}."));
        };
        let mut text = match task.state {
            BgState::Queued => format!(
                "Task {id} (“{}”) is waiting for a free slot ({}s): {MAX_BACKGROUND} background subagents run at once.",
                task.label,
                task.secs()
            ),
            _ => {
                let members = if task.members > 1 {
                    format!(
                        "{} of {} subagents done, ",
                        task.finished_members, task.members
                    )
                } else {
                    String::new()
                };
                let last = task
                    .last
                    .as_deref()
                    .map(|last| format!("; last: {}", truncate(last, 100)))
                    .unwrap_or_default();
                format!(
                    "Task {id} (“{}”) is still running ({}s; {members}{} tool calls so far{last}).",
                    task.label,
                    task.secs(),
                    task.calls
                )
            }
        };
        if interrupted {
            text.push_str("\n[Stopped waiting: a message from the user is waiting. Read it first; task_output with `wait` picks the wait up again.]");
        } else if let Some(other) = other {
            text.push_str(&format!(
                "\n[Stopped waiting: background task {other} ended, and its report follows.]"
            ));
        } else if wait > 0 && task.state != BgState::Queued {
            // Models told not to wait polled anyway, one 300 s wait after another for hours,
            // which kept the turn open so no report could start one of its own.
            text.push_str("\n[Its report comes to you by itself when it ends, and starts a turn of its own if you are idle then. Rather than waiting again, tell the user what is still running and end your turn.]");
        }
        tools::Output::ok(text).brief(format!("task {id} running"))
    }

    /// Background tasks other than `id` that ended and whose report the model hasn't had.
    fn ended_others(&self, id: u32) -> Vec<u32> {
        self.controls
            .background
            .borrow()
            .tasks
            .iter()
            .filter(|(other, task)| **other != id && task.ended() && !task.delivered)
            .map(|(other, _)| *other)
            .collect()
    }

    fn background_ended(&self, id: u32) -> bool {
        self.controls
            .background
            .borrow()
            .tasks
            .get(&id)
            .is_none_or(BgTask::ended)
    }

    /// `agent_message`: queues `message` for a background subagent's next step, marked as
    /// coming from the agent that started it.
    pub(super) fn background_message(&mut self, id: u32, args: &Value) -> tools::Output {
        let message = args["message"].as_str().unwrap_or_default().trim();
        if message.is_empty() {
            return tools::Output::error("agent_message needs a `message`.");
        }
        let registry = self.controls.background.borrow();
        let Some(task) = registry.tasks.get(&id) else {
            return tools::Output::error(format!("No background task {id}."));
        };
        if task.ended() {
            return tools::Output::error(format!(
                "Task {id} has already ended; its report comes as a reminder, or now with task_output."
            ));
        }
        let Some(steer) = &task.steer else {
            return tools::Output::error(format!(
                "Task {id} is a swarm, whose subagents each work on their own item; stop it with task_stop and start what should run instead."
            ));
        };
        steer.borrow_mut().push_back((
            format!(
                "<system-reminder>A message from the agent that started you, not from the user: {}</system-reminder>",
                neutralize_reminder_tags(message)
            ),
            Vec::new(),
        ));
        tools::Output::ok(format!("Told task {id}; it reads it before its next step."))
            .brief(format!("→ task {id}"))
    }

    /// Stops every background task still working, as the session ends: the log says they're
    /// over, so a resume doesn't take them up again. Gives how many there were.
    pub fn stop_background(&mut self) -> usize {
        let ids: Vec<u32> = self
            .controls
            .background
            .borrow()
            .tasks
            .iter()
            .filter(|(_, task)| !task.ended())
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            self.background_stop(*id);
        }
        ids.len()
    }

    /// `task_stop` on a background subagent. What it started stops with it, and the model,
    /// having stopped it, gets no report for it later.
    pub(super) fn background_stop(&mut self, id: u32) -> tools::Output {
        let running = {
            let registry = self.controls.background.borrow();
            match registry.tasks.get(&id) {
                Some(task) if !task.ended() => {
                    task.cancel.set(true);
                    true
                }
                Some(_) => false,
                None => return tools::Output::error(format!("No background task {id}.")),
            }
        };
        if running {
            self.mark_delivered(id);
            tools::Output::ok(format!("Stopped task {id}."))
        } else {
            tools::Output::ok(format!("Task {id} had already finished."))
        }
    }
}

/// Resolves once `flag` is set.
async fn cancelled(flag: &Cell<bool>) {
    while !flag.get() {
        tokio::time::sleep(POLL).await;
    }
}

/// How long a task ran, as "3m 12s".
fn took(task: &BgTask) -> String {
    let secs = match task.ended_ms {
        Some(ended) => ended.saturating_sub(task.started_ms) / 1000,
        None => task.secs(),
    };
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}
