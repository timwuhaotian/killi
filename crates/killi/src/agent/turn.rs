use super::*;

const MAX_CONTINUATIONS: u32 = 3;

/// Told at the end of a turn that leaves commands running, after the list of them.
const LEFT_RUNNING: &str = "Stop with task_stop what nothing needs anymore, such as a server or watcher started for a test or a check. If one should keep running, say so in your reply.";

/// Told once when a reply ends on a colon with no tool call after it: the sentence promised a
/// lookup or a list and the turn ended before giving it.
pub(super) const UNFINISHED: &str = "<system-reminder>Your reply ends with a colon, so something was meant to follow it, but nothing did. If you meant to look something up or run something, do it now with a tool call. If you had nothing more to say, answer in one short sentence. Don't repeat your last reply.</system-reminder>";

impl Agent {
    /// Runs one user turn to completion and returns the final reply text.
    pub async fn run(&mut self, user: &str, images: &[Image]) -> Result<(String, Stop)> {
        // The breaker is for a loop inside one turn; a call the user asks for again is not one.
        self.repeats.clear();
        self.swarm_used = 0;
        self.pending_prompt = None;
        // A blocked prompt leaves no trace: the notes below are taken only once this passes,
        // so nothing already consumed is lost to a prompt the model never sees.
        let submitted = self
            .hook("UserPromptSubmit", None, json!({"prompt": user}))
            .await;
        if let Some(reason) = submitted.block {
            return Err(anyhow!("a hook blocked this prompt: {reason}"));
        }
        let mut notes = Vec::new();
        if std::mem::take(&mut self.interrupted) {
            notes.push("The user interrupted your previous turn.".to_string());
        }
        notes.extend(std::mem::take(&mut self.next_notes));
        notes.extend(self.settings.mcp.take_notes());
        let changed = self.instructions.changed();
        if !changed.is_empty() {
            notes.push(format!(
                "These instruction files changed on disk since your instructions were loaded; read them again and follow the current version: {}",
                changed.join(", ")
            ));
        }
        if !std::mem::replace(&mut self.session_started, true) {
            notes.extend(
                self.hook("SessionStart", None, json!({"source": "startup"}))
                    .await
                    .context,
            );
        }
        // The notes so far are one-shot; the hook's context comes again with a prompt sent again.
        let carried = notes.clone();
        notes.extend(submitted.context);
        // Messages left while the session was idle come before the prompt, so it stays last.
        self.push_peer_notes()?;
        let at = self.messages.len();
        let content = if notes.is_empty() {
            user.to_string()
        } else {
            format!(
                "<system-reminder>{}</system-reminder>\n\n{user}",
                notes.join("\n")
            )
        };
        self.push(json!({"role": "user", "content": content}))?;
        self.push_images(images)?;
        self.pending_prompt = Some(PendingPrompt {
            at,
            count: self.messages.len() - at,
            notes: carried,
        });
        let mut continuations = 0;
        // Every part of a reply cut off at the output token limit: only their whole text
        // answers the turn, whether shown, reported by a subagent or printed by `--json`.
        let mut continued_text = String::new();
        let mut overflow_compacted = false;
        let mut media_stripped = false;
        let mut stop_blocks = 0;
        let mut told_unfinished = false;
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
                Err(error) if !media_stripped && is_media(&error) => {
                    media_stripped = true;
                    self.strip_images(&error)?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let truncated = reply.finish_reason.as_deref() == Some("length");
            let calls = self.record_reply(&reply)?;
            if calls.is_empty() {
                if truncated && continuations < MAX_CONTINUATIONS {
                    continuations += 1;
                    continued_text.push_str(&reply.text);
                    self.push(json!({"role": "user", "content": "<system-reminder>Your reply was cut off at the output token limit. Continue exactly where it stopped.</system-reminder>"}))?;
                    continue;
                }
                if self.pending_steer() {
                    continue;
                }
                if !truncated && !told_unfinished && ends_with_colon(&reply.text) {
                    told_unfinished = true;
                    (self.sink)(Event::Notice("The reply ended on a colon with nothing after it. Asked the agent to go on.".into()));
                    self.push(json!({"role": "user", "content": UNFINISHED}))?;
                    continue;
                }
                // `-p` stops everything as it exits, so only a session that goes on asks.
                if self.depth == 0 && self.controls.interactive && !truncated && !told_left_running
                {
                    let left = self.ctx.tasks.borrow_mut().left_running();
                    if !left.is_empty() {
                        told_left_running = true;
                        let left = left.join(", ");
                        (self.sink)(Event::Notice(format!(
                            "Still running: {left}. Asked the agent to stop what nothing needs."
                        )));
                        self.push(json!({"role": "user", "content": format!("<system-reminder>Still running: {left}. {LEFT_RUNNING}</system-reminder>")}))?;
                        continue;
                    }
                }
                if self.depth == 0
                    && self.controls.interactive
                    && !truncated
                    && !told_open_tasks
                    && !ends_with_question(&reply.text)
                    && let Some(note) = open_tasks_note(&self.todos)
                {
                    told_open_tasks = true;
                    (self.sink)(Event::Notice(
                        "The task list still has open items. Asked the agent to update it.".into(),
                    ));
                    self.push(json!({"role": "user", "content": note}))?;
                    continue;
                }
                let event = if self.depth == 0 {
                    "Stop"
                } else {
                    "SubagentStop"
                };
                let stop = self
                    .hook(event, None, json!({"stop_hook_active": stop_blocks > 0}))
                    .await;
                if let Some(reason) = stop.block.filter(|_| stop_blocks < 3) {
                    stop_blocks += 1;
                    self.push(json!({"role": "user", "content": format!("<system-reminder>A hook asked you to keep going: {reason}</system-reminder>")}))?;
                    continue;
                }
                continued_text.push_str(&reply.text);
                return Ok((
                    continued_text,
                    if truncated {
                        Stop::MaxTokens
                    } else {
                        Stop::Completed
                    },
                ));
            }
            if self.execute_calls(calls, truncated).await? {
                return Ok((String::new(), Stop::RepeatBreaker));
            }
        }
    }

    /// Everything that goes into the conversation between steps: compaction when it's due,
    /// then messages typed meanwhile, a plan- or swarm-mode change and finished background
    /// tasks. Compaction runs first so nothing just pushed is summarized away.
    pub(super) async fn before_request(&mut self) -> Result<()> {
        if self.compaction_due()
            && let Err(error) = self.compact(None).await
        {
            // Proactive compaction is a saving, not a need: the request may still fit, and an
            // overflow compacts the reduced history on the way back.
            (self.sink)(Event::Notice(format!(
                "Compaction failed: {error:#}. Continuing without it."
            )));
        }
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
            self.push(
                json!({"role": "user", "content": if swarming { swarm::ON } else { swarm::OFF }}),
            )?;
        }
        let goal = self.controls.goal.borrow().clone();
        if goal != self.goal_announced && self.depth == 0 {
            let note = match goal.clone() {
                Some(text) => goal::set_note(&text),
                // Only reached from Some: the guard above rules out None == None.
                None => goal::end_note(self.goal_announced.as_deref().unwrap_or_default()),
            };
            self.goal_announced = goal;
            self.push(json!({"role": "user", "content": note}))?;
        }
        self.push_peer_notes()?;
        if let Some(note) = self.peers_note() {
            self.push(json!({"role": "user", "content": note}))?;
        }
        if self.depth == 0
            && self.calls_since_todo >= STALE_TASKS_AFTER
            && let Some(note) = stale_tasks_note(&self.todos, self.calls_since_todo)
        {
            self.calls_since_todo = 0;
            self.push(json!({"role": "user", "content": note}))?;
        }
        if self.depth == 0 {
            // Subagents share this task list: reading it here would swallow the notices the
            // parent still has to report.
            let finished = self.ctx.tasks.borrow_mut().newly_finished();
            if !finished.is_empty() {
                let text = format!(
                    "<system-reminder>{} Read the output with task_output.</system-reminder>",
                    finished.join(" ")
                );
                self.push(json!({"role": "user", "content": text}))?;
            }
        }
        let found = self.instructions.take_found();
        if !found.is_empty() {
            let list: String = found.iter().map(|p| format!("\n- {p}")).collect();
            self.push(json!({"role": "user", "content": format!("<system-reminder>These AGENTS.md files apply to paths you just used but aren't in your instructions. Read them before changing anything in those folders:{list}</system-reminder>")}))?;
        }
        Ok(())
    }

    /// Takes back the turn's prompt when the model has answered none of it, as if it was never
    /// sent: the prompt and its images leave the history, the notes it carried wait for the next
    /// prompt, and the log records the shorter history. The reminders appended after it stay,
    /// and so does everything before it, so the cached prefix still hits. False once a reply or
    /// a rewrite has come after it. For the terminal, which owns that call.
    pub fn retract(&mut self) -> Result<bool> {
        let Some(prompt) = self.pending_prompt.take() else {
            return Ok(false);
        };
        let end = prompt.at + prompt.count;
        if self.messages.len() < end || self.messages[prompt.at]["role"] != "user" {
            return Ok(false);
        }
        Rc::make_mut(&mut self.messages).drain(prompt.at..end);
        self.accounted_messages = self.accounted_messages.min(prompt.at);
        let mut notes = prompt.notes;
        notes.append(&mut self.next_notes);
        self.next_notes = notes;
        self.share();
        self.session.record(&json!({
            "type": "compaction",
            "cause": {"retracted": prompt.at},
            "messages": *self.messages,
        }))?;
        Ok(true)
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
            [] => {
                "No other killi session works in this repository now; told the agent.".to_string()
            }
            [one] => format!(
                "Session {} also works in this repository; told the agent to leave its changes alone.",
                one.pid
            ),
            many => format!(
                "{} other sessions also work in this repository; told the agent to leave their changes alone.",
                many.len()
            ),
        };
        (self.sink)(Event::Notice(shown));
        Some(live::tree_note(&peers, live::now()))
    }
}

/// Whether the reply ends by asking the user something: its last non-empty line ends with
/// `?` (or the full-width `？` of Chinese and Japanese replies). An open task list isn't re-announced then; the question already waits for the user.
pub(super) fn ends_with_question(text: &str) -> bool {
    last_line_ends_with(text, &['?', '？'])
}

/// Whether the reply ends on a colon (or the full-width `：`): it announces something, a lookup
/// or a list, that the turn stopped before giving.
pub(super) fn ends_with_colon(text: &str) -> bool {
    last_line_ends_with(text, &[':', '：'])
}

fn last_line_ends_with(text: &str, marks: &[char]) -> bool {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim_end().ends_with(marks))
}

/// Tool calls the task list may go unchanged for with an item open before the model is
/// reminded of it mid-turn; otherwise a long turn can finish every item with the screen
/// still showing the first one in progress.
pub(super) const STALE_TASKS_AFTER: usize = 10;

/// Told to the agent mid-turn when its task list hasn't changed in `calls` tool calls with
/// items still open. None when the list is empty or finished.
pub(super) fn stale_tasks_note(todos: &[Todo], calls: usize) -> Option<String> {
    let current = todos
        .iter()
        .find(|t| t.status == "in_progress")
        .or_else(|| todos.iter().find(|t| t.status != "completed"))?;
    Some(format!(
        "<system-reminder>Your task list hasn't changed in {calls} tool calls; `{}` still shows as the current item. If it or others are finished, call todo_write now with them completed and the next one in_progress; if not, carry on.</system-reminder>",
        truncate(&current.content, 120)
    ))
}

/// Told to the agent when it ends a turn with its task list unfinished, which shows on the
/// user's screen as work still going: finished items are marked done, or the rest is carried on.
pub(super) fn open_tasks_note(todos: &[Todo]) -> Option<String> {
    let open: Vec<&str> = todos
        .iter()
        .filter(|t| t.status != "completed")
        .map(|t| t.content.as_str())
        .collect();
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
