use super::*;

mod crew;

pub(super) use crew::{Child, ChildDone, Crew, Job, Unfinished, add_stats, tag};

use background::{BgStart, BgWork, Route, moved_note};

const MAX_PARALLEL_AGENTS: usize = 4;
pub(super) const MAX_SWARM_AGENTS_PER_TURN: usize = swarm::MAX_ITEMS;

/// What an item that never started is told: the provider limited one of the group's members,
/// and every one after it would fail the same way.
fn not_run_note(hit: usize, kind: ErrorKind) -> String {
    let limit = match kind {
        ErrorKind::Quota => "has spent its quota",
        _ => "is rate-limiting requests",
    };
    format!(
        "Not run: the provider {limit} (subagent #{} failed on it), so the rest of this group was not run into the same limit. Run the remaining work again once it clears.",
        hit + 1
    )
}

/// One step of a group running in the foreground: a member ended, or the user sent a message.
enum Step {
    Ended(Option<ChildDone>),
    Steered,
}

/// Resolves once the user has a message waiting.
async fn steered(steer: &RefCell<VecDeque<Steered>>) {
    while steer.borrow().is_empty() {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

impl Agent {
    /// Runs a group of `agent` calls in parallel and returns each one's result as a tool output.
    /// Hooks see each call like any other: a blocked call reports why instead of starting. In
    /// orchestrate mode every call starts in the background at once; otherwise the group runs
    /// in the foreground until the user sends a message, which moves what is left there.
    pub(super) async fn run_subagents(
        &mut self,
        group: &[(String, ToolCall)],
    ) -> Vec<tools::Output> {
        let orchestrate = self.orchestrating();
        let mut jobs = Vec::new();
        let mut labels = Vec::new();
        let mut hooked: Vec<Option<Value>> = Vec::new();
        for (id, call) in group {
            self.stats.tool_calls += 1;
            let args = match parse_args(call) {
                Ok(args) => args,
                Err(why) => {
                    self.tool_start(id, "agent (invalid arguments)");
                    jobs.push(Err(invalid_arguments(call, &why, false)));
                    labels.push(String::new());
                    hooked.push(None);
                    continue;
                }
            };
            let label = args["description"]
                .as_str()
                .unwrap_or("subagent")
                .to_string();
            labels.push(label.clone());
            self.tool_start(id, &tools::summary("agent", &args, &self.ctx.cwd));
            let before = self
                .hook(
                    "PreToolUse",
                    Some("agent"),
                    tool_payload("agent", &args, None),
                )
                .await;
            if let Some(reason) = before.block {
                jobs.push(Err(tools::Output::error(format!(
                    "A hook blocked this call: {reason}"
                ))));
                hooked.push(None);
                continue;
            }
            let prompt = args["prompt"].as_str().unwrap_or_default().to_string();
            if prompt.trim().is_empty() {
                jobs.push(Err(tools::Output::error("agent needs a `prompt`.")));
                hooked.push(None);
            } else {
                hooked.push(Some(args.clone()));
                // In orchestrate mode the main agent keeps the conversation, so a subagent
                // starts from it unless told otherwise.
                let fork = args["fork"].as_bool().unwrap_or(orchestrate);
                jobs.push(self.child(&label, None, fork, prompt));
            }
        }
        let ids: Vec<String> = group.iter().map(|(id, _)| id.clone()).collect();
        let mut outputs = if orchestrate {
            self.detach_agents(jobs, &labels)
        } else {
            self.run_agent_group(jobs, &labels, &ids).await
        };
        for ((args, output), id) in hooked.into_iter().zip(outputs.iter_mut()).zip(&ids) {
            if let Some(args) = args {
                self.post_hook("agent", &args, output).await;
                self.save_agent_result(id, output);
            } else if !self.partial_agents.contains_key(id) {
                self.save_agent_result(id, output);
            }
        }
        outputs
    }

    /// Whether this call starts its subagents in the background: orchestrate mode in a session
    /// that takes background work.
    pub(super) fn orchestrating(&self) -> bool {
        self.background_ready() && self.controls.mode.get() == crate::mode::Mode::Orchestrate
    }

    /// Runs `agent` calls in the foreground, `MAX_PARALLEL_AGENTS` at once. A message from the
    /// user meanwhile moves every member still working to the background.
    async fn run_agent_group(
        &mut self,
        jobs: Vec<Job>,
        labels: &[String],
        ids: &[String],
    ) -> Vec<tools::Output> {
        let yields = self.background_ready();
        let steer = self.controls.steer.clone();
        let since = Instant::now();
        let mut crew = Crew::new(jobs, MAX_PARALLEL_AGENTS, self.ctx.clone());
        loop {
            let step = if yields {
                tokio::select! {
                    done = crew.next() => Step::Ended(done),
                    () = steered(&steer) => Step::Steered,
                }
            } else {
                Step::Ended(crew.next().await)
            };
            let done = match step {
                Step::Ended(Some(done)) => done,
                Step::Ended(None) => break,
                Step::Steered => return self.move_agents(crew, labels, since),
            };
            let output = self.child_ended(done.output, done.stats.as_ref());
            if let Some(id) = ids.get(done.index) {
                self.save_agent_result(id, &output);
            }
            crew.store(done.index, output);
        }
        crew.finish().outputs
    }

    /// A member's report with its counts on the screen, its usage added to this agent's.
    fn child_ended(&mut self, mut output: tools::Output, stats: Option<&Stats>) -> tools::Output {
        if let Some(stats) = stats {
            add_stats(&mut self.stats, stats);
            if !output.is_error {
                output = output.brief(format!(
                    "{} requests, {} tool calls",
                    stats.requests, stats.tool_calls
                ));
            }
        }
        output
    }

    /// Moves every member of a foreground `agent` group that hasn't ended to the background,
    /// each as a task of its own, and returns the group's results: reports for the ones that
    /// ended, the task for each that moved.
    fn move_agents(
        &mut self,
        mut crew: Crew,
        labels: &[String],
        since: Instant,
    ) -> Vec<tools::Output> {
        let mut metas = std::mem::take(&mut crew.meta);
        let (outputs, unfinished) = crew.split();
        let mut outputs: Vec<tools::Output> = outputs
            .into_iter()
            .map(|output| output.unwrap_or_else(|| tools::Output::error("No result.")))
            .collect();
        for (index, member) in unfinished {
            let started = matches!(member, Unfinished::Started(_));
            let work: BgWork = match member {
                Unfinished::Started(tagged) => Box::pin(async move { ended(tagged.await) }),
                Unfinished::Waiting(job) => match *job {
                    Err(output) => {
                        outputs[index] = output;
                        continue;
                    }
                    job => {
                        let parent = self.ctx.clone();
                        Box::pin(async move { ended(tag(index, job, parent).await) })
                    }
                },
            };
            outputs[index] = self.detach(
                labels.get(index).cloned().unwrap_or_default(),
                metas.get_mut(index),
                work,
                started,
                true,
                since,
            );
        }
        outputs
    }

    /// Starts every `agent` call of a group in the background (orchestrate mode).
    fn detach_agents(&mut self, jobs: Vec<Job>, labels: &[String]) -> Vec<tools::Output> {
        let mut outputs = Vec::new();
        for (index, job) in jobs.into_iter().enumerate() {
            let child = match job {
                Ok(child) => child,
                Err(output) => {
                    outputs.push(output);
                    continue;
                }
            };
            let mut meta = Some(crew::Meta {
                route: child.route.clone(),
                steer: child.agent.controls.steer.clone(),
                log: log_name(&child.agent),
                sent: child.sent.clone(),
            });
            let parent = self.ctx.clone();
            let work: BgWork = Box::pin(async move { ended(tag(index, Ok(child), parent).await) });
            outputs.push(self.detach(
                labels.get(index).cloned().unwrap_or_default(),
                Some(&mut meta),
                work,
                false,
                false,
                Instant::now(),
            ));
        }
        outputs
    }

    /// Launches one `agent` call's work as a background task, points its events there, and
    /// returns the call's result.
    fn detach(
        &mut self,
        label: String,
        meta: Option<&mut Option<crew::Meta>>,
        work: BgWork,
        started: bool,
        by_message: bool,
        since: Instant,
    ) -> tools::Output {
        let meta = meta.and_then(Option::take);
        let id = self.launch(
            BgStart {
                label: label.clone(),
                members: 1,
                logs: meta.iter().map(|meta| meta.log.clone()).collect(),
                sent: meta.iter().map(|meta| meta.sent.clone()).collect(),
                items: Vec::new(),
                steer: meta.as_ref().map(|meta| meta.steer.clone()),
                started,
                since,
            },
            work,
        );
        if let Some(meta) = meta {
            meta.route.detach(&self.controls.background, id);
        }
        // Its calls in flight leave the screen's running rows; the registry follows it now.
        (self.sink)(Event::Sub {
            label,
            slot: None,
            event: Box::new(Event::Detached),
        });
        moved_note(id, by_message)
    }

    /// Runs an `agent_swarm` call: one fresh subagent per item, `swarm_parallel` at a time,
    /// bounded by the turn's total swarm budget, with every report in item order. In
    /// orchestrate mode, or once the user sends a message, the swarm goes on in the background
    /// as one task.
    pub(super) async fn run_swarm(&mut self, id: &str, call: &ToolCall) -> tools::Output {
        self.stats.tool_calls += 1;
        let args = match parse_args(call) {
            Ok(args) => args,
            Err(why) => {
                self.tool_start(id, "agent_swarm (invalid arguments)");
                return invalid_arguments(call, &why, false);
            }
        };
        self.tool_start(id, &tools::summary("agent_swarm", &args, &self.ctx.cwd));
        let before = self
            .hook(
                "PreToolUse",
                Some("agent_swarm"),
                tool_payload("agent_swarm", &args, None),
            )
            .await;
        if let Some(reason) = before.block {
            return tools::Output::error(format!("A hook blocked this call: {reason}"));
        }
        let prompts = match swarm::prompts(&args) {
            Ok(prompts) => prompts,
            Err(why) => return tools::Output::error(why),
        };
        if let Err(why) = reserve_swarm(self.swarm_used, prompts.len()) {
            return tools::Output::error(why);
        }
        self.swarm_used += prompts.len();
        let description = args["description"]
            .as_str()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .unwrap_or("swarm")
            .to_string();
        let items: Vec<String> = prompts.iter().map(|(item, _)| item.clone()).collect();
        let mut jobs = Vec::new();
        for (n, (_, prompt)) in prompts.into_iter().enumerate() {
            let slot = Slot {
                swarm: id.into(),
                index: n,
            };
            jobs.push(self.child(
                &format!("{description} #{}", n + 1),
                Some(slot),
                false,
                prompt,
            ));
        }
        // Persisted as the swarm runs, so an interruption or a resume keeps what finished
        // instead of running the whole swarm again; a failure here only loses the resume.
        let logs: Vec<String> = jobs
            .iter()
            .map(|job| match job {
                Ok(child) => log_name(&child.agent),
                Err(_) => String::new(),
            })
            .collect();
        let since = Instant::now();
        let crew = Crew::new(jobs, self.swarm_parallel.get(), self.ctx.clone());
        if self.orchestrating() {
            let mut report = self.move_swarm(crew, &description, &items, &logs, false, since);
            self.post_hook("agent_swarm", &args, &mut report).await;
            return report;
        }
        let _ = self
            .session
            .record(&json!({"type": "swarm", "id": id, "items": items, "logs": logs}));
        self.partial_swarm = Some(PartialSwarm {
            id: id.into(),
            items: items.clone(),
            results: vec![None; items.len()],
            logs: logs.clone(),
        });
        (self.sink)(Event::Swarm {
            id: id.into(),
            description: description.clone(),
            items: items.clone(),
        });
        let yields = self.background_ready();
        let steer = self.controls.steer.clone();
        let mut crew = crew;
        loop {
            let step = if yields {
                tokio::select! {
                    done = crew.next() => Step::Ended(done),
                    () = steered(&steer) => Step::Steered,
                }
            } else {
                Step::Ended(crew.next().await)
            };
            let done = match step {
                Step::Ended(Some(done)) => done,
                Step::Ended(None) => break,
                Step::Steered => {
                    // The call is answered by the task it moved to, not by partial results.
                    self.partial_swarm = None;
                    let mut report =
                        self.move_swarm(crew, &description, &items, &logs, true, since);
                    self.post_hook("agent_swarm", &args, &mut report).await;
                    return report;
                }
            };
            let index = done.index;
            let output = self.child_ended(done.output, done.stats.as_ref());
            if let Some(partial) = &mut self.partial_swarm
                && let Some(result) = partial.results.get_mut(index)
            {
                *result = Some((output.text.clone(), output.is_error));
                let _ = self.session.record(&json!({
                    "type": "swarm_result",
                    "id": partial.id,
                    "index": index,
                    "is_error": output.is_error,
                    "text": output.text,
                }));
            }
            crew.store(index, output);
        }
        let children = crew.finish();
        let logs = unrun_logs(logs, &children.unrun);
        let mut report = swarm::report(
            &self.ctx,
            &items,
            &children.outputs,
            &logs,
            MAX_RESULT_BYTES,
        );
        self.post_hook("agent_swarm", &args, &mut report).await;
        report
    }

    /// Hands a swarm to the background as one task that reports every item in order when the
    /// last member ends.
    fn move_swarm(
        &mut self,
        mut crew: Crew,
        description: &str,
        items: &[String],
        logs: &[String],
        by_message: bool,
        since: Instant,
    ) -> tools::Output {
        let metas = std::mem::take(&mut crew.meta);
        let sent: Vec<String> = metas
            .iter()
            .map(|meta| {
                meta.as_ref()
                    .map(|meta| meta.sent.clone())
                    .unwrap_or_default()
            })
            .collect();
        let done_so_far = crew.done();
        let ctx = self.ctx.clone();
        let (report_items, report_logs) = (items.to_vec(), logs.to_vec());
        let work: BgWork = Box::pin(async move {
            let mut stats = Stats::default();
            while let Some(done) = crew.next().await {
                if let Some(used) = &done.stats {
                    add_stats(&mut stats, used);
                }
                crew.store(done.index, done.output);
            }
            let children = crew.finish();
            let logs = unrun_logs(report_logs, &children.unrun);
            let report = swarm::report(
                &ctx,
                &report_items,
                &children.outputs,
                &logs,
                MAX_RESULT_BYTES,
            );
            (report, stats)
        });
        let id = self.launch(
            BgStart {
                label: description.to_string(),
                members: items.len(),
                logs: logs.to_vec(),
                sent,
                items: items.to_vec(),
                steer: None,
                started: by_message,
                since,
            },
            work,
        );
        if let Some(task) = self.controls.background.borrow_mut().tasks.get_mut(&id) {
            task.finished_members = done_so_far;
        }
        for meta in metas.into_iter().flatten() {
            meta.route.detach(&self.controls.background, id);
        }
        moved_note(id, by_message)
    }

    fn save_agent_result(&mut self, id: &str, output: &tools::Output) {
        self.partial_agents
            .insert(id.to_string(), (output.text.clone(), output.is_error));
        if let Err(error) = self.session.record(&json!({
            "type": "agent_result", "id": id, "text": output.text, "is_error": output.is_error
        })) {
            (self.sink)(Event::Notice(format!(
                "Could not save agent result for {id}; it may be lost on resume: {error:#}"
            )));
        }
    }

    /// A subagent ready to run `prompt`, or why it couldn't start.
    fn child(&mut self, label: &str, slot: Option<Slot>, fork: bool, prompt: String) -> Job {
        let route = Route::default();
        self.subagent(label, slot, fork, &route)
            .map(|agent| Child {
                agent,
                sent: child_prompt(&prompt),
                route,
            })
            .map_err(|e| tools::Output::error(format!("Could not start the subagent: {e:#}")))
    }

    /// A subagent with the same model, system prompt and tools, so its requests hit the same
    /// cached prefix. A fork also starts with this conversation so far, already accounted as
    /// billed, so its size isn't estimated a second time on top of the parent's billing.
    pub(super) fn subagent(
        &mut self,
        label: &str,
        slot: Option<Slot>,
        fork: bool,
        route: &Route,
    ) -> Result<Agent> {
        self.children += 1;
        let (mut session, used) = self.session.child(self.children)?;
        self.children = used;
        let history: Vec<Value> = if fork {
            self.messages[..self.current_reply].to_vec()
        } else {
            vec![self.messages[0].clone()]
        };
        let messages = Rc::new(history);
        let (inherited_context, inherited_len) = if fork {
            (self.stats.context, messages.len())
        } else {
            (0, 0)
        };
        session.record(&json!({"type": "session", "id": session.id, "version": env!("CARGO_PKG_VERSION"), "model": self.client.model.alias, "context_window": self.client.model.context_window, "tools": *self.tools}))?;
        session.record(&json!({"type": "compaction", "messages": *messages}))?;
        let announce_plan = !fork && self.controls.plan.get();
        let mut agent = self.child_agent(
            session,
            messages,
            label,
            slot,
            route,
            inherited_context,
            inherited_len,
        );
        if fork {
            // A fork's requests open with this conversation as sent, so they belong under its key.
            agent.cache_key = self.cache_key.clone();
        }
        if announce_plan {
            agent.push(json!({"role": "user", "content": plan::ON}))?;
            agent.plan_announced = true;
        }
        Ok(agent)
    }

    /// A subagent on `messages`, logging to `session`: this agent's model, tools and settings,
    /// one level down. `context` and `accounted` say how much of the history is already billed.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn child_agent(
        &self,
        session: Session,
        messages: Rc<Vec<Value>>,
        label: &str,
        slot: Option<Slot>,
        route: &Route,
        inherited_context: u64,
        inherited_len: usize,
    ) -> Agent {
        let sink = route.sink(self.sink.clone(), label.to_string(), slot);
        Agent {
            client: self.client.clone(),
            // A subagent inherits the session's backup, so one the provider rate-limits
            // finishes its work on the backup instead of losing it: the swarm's report
            // still holds every item, and the model that takes over doesn't run it again.
            backup: self.backup.clone(),
            tools: self.tools.clone(),
            messages,
            // Its background tasks are its own while it runs; what it leaves running moves to
            // this agent when it ends.
            ctx: Rc::new(self.ctx.detached()),
            cache_key: format!("{}/{}", self.cache_key, session.id),
            session,
            stats: Stats {
                context: inherited_context,
                ..Stats::default()
            },
            controls: Controls {
                // The session's registry, so the git guard knows who else works in the tree.
                background: self.controls.background.clone(),
                steer: Rc::default(),
                plan: self.controls.plan.clone(),
                swarm: Rc::default(),
                verbosity: Rc::default(),
                mode: Rc::default(),
                goal: Rc::default(),
                interactive: false,
                aside: Rc::default(),
                here: self.controls.here.clone(),
                peer_notes: Rc::default(),
            },
            settings: self.settings.clone(),
            swarm_parallel: Cell::new(self.swarm_parallel.get()),
            sink,
            ids: self.ids.clone(),
            repeats: HashMap::new(),
            awake: self.awake.clone(),
            near_repeats: HashMap::new(),
            deferred_messages: Vec::new(),
            accounted_messages: inherited_len,
            compact_floor: 0,
            swarm_used: 0,
            partial_swarm: None,
            partial_agents: HashMap::new(),
            reasoning_noted: false,
            todos: Vec::new(),
            calls_since_todo: 0,
            plan_announced: self.plan_announced,
            swarm_announced: false,
            concise_announced: false,
            orchestrate_announced: false,
            goal_announced: None,
            peers_announced: Vec::new(),
            interrupted: false,
            depth: self.depth + 1,
            route: route.clone(),
            paused: Default::default(),
            children: 0,
            pair: None,
            pair_exec: false,
            current_reply: 0,
            session_started: true,
            snapshot_wait: None,
            instructions: self.instructions.clone(),
            next_notes: Vec::new(),
            pending_prompt: None,
            hook_notified: RefCell::default(),
        }
    }
}

pub(super) const CHILD_REMINDER: &str = "&lt;system-reminder>You are a subagent and cannot start other subagents. Do not call agent or agent_swarm, even if earlier instructions or a mode such as swarm or orchestrate ask you to delegate. Do the assigned work yourself, without waiting on anything in the background, and report any blockers to your parent.&lt;/system-reminder>";

fn child_prompt(prompt: &str) -> String {
    format!("{}\n\n{CHILD_REMINDER}", subagent_prompt(prompt))
}

/// The task prompt with its reporting convention: the parent tells a real result from a
/// stuck subagent by the final line, without parsing free text in code. A task another session
/// sent reports the same way.
pub(crate) fn subagent_prompt(prompt: &str) -> String {
    format!(
        "{prompt}\n\nEnd your final message with one line: `Status: ready` when the work is done, `blocked` (say what you need), `failed` (say what failed), or `waiting` (say what you wait on)."
    )
}

/// A subagent's log file name in the session folder.
fn log_name(agent: &Agent) -> String {
    agent
        .session
        .log_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The swarm's log names with the ones that never started left empty, so the report doesn't
/// point at a log with nothing in it.
fn unrun_logs(logs: Vec<String>, unrun: &[usize]) -> Vec<String> {
    logs.into_iter()
        .enumerate()
        .map(|(n, log)| {
            if unrun.contains(&n) {
                String::new()
            } else {
                log
            }
        })
        .collect()
}

/// A background member's report and usage, from how it ended.
fn ended(done: ChildDone) -> (tools::Output, Stats) {
    (done.output, done.stats.unwrap_or_default())
}

pub(super) fn reserve_swarm(used: usize, requested: usize) -> Result<(), String> {
    let remaining = MAX_SWARM_AGENTS_PER_TURN.saturating_sub(used);
    if requested > remaining {
        return Err(format!(
            "This turn's swarm budget has {remaining} subagents left, but this call needs {requested}. Use fewer items or finish with the existing reports; the total limit is {MAX_SWARM_AGENTS_PER_TURN}."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_task_overrides_inherited_delegation_instructions() {
        let prompt = child_prompt("Do the task.");
        assert!(prompt.starts_with("Do the task."));
        assert!(prompt.contains("Status: ready"));
        assert!(prompt.ends_with(CHILD_REMINDER));
        assert!(CHILD_REMINDER.contains("agent_swarm"));
        assert!(!subagent_prompt("Do the task.").contains(CHILD_REMINDER));
    }
}
