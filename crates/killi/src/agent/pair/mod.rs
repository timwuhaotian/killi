//! Pair mode (`/pair`): a mentor model plans and reviews, an executor model carries out
//! the work. Each role is an agent of its own with its own history: the mentor keeps the full
//! picture (the task plus every executor report), the executor keeps its own work plus the
//! mentor's instructions. The session's own history is untouched; the pair shows in the same
//! transcript and keeps its own logs.

use super::*;

/// A pair's two roles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Mentor,
    Executor,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Mentor => "Mentor",
            Role::Executor => "Executor",
        }
    }
}

/// The mentor ends the pair by putting this on a line of its own in a review.
pub const DONE_MARK: &str = "TASK_COMPLETE";

/// Executor turns before the pair stops by itself.
pub const MAX_ROUNDS: u32 = 100;

/// What each role runs on, as `/pair` picked it.
#[derive(Clone, Debug, Default)]
pub struct PairConfig {
    pub mentor_model: String,
    pub mentor_effort: String,
    pub executor_model: String,
    pub executor_effort: String,
}

/// The handles the terminal holds while a pair runs: stopping, interjecting and the round
/// on screen. Shared like `Controls`, since the driving future holds the agent.
#[derive(Clone, Default)]
pub struct PairControls {
    /// `/pair stop` while the pair works: the loop ends after the interrupted turn.
    pub stop: Rc<Cell<bool>>,
    /// Messages typed while the pair works, for the mentor's replan.
    pub interject: Rc<RefCell<VecDeque<Steered>>>,
    /// The round on screen, for `/pair status` while the pair works.
    pub progress: Rc<RefCell<String>>,
}

/// One role's counts: requests, tool calls, input, cached and output tokens.
pub struct RoleCounts {
    pub requests: u32,
    pub tool_calls: u32,
    pub prompt: u64,
    pub cached: u64,
    pub completion: u64,
}

/// How a pair ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The mentor put `TASK_COMPLETE` on a line of its own in a review.
    Done,
    /// `MAX_ROUNDS` executor turns ran without it.
    Budget,
    /// The user ended it with `/pair stop`.
    Stopped,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Done => "done",
            Outcome::Budget => "budget",
            Outcome::Stopped => "stopped",
        }
    }
}

/// The next turn to run: a mentor plan (the kickoff or a replan after an interjection), a
/// mentor review of an executor report, or an executor turn on the mentor's instructions.
#[derive(Clone)]
enum Step {
    MentorPlan(String, Vec<Image>),
    MentorReview(String),
    Executor(String),
}

pub struct Pair {
    mentor: Option<Box<Agent>>,
    executor: Option<Box<Agent>>,
    ctl: PairControls,
    config: PairConfig,
    task: String,
    /// Executor turns run so far.
    rounds: u32,
    pending: VecDeque<Step>,
    outcome: Option<Outcome>,
    /// The executor wrote the done mark: only the mentor may end the pair, so the next
    /// handoff repeats the ban.
    executor_marked_done: bool,
    /// Each role's counts, kept when the pair ends and its agents are dropped.
    stats: Option<(Stats, Stats)>,
}

/// Whether `text` ends the pair: the done mark on a line of its own, backticks tolerated.
fn is_done(text: &str) -> bool {
    text.lines().any(|line| {
        line.trim()
            .trim_matches('`')
            .trim()
            .eq_ignore_ascii_case(DONE_MARK)
    })
}

fn kickoff_prompt(task: &str) -> String {
    format!(
        "You are the mentor in a mentor/executor pair working on this task:\n\n{task}\n\nPlan only: you cannot change files, and your tools are read-only. Write the plan the executor carries out: concrete steps, the files and commands each needs, and what done looks like. Do not write {DONE_MARK} in the plan.\n\nAfter each executor turn you get its report back. Then either send concrete feedback for the next round, or — only when the task is truly done — end your message with one line holding exactly `{DONE_MARK}`."
    )
}

fn handoff_prompt(plan: &str, marked_done: bool) -> String {
    let nudge = if marked_done {
        format!(" You wrote {DONE_MARK} last round: only the mentor ends the pair.")
    } else {
        String::new()
    };
    format!(
        "Carry out the mentor's plan. You have the full tools; the mentor reviews your report after each turn and decides what is next. You never end the pair yourself: never write {DONE_MARK}.{nudge}\n\n<plan>\n{plan}\n</plan>\n\nEnd your turn with a report: what changed, the commands you ran and their results, and what is left."
    )
}

fn review_prompt(round: u32, report: &str) -> String {
    format!(
        "The executor reports back on round {round}:\n\n<report>\n{report}\n</report>\n\nReview it against the plan and the task. If the task is truly done, end your message with one line holding exactly `{DONE_MARK}` and nothing else on it. Otherwise send concrete feedback and instructions for the next round."
    )
}

fn replan_prompt(messages: &[String]) -> String {
    format!(
        "The user interrupts with new direction:\n\n{}\n\nRe-plan from everything so far: write the revised plan the executor carries out next.",
        messages.join("\n\n")
    )
}

/// A forwarding sink for a pair child: its text, tools and tasks show as the session's own,
/// under the role header the loop prints before each turn. Tool ids carry the role so the
/// two agents' providers can't hand out the same one; usage stays off the context meter,
/// which measures the session's own conversation.
fn forward_sink(parent: &Sink, role: Role) -> Sink {
    let parent = parent.clone();
    let prefix = role.label().to_lowercase();
    Rc::new(move |event| {
        let tag = |id: String| format!("{prefix}:{id}");
        match event {
            Event::ToolStart { id, summary } => parent(Event::ToolStart {
                id: tag(id),
                summary,
            }),
            Event::ToolEnd {
                id,
                is_error,
                brief,
                display,
                output,
            } => parent(Event::ToolEnd {
                id: tag(id),
                is_error,
                brief,
                display,
                output,
            }),
            Event::Usage(usage) => parent(Event::Sub {
                label: role.label().into(),
                slot: None,
                event: Box::new(Event::Usage(usage)),
            }),
            Event::Swarm {
                id,
                description,
                items,
            } => parent(Event::Swarm {
                id: tag(id),
                description,
                items,
            }),
            Event::Sub { label, slot, event } => parent(Event::Sub {
                label: format!("{} {label}", role.label()),
                // The swarm panel matches the call's id, which ToolEnd carries role-tagged.
                slot: slot.map(|Slot { swarm, index }| Slot {
                    swarm: tag(swarm),
                    index,
                }),
                event,
            }),
            // The loop's role headers cover these; children can't ask anyway.
            Event::Started | Event::Finished { .. } => {}
            Event::Ask(question) => {
                let _ = question.reply.send(String::new());
            }
            other => parent(other),
        }
    })
}

impl Agent {
    /// Starts a pair on `task`: a read-only mentor and a full executor, each with its own
    /// model, effort and history. The session's own history is untouched.
    pub fn pair_begin(
        &mut self,
        config: &PairConfig,
        mentor: Client,
        executor: Client,
        task: &str,
        images: Vec<Image>,
    ) -> Result<()> {
        let mut pending = VecDeque::new();
        pending.push_back(Step::MentorPlan(kickoff_prompt(task), images));
        let mentor = self.pair_child(Role::Mentor, mentor)?;
        let executor = match self.pair_child(Role::Executor, executor) {
            Ok(executor) => executor,
            Err(e) => {
                // Without the pair the mentor's log would orphan the session folder.
                let _ = std::fs::remove_file(&mentor.session.log_path);
                return Err(e);
            }
        };
        self.pair = Some(Pair {
            mentor: Some(Box::new(mentor)),
            executor: Some(Box::new(executor)),
            ctl: PairControls::default(),
            config: config.clone(),
            task: task.to_string(),
            rounds: 0,
            pending,
            outcome: None,
            executor_marked_done: false,
            stats: None,
        });
        Ok(())
    }

    /// A pair child: its own model and history, the session's system prompt and tools. The
    /// mentor is plan-locked (and, below the main agent, `exit_plan_mode` can't unlock it);
    /// the executor keeps the full tools, subagents included.
    fn pair_child(&mut self, role: Role, client: Client) -> Result<Agent> {
        self.children += 1;
        let (mut session, used) = self.session.child(self.children)?;
        self.children = used;
        let messages = Rc::new(vec![self.messages[0].clone()]);
        session.record(&json!({"type": "session", "id": session.id, "version": env!("CARGO_PKG_VERSION"), "model": client.model.alias, "context_window": client.model.context_window, "tools": *self.tools}))?;
        session.record(&json!({"type": "compaction", "messages": *messages}))?;
        let mut agent = Agent {
            client: Rc::new(client),
            backup: self.backup.clone(),
            tools: self.tools.clone(),
            messages,
            ctx: self.ctx.clone(),
            session,
            stats: Stats::default(),
            controls: Controls {
                steer: Rc::default(),
                plan: Rc::new(Cell::new(role == Role::Mentor)),
                swarm: Rc::default(),
                goal: Rc::default(),
                interactive: false,
                aside: Rc::default(),
                here: self.controls.here.clone(),
                peer_notes: Rc::default(),
            },
            settings: self.settings.clone(),
            swarm_parallel: Cell::new(self.swarm_parallel.get()),
            sink: forward_sink(&self.sink, role),
            ids: self.ids.clone(),
            repeats: HashMap::new(),
            near_repeats: HashMap::new(),
            deferred_messages: Vec::new(),
            accounted_messages: 0,
            compact_floor: 0,
            swarm_used: 0,
            partial_swarm: None,
            reasoning_noted: false,
            todos: Vec::new(),
            calls_since_todo: 0,
            plan_announced: false,
            swarm_announced: false,
            goal_announced: None,
            peers_announced: Vec::new(),
            interrupted: false,
            depth: self.depth + 1,
            children: 0,
            current_reply: 0,
            session_started: true,
            snapshot_wait: None,
            instructions: self.instructions.clone(),
            next_notes: Vec::new(),
            pending_prompt: None,
            hook_notified: RefCell::default(),
            pair: None,
            pair_exec: role == Role::Executor,
        };
        if role == Role::Mentor {
            agent.push(json!({"role": "user", "content": plan::ON}))?;
            agent.plan_announced = true;
        }
        Ok(agent)
    }

    /// Runs pair turns until the mentor ends it, the budget runs out, or the terminal stops
    /// or interrupts the loop. Each step queues the next, so an interruption resumes where
    /// the queue stands.
    pub async fn pair_drive(&mut self) -> Result<(String, Stop)> {
        loop {
            let ended = self
                .pair
                .as_ref()
                .is_some_and(|pair| pair.outcome.is_some());
            if ended {
                return Ok((String::new(), Stop::Completed));
            }
            let stop = self.pair.as_ref().is_some_and(|pair| pair.ctl.stop.get());
            if stop {
                self.pair_finish(Outcome::Stopped);
                return Ok((String::new(), Stop::Completed));
            }
            // Peek, don't pop: a step stays queued until its turn finishes, so an
            // interruption or a failed turn leaves it for `/pair resume` to retry.
            let step = self
                .pair
                .as_mut()
                .and_then(|pair| pair.pending.front().cloned());
            let Some(step) = step else {
                return Ok((String::new(), Stop::Completed));
            };
            match step {
                Step::MentorPlan(prompt, images) => {
                    self.pair_header(Role::Mentor, "planning");
                    let text = self
                        .pair_run(Role::Mentor, &prompt, &images)
                        .await
                        .map_err(|e| anyhow!("the mentor's turn failed: {e:#}"))?;
                    let pair = self.pair.as_mut().expect("a pair is running");
                    pair.pending.pop_front();
                    let marked = pair.executor_marked_done;
                    pair.pending
                        .push_back(Step::Executor(handoff_prompt(&text, marked)));
                }
                Step::Executor(prompt) => {
                    // The budget is a hard cap: an interjection at the boundary must not let a
                    // hundred-and-first executor turn run.
                    if self
                        .pair
                        .as_ref()
                        .is_some_and(|pair| pair.rounds >= MAX_ROUNDS)
                    {
                        self.pair_finish(Outcome::Budget);
                        return Ok((String::new(), Stop::Completed));
                    }
                    let round = self.pair.as_ref().map_or(1, |pair| pair.rounds + 1);
                    self.pair_header(Role::Executor, &format!("round {round}"));
                    let text = self
                        .pair_run(Role::Executor, &prompt, &[])
                        .await
                        .map_err(|e| anyhow!("the executor's turn failed: {e:#}"))?;
                    let pair = self.pair.as_mut().expect("a pair is running");
                    pair.pending.pop_front();
                    pair.rounds = round;
                    pair.executor_marked_done = is_done(&text);
                    pair.pending.push_back(Step::MentorReview(text));
                }
                Step::MentorReview(report) => {
                    let round = self.pair.as_ref().map_or(0, |pair| pair.rounds);
                    self.pair_header(Role::Mentor, &format!("reviewing round {round}"));
                    let text = self
                        .pair_run(Role::Mentor, &review_prompt(round, &report), &[])
                        .await
                        .map_err(|e| anyhow!("the mentor's review failed: {e:#}"))?;
                    if is_done(&text) {
                        self.pair_finish(Outcome::Done);
                        return Ok((String::new(), Stop::Completed));
                    }
                    let budget_out = self
                        .pair
                        .as_ref()
                        .is_some_and(|pair| pair.rounds >= MAX_ROUNDS);
                    if budget_out {
                        self.pair_finish(Outcome::Budget);
                        return Ok((String::new(), Stop::Completed));
                    }
                    let pair = self.pair.as_mut().expect("a pair is running");
                    pair.pending.pop_front();
                    let marked = pair.executor_marked_done;
                    pair.pending
                        .push_back(Step::Executor(handoff_prompt(&text, marked)));
                }
            }
        }
    }

    /// Runs one role's turn; the other role's agent is untouched. An empty turn breaks the
    /// protocol (a plan, report or review of nothing), so it fails the drive and the
    /// terminal pauses the pair where it stands.
    async fn pair_run(&mut self, role: Role, prompt: &str, images: &[Image]) -> Result<String> {
        let pair = self.pair.as_mut().expect("a pair is running");
        let child = match role {
            Role::Mentor => pair.mentor.as_mut().expect("the mentor runs"),
            Role::Executor => pair.executor.as_mut().expect("the executor runs"),
        };
        let (text, _) = child.run(prompt, images).await?;
        if text.trim().is_empty() {
            bail!(
                "the {} ended its turn with no reply",
                role.label().to_lowercase()
            );
        }
        Ok(text)
    }

    /// Prints the role header a turn shows under.
    fn pair_header(&mut self, role: Role, what: &str) {
        let Some(pair) = self.pair.as_ref() else {
            return;
        };
        let config = &pair.config;
        let (model, effort) = match role {
            Role::Mentor => (&config.mentor_model, &config.mentor_effort),
            Role::Executor => (&config.executor_model, &config.executor_effort),
        };
        let header = format!("{} · {model} ({effort}) — {what}", role.label());
        *pair.ctl.progress.borrow_mut() = format!("round {} · {header}", pair.rounds.max(1));
        (self.sink)(Event::PairTurn {
            role,
            model: model.clone(),
            effort: effort.clone(),
            what: what.to_string(),
        });
    }

    /// Answers the tool calls an interruption left without results in both roles, so either
    /// resumes with a valid history.
    pub fn pair_recover(&mut self) -> Result<()> {
        let Some(pair) = self.pair.as_mut() else {
            return Ok(());
        };
        if let Some(mentor) = pair.mentor.as_mut() {
            mentor.recover()?;
        }
        if let Some(executor) = pair.executor.as_mut() {
            executor.recover()?;
        }
        Ok(())
    }

    /// Hands the queued interjections to the mentor as a replan: whatever was queued is
    /// dropped, and the mentor plans again from its full context. True when any waited.
    pub fn pair_interject(&mut self) -> bool {
        let Some(pair) = self.pair.as_mut() else {
            return false;
        };
        if pair.outcome.is_some() {
            // An interjection that raced the done mark: the pair is over.
            pair.ctl.interject.borrow_mut().clear();
            return false;
        }
        let steered: Vec<Steered> = pair.ctl.interject.borrow_mut().drain(..).collect();
        if steered.is_empty() {
            return false;
        }
        let images: Vec<Image> = steered
            .iter()
            .flat_map(|(_, images)| images.clone())
            .collect();
        let messages: Vec<String> = steered.into_iter().map(|(text, _)| text).collect();
        pair.pending.clear();
        pair.pending
            .push_back(Step::MentorPlan(replan_prompt(&messages), images));
        true
    }

    /// Ends the standing pair as stopped, from `/pair stop`.
    pub fn pair_stop(&mut self) {
        if self.pair_active() {
            self.pair_finish(Outcome::Stopped);
        }
    }

    /// Ends the pair, keeping each role's counts and noting the run in the session's log.
    /// Unknown entry types don't disturb a resume, so the note is only ever read by people.
    fn pair_finish(&mut self, outcome: Outcome) {
        let Some(pair) = self.pair.as_mut() else {
            return;
        };
        if pair.outcome.is_some() {
            return;
        }
        let log_name = |child: &Agent| {
            child
                .session
                .log_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let logs: Vec<String> = [&pair.mentor, &pair.executor]
            .iter()
            .filter_map(|child| child.as_ref().map(|child| log_name(child)))
            .collect();
        let mentor_stats = pair
            .mentor
            .take()
            .map(|mut mentor| std::mem::take(&mut mentor.stats));
        let executor_stats = pair
            .executor
            .take()
            .map(|mut executor| std::mem::take(&mut executor.stats));
        if let (Some(mentor), Some(executor)) = (mentor_stats, executor_stats) {
            pair.stats = Some((mentor, executor));
        }
        let entry = json!({
            "type": "pair",
            "task": pair.task_first_line(),
            "rounds": pair.rounds,
            "outcome": outcome.label(),
            "mentor": pair.config.mentor_model,
            "executor": pair.config.executor_model,
            "logs": logs,
        });
        let _ = self.session.record(&entry);
        if let Some(pair) = self.pair.as_mut() {
            pair.outcome = Some(outcome);
        }
    }

    /// Whether a pair stands that hasn't ended: driving now, or paused.
    pub fn pair_active(&self) -> bool {
        self.pair
            .as_ref()
            .is_some_and(|pair| pair.outcome.is_none())
    }

    /// The terminal's handles to the running pair, if one stands.
    pub fn pair_ctl(&self) -> Option<PairControls> {
        self.pair.as_ref().map(|pair| pair.ctl.clone())
    }

    /// One line on the pair that stands: its models, rounds and where it is.
    pub fn pair_status(&self) -> Option<String> {
        let pair = self.pair.as_ref()?;
        let config = &pair.config;
        let where_at = match pair.outcome {
            Some(outcome) => format!("ended {}", outcome.label()),
            None => pair.ctl.progress.borrow().clone(),
        };
        let where_at = if where_at.is_empty() {
            "starting".to_string()
        } else {
            where_at
        };
        // The live progress counts the round in flight as round 1, so an active pair never
        // shows 0 rounds; an ended pair shows exactly what ran.
        let rounds = if pair.outcome.is_none() {
            pair.rounds.max(1)
        } else {
            pair.rounds
        };
        Some(format!(
            "Pair · mentor {} ({}) + executor {} ({}) · {} round{} · {where_at}",
            config.mentor_model,
            config.mentor_effort,
            config.executor_model,
            config.executor_effort,
            rounds,
            if rounds == 1 { "" } else { "s" },
        ))
    }

    /// Each role's counts for the split `/usage` shows. Reads the running agents, or what
    /// the ended pair kept.
    pub fn pair_split(&self) -> Option<[RoleCounts; 2]> {
        let pair = self.pair.as_ref()?;
        let counts = |stats: &Stats| RoleCounts {
            requests: stats.requests,
            tool_calls: stats.tool_calls,
            prompt: stats.usage.prompt,
            cached: stats.usage.cached,
            completion: stats.usage.completion,
        };
        match (&pair.mentor, &pair.executor, &pair.stats) {
            (Some(mentor), Some(executor), _) => {
                Some([counts(&mentor.stats), counts(&executor.stats)])
            }
            (_, _, Some((mentor, executor))) => Some([counts(mentor), counts(executor)]),
            _ => None,
        }
    }

    /// How the pair that stands ended, if it did.
    pub fn pair_outcome(&self) -> Option<Outcome> {
        self.pair.as_ref().and_then(|pair| pair.outcome)
    }
}

impl Pair {
    /// The task's first line for the session log: the task itself only went to the
    /// children, so the log keeps one line to say what the pair was for.
    fn task_first_line(&self) -> String {
        self.task
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    }
}

#[cfg(test)]
mod tests;
