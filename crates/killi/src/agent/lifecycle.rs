use super::*;

/// The folder the resumed session ran in, from its log's `session` entry: resuming from
/// another folder keeps running here, so the mismatch is worth a notice. Old logs
/// carry no `cwd` and stay silent.
fn logged_cwd(log: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(log).ok()?;
    for chunk in bytes.split(|b| *b == b'\n') {
        if chunk.is_empty() {
            continue;
        }
        let entry: Value = serde_json::from_str(std::str::from_utf8(chunk).ok()?).ok()?;
        if entry["type"] == "session"
            && let Some(cwd) = entry["cwd"].as_str().filter(|cwd| !cwd.is_empty())
        {
            return Some(PathBuf::from(cwd));
        }
    }
    None
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
        log: &Path,
        report: crate::session::LoadReport,
        tools: Vec<Value>,
        settings: Settings,
        controls: Controls,
        sink: Sink,
    ) -> Result<Self> {
        let crate::session::LoadReport { loaded, skipped, repaired } = report;
        let session = Session::open(log)?;
        // A resumed session doesn't pick up messages typed during another session's turn.
        controls.steer.borrow_mut().clear();
        let mut agent = Self::assemble(client, cwd, tools, settings, controls, sink, session);
        agent.backup = backup.map(Rc::new);
        agent.plan_announced = last_plan_state(&loaded.messages);
        for message in &loaded.messages {
            for call in message["tool_calls"].as_array().into_iter().flatten() {
                if let Some(id) = call["id"].as_str() {
                    agent.ids.insert(id.to_string());
                }
            }
        }
        agent.accounted_messages = loaded.messages.len();
        agent.messages = Rc::new(loaded.messages);
        agent.stats.context = loaded.context;
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
        if let Some(logged) = logged_cwd(log)
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
        if let Ok(mut crash_log) = CRASH_LOG.lock() {
            *crash_log = Some(session.log_path.clone());
        }
        ctx.images.set(client.model.images);
        *ctx.search.borrow_mut() = settings.search.clone();
        Self {
            client: Rc::new(client),
            backup: None,
            tools: Rc::new(tools),
            messages: Rc::default(),
            ctx: Rc::new(ctx),
            session,
            stats: Stats::default(),
            controls,
            settings: Rc::new(settings),
            sink,
            ids: HashSet::new(),
            repeats: HashMap::new(),
            accounted_messages: 0,
            compact_floor: 0,
            swarm_used: 0,
            partial_swarm: None,
            reasoning_noted: false,
            todos: Vec::new(),
            calls_since_todo: 0,
            plan_announced: false,
            swarm_announced: false,
            peers_announced: Vec::new(),
            interrupted: false,
            next_notes: Vec::new(),
            hook_notified: RefCell::default(),
            depth: 0,
            children: 0,
            current_reply: 0,
            session_started: false,
            snapshot_wait: None,
            instructions: crate::prompt::Instructions::new(cwd, &settings_files),
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

    /// Continues the conversation on another model; the next request starts the cache over,
    /// and the size estimate with it: the old model's billing no longer describes the history.
    /// The log notes it, so a resume continues on this model.
    pub fn switch_model(&mut self, client: Client) -> Result<()> {
        self.switch_client(client)
    }

    /// Swaps the session's provider, resetting the size estimate and noting the switch in the log.
    pub(super) fn switch_client(&mut self, client: Client) -> Result<()> {
        self.ctx.images.set(client.model.images);
        self.client = Rc::new(client);
        self.stats.context = 0;
        self.accounted_messages = 0;
        self.share();
        self.record_model()
    }

    /// Sets this session's fallback provider: what it asks once the primary gives up on a rate
    /// limit. A new session starts without one, so it is never saved for later sessions.
    pub fn set_backup(&mut self, client: Client) -> Result<()> {
        self.backup = Some(Rc::new(client));
        self.record_backup()
    }

    /// Drops this session's fallback provider.
    pub fn clear_backup(&mut self) -> Result<()> {
        self.backup = None;
        self.record_backup()
    }

    /// Records the backup the way the log records a model switch, so a resume keeps it.
    fn record_backup(&mut self) -> Result<()> {
        self.session.record(&json!({"type": "backup", "model": self.backup.as_ref().map(|c| c.model.alias.as_str())}))
    }

    /// The backup model this session falls back to, as configured.
    pub fn backup_alias(&self) -> Option<&str> {
        self.backup.as_ref().map(|client| client.model.alias.as_str())
    }

    /// Reasons at another effort from the next request on, on the same model.
    pub fn set_thinking(&mut self, thinking: Thinking) -> Result<()> {
        let mut client = (*self.client).clone();
        client.model.thinking = thinking;
        self.client = Rc::new(client);
        self.share();
        self.record_model()
    }

    /// Lets /btw see the conversation and model as they are now.
    pub(super) fn share(&self) {
        *self.controls.aside.borrow_mut() = Aside {
            client: Some(self.client.clone()),
            tools: self.tools.clone(),
            messages: Rc::downgrade(&self.messages),
            ctx: Rc::downgrade(&self.ctx),
            cache_key: self.session.id.clone(),
        };
    }

    fn record_model(&mut self) -> Result<()> {
        let model = &self.client.model;
        self.session.record(&json!({"type": "model", "model": model.alias, "effort": model.thinking.label()}))
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
    }

    /// Replaces the system prompt and settings on purpose, for /reload; the next request is
    /// uncached.
    pub fn reload_system(&mut self, system: &str, settings: Settings) -> Result<()> {
        self.instructions = crate::prompt::Instructions::new(&self.ctx.cwd, &settings.instruction_files);
        let search = settings.search.clone();
        self.settings = Rc::new(settings);
        *self.ctx.search.borrow_mut() = search;
        Rc::make_mut(&mut self.messages)[0] = json!({"role": "system", "content": system});
        self.share();
        self.session.record(&json!({"type": "compaction", "messages": *self.messages}))
    }

    pub fn model_alias(&self) -> &str {
        &self.client.model.alias
    }

    /// Whether a provider failure was the primary rate-limiting its requests — not a spent
    /// quota or anything else — which is the only failure that switches a session to its backup.
    pub(super) fn is_rate_limited(error: &anyhow::Error) -> bool {
        error.downcast_ref::<ProviderError>().is_some_and(|provider| provider.kind == ErrorKind::RateLimited)
    }

    /// Whether the model takes images (`image_in` in its capabilities).
    pub fn sees_images(&self) -> bool {
        self.client.model.images
    }

    pub fn context_window(&self) -> Option<u64> {
        self.client.model.context_window
    }

    /// Deletes this session's log if nothing was ever said in it, so opening killi and leaving
    /// doesn't leave a file behind.
    pub fn discard_if_unused(&self) {
        if self.messages.len() <= 1 {
            self.session.discard();
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
