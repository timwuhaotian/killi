use super::*;

/// A subagent with its prompt, or the result of a call that couldn't start one.
type Job = std::result::Result<(Agent, String), tools::Output>;

const MAX_PARALLEL_AGENTS: usize = 4;
pub(super) const MAX_SWARM_AGENTS_PER_TURN: usize = swarm::MAX_ITEMS;

impl Agent {
    /// Runs a group of `agent` calls in parallel and returns each one's result as a tool output.
    /// Hooks see each call like any other: a blocked call reports why instead of starting.
    pub(super) async fn run_subagents(
        &mut self,
        group: &[(String, ToolCall)],
    ) -> Vec<tools::Output> {
        let mut jobs = Vec::new();
        let mut hooked: Vec<Option<Value>> = Vec::new();
        for (id, call) in group {
            self.stats.tool_calls += 1;
            let args = match parse_args(call) {
                Ok(args) => args,
                Err(why) => {
                    self.tool_start(id, "agent (invalid arguments)");
                    jobs.push(Err(invalid_arguments(call, &why, false)));
                    hooked.push(None);
                    continue;
                }
            };
            let label = args["description"]
                .as_str()
                .unwrap_or("subagent")
                .to_string();
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
                jobs.push(self.child(
                    &label,
                    None,
                    args["fork"].as_bool().unwrap_or(false),
                    prompt,
                ));
            }
        }
        let mut outputs = self.run_children(jobs, MAX_PARALLEL_AGENTS).await;
        for (args, output) in hooked.into_iter().zip(outputs.iter_mut()) {
            let Some(args) = args else { continue };
            self.post_hook("agent", &args, output).await;
        }
        outputs
    }

    /// Runs an `agent_swarm` call: one fresh subagent per item, `swarm_parallel` at a time,
    /// bounded by the turn's total swarm budget, with every report in item order.
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
            .unwrap_or("swarm");
        let items: Vec<String> = prompts.iter().map(|(item, _)| item.clone()).collect();
        self.partial_swarm = Some(PartialSwarm {
            id: id.into(),
            items: items.clone(),
            results: vec![None; items.len()],
        });
        (self.sink)(Event::Swarm {
            id: id.into(),
            description: description.into(),
            items: items.clone(),
        });
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
        let outputs = self.run_children(jobs, self.settings.swarm_parallel).await;
        let mut report = swarm::report(&self.ctx, &items, &outputs, MAX_RESULT_BYTES);
        self.post_hook("agent_swarm", &args, &mut report).await;
        report
    }

    /// Runs subagents, at most `limit` at once, the next starting as soon as one finishes, and
    /// returns each one's result as a tool output, in order. One that couldn't start reports why.
    async fn run_children(&mut self, jobs: Vec<Job>, limit: usize) -> Vec<tools::Output> {
        let count = jobs.len();
        let mut jobs = jobs.into_iter().enumerate();
        let mut running = FuturesUnordered::new();
        for (index, job) in jobs.by_ref().take(limit.max(1)) {
            running.push(Self::run_child(index, job));
        }
        let mut outputs: Vec<Option<tools::Output>> = Vec::new();
        outputs.resize_with(count, || None);
        while let Some((index, output, stats)) = running.next().await {
            if let Some((next_index, next_job)) = jobs.next() {
                running.push(Self::run_child(next_index, next_job));
            }
            let output = match stats {
                Some(stats) => {
                    let usage = &stats.usage;
                    let total = &mut self.stats.usage;
                    total.prompt += usage.prompt;
                    total.cached += usage.cached;
                    total.completion += usage.completion;
                    total.reasoning += usage.reasoning;
                    self.stats.requests += stats.requests;
                    self.stats.tool_calls += stats.tool_calls;
                    if output.is_error {
                        output
                    } else {
                        output.brief(format!(
                            "{} requests, {} tool calls",
                            stats.requests, stats.tool_calls
                        ))
                    }
                }
                None => output,
            };
            if let Some(partial) = &mut self.partial_swarm
                && let Some(result) = partial.results.get_mut(index)
            {
                *result = Some((output.text.clone(), output.is_error));
            }
            outputs[index] = Some(output);
        }
        outputs
            .into_iter()
            .map(|output| output.expect("every child finished"))
            .collect()
    }

    async fn run_child(index: usize, job: Job) -> (usize, tools::Output, Option<Stats>) {
        let (mut agent, prompt) = match job {
            Ok(job) => job,
            Err(output) => return (index, output, None),
        };
        (agent.sink)(Event::Started);
        let output = match agent.run(&prompt, &[]).await {
            Ok((text, stop)) if !text.trim().is_empty() && stop != Stop::RepeatBreaker => {
                tools::Output::ok(text)
            }
            Ok((_, stop)) => tools::Output::error(format!(
                "The subagent stopped without a final report ({stop:?})."
            )),
            Err(e) => tools::Output::error(format!("The subagent failed: {e:#}")),
        };
        (agent.sink)(Event::Finished {
            is_error: output.is_error,
        });
        // Only its counts outlive it: its conversation and log file are freed here, not
        // when the slowest sibling of a swarm finishes.
        (index, output, Some(std::mem::take(&mut agent.stats)))
    }

    /// A subagent ready to run `prompt`, or why it couldn't start.
    fn child(&mut self, label: &str, slot: Option<Slot>, fork: bool, prompt: String) -> Job {
        self.subagent(label, slot, fork)
            .map(|agent| (agent, prompt))
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
        let parent = self.sink.clone();
        let label = label.to_string();
        let sink: Sink = Rc::new(move |event| {
            parent(Event::Sub {
                label: label.clone(),
                slot: slot.clone(),
                event: Box::new(event),
            })
        });
        let announce_plan = !fork && self.controls.plan.get();
        let mut agent = Agent {
            client: self.client.clone(),
            // A subagent inherits the session's backup, so one the provider rate-limits
            // finishes its work on the backup instead of losing it: the swarm's report
            // still holds every item, and the model that takes over doesn't run it again.
            backup: self.backup.clone(),
            tools: self.tools.clone(),
            messages,
            ctx: self.ctx.clone(),
            session,
            stats: Stats {
                context: inherited_context,
                ..Stats::default()
            },
            controls: Controls {
                steer: Rc::default(),
                plan: self.controls.plan.clone(),
                swarm: Rc::default(),
                goal: Rc::default(),
                interactive: false,
                aside: Rc::default(),
                here: self.controls.here.clone(),
                peer_notes: Rc::default(),
            },
            settings: self.settings.clone(),
            sink,
            ids: self.ids.clone(),
            repeats: HashMap::new(),
            accounted_messages: inherited_len,
            compact_floor: 0,
            swarm_used: 0,
            partial_swarm: None,
            reasoning_noted: false,
            todos: Vec::new(),
            calls_since_todo: 0,
            plan_announced: self.plan_announced,
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
            hook_notified: RefCell::default(),
        };
        if announce_plan {
            agent.push(json!({"role": "user", "content": plan::ON}))?;
            agent.plan_announced = true;
        }
        Ok(agent)
    }
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
