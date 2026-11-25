use super::*;

/// Makes `session`'s log the one a crash is noted in. Only a session the user runs calls this:
/// a subagent or a task another session sent would otherwise take the crash record of the
/// session it runs in.
fn note_crash_log(session: &Session) {
    if let Ok(mut crash_log) = CRASH_LOG.lock() {
        *crash_log = Some(session.log_path.clone());
    }
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
        // A new session never inherits another session's queued messages through shared controls.
        controls.steer.borrow_mut().clear();
        note_crash_log(&session);
        Self::begin(
            client, cwd, system, tools, settings, controls, sink, session,
        )
    }

    /// Starts a conversation in `session`: its `session` entry, then the system prompt. A new
    /// session's log is one it just created; a task another session sent brings its own.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn begin(
        client: Client,
        cwd: &Path,
        system: &str,
        tools: Vec<Value>,
        settings: Settings,
        controls: Controls,
        sink: Sink,
        session: Session,
    ) -> Result<Self> {
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

    /// Continues a logged session with its original system prompt, so its cached prefix
    /// hits. Takes the log's load, so callers that picked the model from it don't parse
    /// the log twice.
    #[allow(clippy::too_many_arguments)]
    pub fn resume_reported(
        client: Client,
        backup: Option<Client>,
        cwd: &Path,
        session: Session,
        report: crate::session::LoadReport,
        tools: Vec<Value>,
        settings: Settings,
        controls: Controls,
        sink: Sink,
    ) -> Result<Self> {
        let crate::session::LoadReport {
            mut loaded,
            skipped,
            repaired,
        } = report;
        let log = session.log_path.clone();
        // A resumed session keeps its goal, re-announced on its first step even when the
        // log still holds the reminder: compaction may have summarized it away.
        if let Some(goal) = &loaded.goal {
            controls.goal.borrow_mut().replace(goal.clone());
        }
        // A resumed session doesn't pick up messages typed during another session's turn.
        controls.steer.borrow_mut().clear();
        note_crash_log(&session);
        let mut agent = Self::assemble(client, cwd, tools, settings, controls, sink, session);
        agent.backup = backup.map(Rc::new);
        agent.plan_announced = last_plan_state(&loaded.messages);
        agent.model_announced = last_model_state(&loaded.messages);
        agent.swarm_announced = swarm::last_swarm_state(&loaded.messages);
        agent
            .swarm_cap
            .set(swarm::last_swarm_cap(&loaded.messages).unwrap_or(0));
        agent.concise_announced = crate::verbosity::last_concise_state(&loaded.messages);
        agent.orchestrate_announced = crate::mode::last_orchestrate_state(&loaded.messages);
        for message in &loaded.messages {
            for call in message["tool_calls"].as_array().into_iter().flatten() {
                if let Some(id) = call["id"].as_str() {
                    agent.ids.insert(id.to_string());
                }
            }
        }
        agent.restore_todos(&loaded.messages);
        agent.accounted_messages = loaded.messages.len();
        agent.messages = Rc::new(loaded.messages);
        agent.stats.context = loaded.context;
        // The prompt in the log is from when the session started: point the model at the
        // current list when the saved memory differs now, since the prompt won't change.
        let current = crate::memory::section(&crate::memory::list());
        let stale = agent
            .messages
            .first()
            .and_then(|message| message["content"].as_str())
            .is_some_and(|text| {
                if current.is_empty() {
                    text.contains("# Memory\n")
                } else {
                    !text.contains(&current)
                }
            });
        if stale {
            agent.note_for_next_prompt(
                "The user's memory changed since this session started; call `memory` with no arguments to read the current list before relying on the prompt's version."
                    .to_string(),
            );
        }
        if repaired > 0 {
            agent.session.record(&json!({
                "type": "compaction",
                "cause": {"dangling_calls": repaired},
                "messages": *agent.messages,
            }))?;
        }
        if skipped > 0 {
            (agent.sink)(Event::Notice(format!(
                "Skipped {skipped} unreadable log {} while resuming; part of the conversation may be missing.",
                if skipped == 1 { "line" } else { "lines" }
            )));
        }
        if let Some(logged) = super::restore::logged_cwd(&log)
            && logged.as_path() != cwd
        {
            (agent.sink)(Event::Notice(format!(
                "This session started in {}, but you are in {}. Commands run here. To continue there instead, run: cd {} && killi -r {}",
                logged.display(),
                cwd.display(),
                logged.display(),
                log.display()
            )));
        }
        // A swarm in flight when the log stopped: its finished members were persisted as
        // they came in, so the interruption keeps their reports instead of losing the swarm.
        for id in unanswered(&agent.messages) {
            if let Some(swarm) = loaded.swarms.remove(&id) {
                agent.partial_swarm = Some(PartialSwarm {
                    id,
                    items: swarm.items,
                    results: swarm.results,
                    logs: swarm.logs,
                });
            }
        }
        agent.partial_agents = loaded.agents;
        // Every id the history names stays taken, ended work's included: a new task 1 would
        // read as the old one wherever the history mentions it. Paused work waits for the
        // terminal under its own id.
        let last = loaded.background.keys().next_back().copied().unwrap_or(0);
        agent
            .ctx
            .tasks
            .borrow_mut()
            .reserve_through(last.max(loaded.last_task));
        agent.paused = loaded.background;
        agent.recover()?;
        agent.share();
        Ok(agent)
    }

    pub(super) fn assemble(
        client: Client,
        cwd: &Path,
        tools: Vec<Value>,
        settings: Settings,
        controls: Controls,
        sink: Sink,
        session: Session,
    ) -> Self {
        let settings_files = settings.instruction_files.clone();
        let ctx = tools::Context::new(cwd.to_path_buf(), session.dir.clone());
        ctx.images.set(client.model.images);
        *ctx.search.borrow_mut() = settings.search.clone();
        let swarm_parallel = Cell::new(settings.swarm_parallel.max(1));
        Self {
            client: Rc::new(client),
            backup: None,
            advisor: None,
            tools: Rc::new(tools),
            messages: Rc::default(),
            ctx: Rc::new(ctx),
            cache_key: session.id.clone(),
            session,
            stats: Stats::default(),
            controls,
            settings: Rc::new(settings),
            swarm_parallel,
            swarm_cap: Cell::new(0),
            sink,
            ids: HashSet::new(),
            repeats: HashMap::new(),
            awake: Default::default(),
            near_repeats: HashMap::new(),
            deferred_messages: Vec::new(),
            accounted_messages: 0,
            compact_floor: 0,
            swarm_used: 0,
            partial_swarm: None,
            partial_agents: HashMap::new(),
            reasoning_noted: false,
            todos: Vec::new(),
            calls_since_todo: 0,
            plan_announced: false,
            swarm_announced: false,
            concise_announced: false,
            orchestrate_announced: false,
            goal_announced: None,
            model_announced: None,
            peers_announced: Vec::new(),
            interrupted: false,
            failed_on: None,
            next_notes: Vec::new(),
            pending_prompt: None,
            hook_notified: RefCell::default(),
            depth: 0,
            route: Default::default(),
            paused: Default::default(),
            children: 0,
            pair: None,
            pair_exec: false,
            current_reply: 0,
            session_started: false,
            snapshot_wait: None,
            instructions: crate::prompt::Instructions::new(
                cwd,
                &settings_files,
                &crate::memory::path(),
            ),
        }
    }

    pub fn sink(&self) -> Sink {
        self.sink.clone()
    }

    /// Holds a snapshot started alongside the first request until the turn's first tool
    /// call: `execute_calls` waits for it there. The caller records the finished tree
    /// itself; a dropped sender just lets the turn run.
    #[allow(dead_code)]
    pub fn set_snapshot_wait(&mut self, wait: oneshot::Receiver<()>) {
        self.snapshot_wait = Some(wait);
    }

    /// Applies `/swarm parallel <n>` to this session's next swarm.
    pub fn set_swarm_parallel(&mut self, parallel: usize) {
        self.swarm_parallel.set(parallel.max(1));
    }

    /// Continues the conversation on another model; the next request starts the cache over,
    /// and the size estimate with it: the old model's billing no longer describes the history.
    /// The log notes it, so a resume continues on this model.
    pub fn switch_model(&mut self, client: Client) -> Result<()> {
        self.switch_client(client)
    }

    /// Swaps the session's provider, resetting the size estimate and noting the switch in the log.
    /// The floor under proactive compaction goes with it: it was measured in the old estimate.
    pub(super) fn switch_client(&mut self, client: Client) -> Result<()> {
        self.ctx.images.set(client.model.images);
        self.client = Rc::new(client);
        self.stats.context = 0;
        self.accounted_messages = 0;
        self.compact_floor = 0;
        self.share();
        self.record_model()
    }

    /// Sets this session's fallback provider: what it asks once the primary gives up after its
    /// retries on a 4xx, 5xx or network failure. The config's `backup_model` — what `/backup-model` picks — names it for every
    /// session, so no log record is needed to keep it.
    pub fn set_backup(&mut self, client: Client) {
        self.backup = Some(Rc::new(client));
        self.share();
    }

    /// Drops this session's fallback provider.
    pub fn clear_backup(&mut self) {
        self.backup = None;
        self.share();
    }

    /// The backup this session would switch to now: none while it already runs on it, so a
    /// fallback lasts until `/model` returns to another model, which arms it again.
    pub(super) fn armed_backup(&self) -> Option<&Rc<Client>> {
        self.backup
            .as_ref()
            .filter(|backup| backup.model.alias != self.client.model.alias)
    }

    /// The backup model this session falls back to, as configured; none while it runs on it.
    pub fn backup_alias(&self) -> Option<&str> {
        self.armed_backup()
            .map(|client| client.model.alias.as_str())
    }

    /// The session goal set with `/goal`, if any.
    pub fn goal(&self) -> Option<String> {
        self.controls.goal.borrow().clone()
    }

    /// Sets the session goal; the model hears it on its next step. Recorded in the log so
    /// a resume keeps it.
    pub fn set_goal(&mut self, goal: &str) -> Result<()> {
        let goal = goal.split_whitespace().collect::<Vec<_>>().join(" ");
        self.controls.goal.borrow_mut().replace(goal.clone());
        self.session.record(&json!({"type": "goal", "goal": goal}))
    }

    /// Ends the session goal; the model hears it is over on its next step.
    pub fn clear_goal(&mut self) -> Result<()> {
        self.controls.goal.borrow_mut().take();
        self.session.record(&json!({"type": "goal"}))
    }

    /// Keeps a shown recap in the log, so a resume shows it again. Never in the
    /// messages, so the cached prefix still hits; the last one wins.
    pub fn record_recap(&mut self, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        self.session.record(&json!({"type": "recap", "text": text}))
    }

    /// Reasons at another effort from the next request on, on the same model.
    pub fn set_thinking(&mut self, thinking: Thinking) -> Result<()> {
        let mut client = (*self.client).clone();
        client.model.thinking = thinking;
        self.client = Rc::new(client);
        self.share();
        self.record_model()
    }

    /// Lets /btw see the conversation and model as they are now, and a task another session
    /// sent build an agent of this session's own.
    pub(super) fn share(&self) {
        *self.controls.aside.borrow_mut() = Aside {
            client: Some(self.client.clone()),
            backup: self.backup.clone(),
            tools: self.tools.clone(),
            settings: Some(self.settings.clone()),
            messages: Rc::downgrade(&self.messages),
            ctx: Rc::downgrade(&self.ctx),
            cache_key: self.cache_key.clone(),
        };
    }

    fn record_model(&mut self) -> Result<()> {
        let model = &self.client.model;
        self.session.record(
            &json!({"type": "model", "model": model.alias, "effort": model.thinking.label()}),
        )
    }

    /// The model and effort this session runs on now, for a fresh session to start on.
    pub fn client(&self) -> Client {
        (*self.client).clone()
    }

    pub fn thinking(&self) -> &Thinking {
        &self.client.model.thinking
    }

    /// Points web_search at another service without touching the cached conversation, for
    /// /web-search-config.
    pub fn set_search(&mut self, search: crate::tools::SearchSettings) {
        let mut settings = (*self.settings).clone();
        settings.search = search.clone();
        self.settings = Rc::new(settings);
        *self.ctx.search.borrow_mut() = search;
        self.share();
    }

    /// Replaces the system prompt and settings on purpose, for /reload; the next request is
    /// uncached.
    pub fn reload_system(&mut self, system: &str, settings: Settings) -> Result<()> {
        let mut messages = (*self.messages).clone();
        messages[0] = json!({"role": "system", "content": system});
        self.session
            .record(&json!({"type": "compaction", "messages": messages}))?;
        self.messages = Rc::new(messages);
        self.instructions = crate::prompt::Instructions::new(
            &self.ctx.cwd,
            &settings.instruction_files,
            &crate::memory::path(),
        );
        *self.ctx.search.borrow_mut() = settings.search.clone();
        self.swarm_parallel.set(settings.swarm_parallel.max(1));
        self.settings = Rc::new(settings);
        self.share();
        Ok(())
    }

    pub fn model_alias(&self) -> &str {
        &self.client.model.alias
    }

    /// The name to show for the model: its `display_name` when set, else the alias.
    pub fn model_display(&self) -> &str {
        self.client
            .model
            .display_name
            .as_deref()
            .unwrap_or(&self.client.model.alias)
    }

    /// The name to show for the backup model, when it has a display name.
    pub fn backup_display(&self) -> Option<&str> {
        self.armed_backup()
            .and_then(|c| c.model.display_name.as_deref())
    }

    /// Why the primary can't be asked right now, so the backup takes over: after the retries it
    /// still rate-limits, has spent its quota, rejects the key, doesn't know the model, fails on
    /// its side, can't be reached or answers with garbage. The turn loop fixes overflow and media
    /// on the same model, and the backup would refuse or reject a malformed request the same way.
    pub(super) fn fallback_reason(error: &anyhow::Error) -> Option<&'static str> {
        let provider = error.downcast_ref::<ProviderError>()?;
        match provider.kind {
            ErrorKind::Overflow | ErrorKind::Media => None,
            ErrorKind::Refused | ErrorKind::Invalid => None,
            ErrorKind::RateLimited => Some("is rate-limited"),
            ErrorKind::Quota => Some("has spent its quota"),
            ErrorKind::Auth => Some("rejected its credentials"),
            ErrorKind::Transport => Some("can't be reached"),
            ErrorKind::BadResponse => Some("sent an unusable response"),
            ErrorKind::Other => Some("keeps failing"),
        }
    }

    /// Whether the model takes images (`image_in` in its capabilities).
    pub fn sees_images(&self) -> bool {
        self.client.model.images
    }

    pub fn context_window(&self) -> Option<u64> {
        self.client.model.context_window
    }

    /// Takes the memory's current stamp after this session wrote it, so the next turn doesn't
    /// report this session's own change back to the model as someone else's edit.
    pub fn refresh_memory(&mut self) {
        self.instructions.refresh_memory();
    }

    /// Deletes this session's log if nothing was ever said in it, so opening killi and leaving
    /// doesn't leave a file behind. Reminders alone don't count: a prompt taken back with Esc
    /// leaves the ones sent after it; a compaction's summary does count. A pair counts as use:
    /// its note in the log points at the children's logs, which would be orphaned otherwise.
    pub fn discard_if_unused(&self) {
        if self.pair.is_some() {
            return;
        }
        let compacted = |text: &str| text.starts_with(crate::session::COMPACTED_MARK);
        let said = !self.user_prompts().is_empty()
            || self
                .messages
                .iter()
                .skip(1)
                .any(|m| m["role"] != "user" || m["content"].as_str().is_none_or(compacted));
        if !said {
            // A pair that ended lives on only as a `pair` note pointing at its children's
            // logs, and a resumed session has no `pair` agent to guard them.
            let pair_noted = std::fs::read_to_string(&self.session.log_path)
                .is_ok_and(|log| log.contains("\"type\":\"pair\""));
            let intact = crate::session::load_reported(&self.session.log_path)
                .is_ok_and(|report| report.skipped == 0);
            if !pair_noted && intact {
                self.session.discard();
            }
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
}
