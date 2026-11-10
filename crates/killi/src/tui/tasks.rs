//! Tasks other sessions send. One runs a fresh agent of this session's own — its model, tools
//! and system prompt — to a final report, while the session goes on with whatever its user is
//! doing: `killi send` hands a task over, `killi task` reads where it stands, `killi task
//! --follow` watches it work, and `killi cancel` stops it.

mod run;
#[cfg(test)]
mod tests;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::*;
use run::run_peer_task;

#[cfg(test)]
use run::outcome_of;

/// Tasks one session runs at once; more are refused, since each spends this session's model.
pub(super) const MAX_PEER_TASKS: usize = 2;

/// Finished tasks a session can still be asked about, oldest first.
const KEPT_TASKS: usize = 16;

impl App {
    /// Takes a task another session sent: composes its prompt, starts its agent, and answers
    /// `killi send` — at once with the task's id when that is all it asked for, otherwise with
    /// the report once the task ends.
    pub(super) fn on_task_send(
        &mut self,
        from: Option<u32>,
        text: String,
        timeout: Option<u64>,
        after: Option<String>,
        quick: bool,
        reply: live::Reply,
    ) {
        let running = self.tasks_running();
        if running >= MAX_PEER_TASKS {
            return refuse(
                reply,
                &format!(
                    "it is running {running} tasks already; ask again when one of them ends"
                ),
            );
        }
        let prompt = match self.task_prompt(&text, after.as_deref()) {
            Ok(prompt) => prompt,
            Err(why) => return refuse(reply, &why),
        };
        self.peer_task_id += 1;
        // The owner's pid is the id's prefix, so `killi task` and `killi cancel` find this
        // session from the id alone.
        let id = format!("{}-{}", std::process::id(), self.peer_task_id);
        let view = Rc::new(RefCell::new(live::TaskView {
            id: id.clone(),
            from,
            prompt: crate::llm::truncate(text.lines().next().unwrap_or_default(), 200).to_string(),
            state: live::TaskState::Submitted,
            since: live::now(),
            ..live::TaskView::default()
        }));
        let watchers: Rc<RefCell<Vec<mpsc::UnboundedSender<String>>>> = Rc::default();
        let (cancel, stopped) = oneshot::channel();
        let waiting = match reply {
            live::Reply::Once(answer) if quick => {
                let _ = answer.send(json!({"ok": true, "id": id, "state": view.borrow().state}));
                None
            }
            live::Reply::Once(answer) => Some(answer),
            live::Reply::Stream(events) => {
                let _ = events.send(live::event_line(view.borrow().state, None));
                None
            }
        };
        let aside = self.controls.aside.clone();
        let here = self.controls.here.clone();
        self.peer_tasks.push(PeerTask {
            view: view.clone(),
            waiting,
            watchers: watchers.clone(),
            cancel: Some(cancel),
            stopped: false,
            ended: None,
        });
        // A request written by hand can name any number of seconds; a task still ends.
        let timeout = timeout
            .unwrap_or(live::TASK_TIMEOUT)
            .clamp(1, live::MAX_TASK_TIMEOUT);
        self.peer_running.push(Box::pin(run_peer_task(
            view,
            self.peer_task_id as usize,
            prompt,
            aside,
            here,
            watchers,
            stopped,
            Duration::from_secs(timeout),
        )));
        self.live_tasks();
        self.task_notice(&format!(
            "{} sent a task: “{}” · `killi task {id}` follows it, `killi cancel {id}` stops it",
            peer_label(from),
            first_line(&text, 80)
        ));
    }

    /// `Get`: what a task stands on now, and its report once it ended.
    pub(super) fn on_task_get(&mut self, id: &str, reply: live::Reply) {
        match self.peer_tasks.iter().find(|task| task.view.borrow().id == id) {
            Some(task) => answer(reply, json!({"ok": true, "task": *task.view.borrow()})),
            None => refuse(reply, &format!("no task {id} here; `killi tasks` lists them")),
        }
    }

    /// `List`: every task this session has taken, oldest first.
    pub(super) fn on_task_list(&mut self, reply: live::Reply) {
        let tasks: Vec<live::TaskView> = self
            .peer_tasks
            .iter()
            .map(|task| task.view.borrow().clone())
            .collect();
        answer(reply, json!({"ok": true, "tasks": tasks}));
    }

    /// `Cancel`: stops a task that is still running. A task that already ended says so.
    pub(super) fn on_task_cancel(&mut self, id: &str, reply: live::Reply) {
        let Some(index) = self
            .peer_tasks
            .iter()
            .position(|task| task.view.borrow().id == id)
        else {
            return refuse(reply, &format!("no task {id} here; `killi tasks` lists them"));
        };
        let task = &mut self.peer_tasks[index];
        let stopped = {
            let mut view = task.view.borrow_mut();
            if view.state.terminal() {
                let state = view.state.as_str();
                drop(view);
                return refuse(reply, &format!("task {id} already {state}"));
            }
            // The state changes now, so a `Get` right after says canceled; the agent stops at
            // its next step and leaves the state as it is.
            view.state = live::TaskState::Canceled;
            view.message = Some("canceled with `killi cancel`".into());
            view.message.clone()
        };
        task.stopped = true;
        task.ended = Some(live::now());
        if let Some(cancel) = task.cancel.take() {
            let _ = cancel.send(());
        }
        // The streams watching it get its last event and end with it.
        let line = live::view_line(&task.view.borrow());
        task.watchers
            .borrow_mut()
            .retain(|events| events.send(line.clone()).is_ok());
        task.watchers.borrow_mut().clear();
        // A blocking `killi send` waiting for the report is told it will not come, instead of
        // waiting for a task that is already over.
        if let Some(waiting) = task.waiting.take() {
            let _ = waiting.send(json!({
                "ok": true, "id": id, "state": live::TaskState::Canceled, "message": stopped,
            }));
        }
        answer(
            reply,
            json!({"ok": true, "id": id, "state": live::TaskState::Canceled}),
        );
        self.live_tasks();
        self.task_notice(&format!("Task {id} canceled with `killi cancel`."));
    }

    /// `Follow`: a task's events as they happen. A task that already ended sends its last event
    /// and closes; a running one sends where it stands, then each step it takes.
    pub(super) fn on_task_follow(&mut self, id: &str, events: mpsc::UnboundedSender<String>) {
        let Some(task) = self.peer_tasks.iter_mut().find(|task| task.view.borrow().id == id) else {
            let why = format!("no task {id} here; `killi tasks` lists them");
            let _ = events.send(live::event_line(live::TaskState::Rejected, Some(&why)));
            return;
        };
        let view = task.view.borrow().clone();
        if view.state.terminal() {
            let _ = events.send(live::view_line(&view));
            return;
        }
        // Where it stands and what it is on now, so a stream that joins mid-step doesn't miss
        // the step already running.
        let _ = events.send(live::event_line(view.state, view.message.as_deref()));
        task.watchers.borrow_mut().push(events);
    }

    /// Files a finished task: its last event to the streams watching it, the report to a
    /// blocking `killi send`, and a line in the live area. A task canceled while it ran keeps
    /// its canceled state.
    pub(super) fn on_peer_task_done(&mut self, id: String) {
        let done = {
            let Some(task) = self
                .peer_tasks
                .iter()
                .find(|task| task.view.borrow().id == id)
            else {
                return;
            };
            // A task canceled while it ran keeps its canceled state and its own notice.
            if task.stopped {
                return;
            }
            task.view.borrow().clone()
        };
        let note = match done.state {
            live::TaskState::Completed => format!("Task {} completed.", done.id),
            live::TaskState::InputRequired => format!(
                "Task {} is blocked: {}",
                done.id,
                done.message.as_deref().unwrap_or("it needs an answer")
            ),
            state => format!(
                "Task {} {}: {}",
                done.id,
                state.as_str(),
                done.message.as_deref().unwrap_or("it gave no reason")
            ),
        };
        {
            let Some(task) = self
                .peer_tasks
                .iter_mut()
                .find(|task| task.view.borrow().id == id)
            else {
                return;
            };
            task.ended = Some(live::now());
            // The last event, then the streams end: dropping the watchers closes each one.
            let line = live::view_line(&done);
            task.watchers
                .borrow_mut()
                .retain(|events| events.send(line.clone()).is_ok());
            task.watchers.borrow_mut().clear();
            if let Some(waiting) = task.waiting.take() {
                let _ = waiting.send(json!({
                    "ok": true,
                    "id": done.id,
                    "state": done.state,
                    "report": done.report,
                    "message": done.message,
                }));
            }
        }
        self.live_tasks();
        self.task_notice(&note);
        self.prune_tasks();
    }

    /// The tasks that worked in this session's tree since `since` (ms since the epoch), or still
    /// do, as `/undo` names them: their agents change the same files this session's turns do,
    /// so an undo back to then would undo their changes too.
    pub(super) fn tasks_since(&self, since: Option<u64>) -> Vec<String> {
        let Some(since) = since else {
            return Vec::new();
        };
        self.peer_tasks
            .iter()
            .filter(|task| task.ended.is_none_or(|ended| ended >= since))
            .map(|task| format!("task {}", task.view.borrow().id))
            .collect()
    }

    /// How many tasks are running now, which a `Status` and this session's file say.
    pub(super) fn tasks_running(&self) -> usize {
        self.peer_tasks
            .iter()
            .filter(|task| !task.view.borrow().state.terminal())
            .count()
    }

    /// Says in this session's file that a task started or ended, so another session in the
    /// tree knows this one changed files while its own turn sat idle.
    fn live_tasks(&mut self) {
        let running = self.tasks_running();
        if let Some(live) = &mut self.live {
            live.update(|e| {
                e.tasks = running;
                e.task_since = live::now();
            });
        }
    }

    /// A line about a task. While the model is in the middle of a paragraph it waits with the
    /// answered asides for the paragraph to end, instead of cutting it in two.
    fn task_notice(&mut self, text: &str) {
        if self.busy.is_some() && self.last_kind == Kind::Text && !self.held_blank {
            self.held_asides
                .push(Line::Row(format!("{DIM}{}{RESET}", text::sanitize(text))));
        } else {
            self.notice(text);
        }
    }

    /// The task's prompt: what it was asked, and what a task it continues left behind.
    fn task_prompt(&self, text: &str, after: Option<&str>) -> Result<String, String> {
        let mut prompt = text.to_string();
        if let Some(id) = after {
            let previous = self
                .peer_tasks
                .iter()
                .find(|task| task.view.borrow().id == id)
                .map(|task| task.view.borrow().clone());
            let Some(previous) = previous else {
                return Err(format!("no task {id} here; `killi tasks` lists them"));
            };
            let left = previous
                .report
                .as_deref()
                .or(previous.message.as_deref())
                .unwrap_or("It left no report.");
            prompt = format!(
                "{text}\n\n<previous task=\"{}\" state=\"{}\">\n{left}\n</previous>",
                previous.id,
                previous.state.as_str()
            );
            if let Some(log) = &previous.log {
                prompt.push_str(&format!("\nIts log is {}.", log.display()));
            }
        }
        Ok(prompt)
    }

    /// Keeps the finished tasks this session can still be asked about bounded: the oldest go
    /// once a new one ends, so a long session doesn't hold every report it ever gave.
    fn prune_tasks(&mut self) {
        while self
            .peer_tasks
            .iter()
            .filter(|task| task.view.borrow().state.terminal())
            .count()
            > KEPT_TASKS
        {
            let Some(oldest) = self
                .peer_tasks
                .iter()
                .position(|task| task.view.borrow().state.terminal())
            else {
                break;
            };
            self.peer_tasks.remove(oldest);
        }
    }
}

/// Answers a request that took one reply.
pub(super) fn answer(reply: live::Reply, value: Value) {
    if let live::Reply::Once(answer) = reply {
        let _ = answer.send(value);
    }
}

/// Says why a request can't be answered, on whichever reply it came with: a `Follow` always
/// answers with a task's events, so a refusal is one too.
pub(super) fn refuse(reply: live::Reply, why: &str) {
    match reply {
        live::Reply::Once(answer) => {
            let _ = answer.send(json!({"error": why}));
        }
        live::Reply::Stream(events) => {
            let _ = events.send(live::event_line(live::TaskState::Rejected, Some(why)));
        }
    }
}
