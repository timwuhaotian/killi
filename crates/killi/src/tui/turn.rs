use super::*;

/// The question in `/btw <question>`, if that is what `text` is.
pub(super) fn aside_question(text: &str) -> Option<String> {
    let rest = text.trim().strip_prefix("/btw")?;
    rest.starts_with(char::is_whitespace)
        .then(|| rest.trim().to_string())
        .filter(|q| !q.is_empty())
}

/// Whether a reply says it left work undone, by the words replies use for that.
pub(super) fn leaves_something_for_later(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "for later",
        "follow-up",
        "follow up",
        "deferred",
        "left out",
        "not done",
        "out of scope",
        "didn't do",
        "后续",
        "待办",
        "留到",
        "之后再",
        "没有做",
        "未做",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
}

/// The queued messages the agent has taken, oldest first: all but the `waiting` ones still in
/// the steer queue.
pub(super) fn take_sent(queued: &mut Vec<String>, waiting: usize) -> Vec<String> {
    queued
        .drain(..queued.len().saturating_sub(waiting))
        .collect()
}

impl App {
    /// Keys while the agent works. Returns true to interrupt it.
    pub(super) fn on_key_busy(&mut self, event: TermEvent) -> bool {
        let key = match event {
            TermEvent::Paste(text) => {
                self.paste(&text);
                return false;
            }
            TermEvent::Resize(columns, rows) => {
                self.resize(columns, rows);
                return false;
            }
            TermEvent::Mouse(mouse) => {
                self.on_mouse(mouse);
                return false;
            }
            TermEvent::Key(key) if key.kind != KeyEventKind::Release => {
                self.clear_selection();
                if !self.page_key(&key) {
                    self.history.follow();
                }
                key
            }
            _ => return false,
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.asking.is_some() {
            return self.on_key_asking(key);
        }
        if let Some(menu) = self.suggestions()
            && let Some(idle) = self.on_key_menu(key, &menu)
        {
            if let Idle::Input(Input::Command(name, arg, _)) = idle {
                self.command_while_busy(&name, &arg);
            }
            return false;
        }
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Char('c') if ctrl && self.editor.selection().is_some() => {
                self.copy_selection();
                self.editor.deselect();
            }
            KeyCode::Char('c') if ctrl && !self.editor.is_empty() => self.editor.clear(),
            KeyCode::Char('c') if ctrl => return true,
            KeyCode::Enter if !newline_key(&key) => self.send_busy(),
            KeyCode::Char('s') if ctrl => {
                self.send_busy();
                self.send_now = !self.controls.steer.borrow().is_empty();
                return self.send_now;
            }
            _ => self.edit_key(key),
        }
        false
    }

    /// Enter while the agent works: a /btw question or a command runs now, a skill queues
    /// its expanded prompt, anything else is queued for the agent's next step.
    pub(super) fn send_busy(&mut self) {
        if let Some(question) = aside_question(self.editor.text()) {
            self.aside_asked = Some(question);
            self.editor.take();
            return;
        }
        if let Some(command) = self.editor.text().trim().strip_prefix('/') {
            let (name, arg) = command
                .split_once(char::is_whitespace)
                .unwrap_or((command, ""));
            let (name, arg) = (name.to_string(), arg.trim().to_string());
            if !is_command(&name)
                && let Some(prompt) = self.skills.invocation(&name, &arg)
            {
                // A skill sent while the agent works queues like a typed message: the queue
                // shows `/name arg` as typed, the agent gets the skill's expanded prompt.
                let sent = match mention::attachment(&self.cwd, &arg) {
                    Some(files) => format!("{prompt}\n\n{files}"),
                    None => prompt,
                };
                let (text, images) = self.editor.take_with_images();
                let shown = text.trim().to_string();
                if !shown.is_empty() {
                    self.controls.steer.borrow_mut().push_back((sent, images));
                    self.queued.push(shown);
                }
                return;
            }
            if self.command_while_busy(&name, &arg) {
                self.editor.take();
            }
            return;
        }
        let (text, images) = self.editor.take_with_images();
        if !text.trim().is_empty() {
            self.controls
                .steer
                .borrow_mut()
                .push_back((mention::expand(&self.cwd, &text), images));
            self.queued.push(text);
        }
    }

    /// The queued messages Ctrl+S sends now, as the next prompt: the typed text with the images
    /// pasted beside it. `@` files are expanded once, when the prompt is sent.
    pub(super) fn send_now_prompt(&mut self) -> Option<(String, Vec<Image>)> {
        let steered: Vec<Steered> = self.controls.steer.borrow_mut().drain(..).collect();
        let sent = std::mem::take(&mut self.queued);
        if sent.is_empty() {
            return None;
        }
        let images = steered
            .into_iter()
            .flat_map(|(_, images)| images)
            .filter(|image| sent.iter().any(|text| text.contains(&image.label)))
            .collect();
        Some((sent.join("\n\n"), images))
    }

    /// Moves the queued messages the agent has taken into the conversation from the live area
    /// to the scrollback, so the queue shows only what Ctrl+S can still send.
    pub(super) fn show_sent(&mut self) {
        let sent = take_sent(&mut self.queued, self.controls.steer.borrow().len());
        if sent.is_empty() {
            return;
        }
        self.flush_partial();
        self.end_thinking();
        for text in sent {
            self.commit_prompt(&text);
        }
    }

    /// A command sent while the agent works: one in `BUSY_COMMANDS` runs now, the others wait
    /// for the turn to end. False when it didn't run.
    pub(super) fn command_while_busy(&mut self, name: &str, arg: &str) -> bool {
        if matches!(name, "effort" | "thinking") {
            if arg.trim().is_empty() {
                self.editor.clear();
                self.editor.insert("/effort ");
                return false;
            }
            self.effort_after_turn(arg);
            return true;
        }
        let ran = self.run_local(name, arg);
        if !ran {
            self.notice(&format!("/{name} runs when the agent is idle: wait for this turn to finish, or press Esc. / lists what runs now."));
        }
        ran
    }

    /// /effort while the agent works: the running turn holds the agent, so the pick is kept and
    /// `read_input` applies it as soon as the turn ends. A bare /effort never reaches here:
    /// `command_while_busy` leaves it in the prompt, where the menu offers the levels.
    fn effort_after_turn(&mut self, arg: &str) {
        let efforts: Vec<&str> = self
            .effort_choices
            .iter()
            .map(|item| item.value.as_str())
            .collect();
        match effort_named(&self.status.model, &efforts, arg) {
            Ok(effort) => {
                self.notice(&format!(
                    "Thinking effort {effort} starts when this turn ends."
                ));
                self.effort_later = Some(effort);
            }
            Err(reason) => self.error(&reason),
        }
    }

    /// Starts a turn on `prompt`, held in the live area until something of the turn is printed.
    pub(super) fn begin_turn(&mut self, prompt: &str, images: &[Image]) {
        self.held_prompt = Some((prompt.to_string(), images.to_vec()));
        self.ask_past.clear();
        self.ask_view = None;
        self.ask_corrections.clear();
        self.begin_busy();
    }

    /// Prints what the user typed as their message block.
    pub(super) fn commit_prompt(&mut self, prompt: &str) {
        self.commit(String::new());
        let prompt = prompt.to_string();
        self.emit(Line::block(move |width| prompt_rows(&prompt, width)));
        self.commit(String::new());
        self.last_kind = Kind::None;
    }

    /// Prints a resumed session's conversation into the scrollback, as it read when it ran.
    /// The caller loads the log once and replays from its report, so resuming parses once.
    pub(super) fn replay_items(&mut self, items: Vec<Replayed>) {
        for shown in items {
            match shown {
                Replayed::Note(text) => self.notice(&text),
                Replayed::Prompt(prompt) => {
                    let shown: Vec<&str> = prompt.lines().take(REPLAY_PROMPT_ROWS).collect();
                    self.commit_prompt(&shown.join("\n"));
                    let more = prompt.lines().count().saturating_sub(shown.len());
                    if more > 0 {
                        self.notice(&format!(
                            "… {more} more line{}",
                            if more == 1 { "" } else { "s" }
                        ));
                    }
                }
                Replayed::Text(text) => {
                    self.on_agent(Event::Text(text));
                    self.flush_partial();
                }
                Replayed::Tool {
                    summary,
                    output,
                    is_error,
                    brief,
                } => {
                    self.on_agent(Event::ToolStart {
                        id: "replay".into(),
                        summary,
                    });
                    self.on_agent(Event::ToolEnd {
                        id: "replay".into(),
                        is_error,
                        brief,
                        display: None,
                        output,
                    });
                }
            }
        }
        self.commit(String::new());
        self.last_kind = Kind::None;
    }

    pub(super) fn begin_busy(&mut self) {
        self.busy = Some(Instant::now());
        self.spinner = 0;
        self.later_noted = false;
        self.last_end = None;
    }

    pub(super) fn end_busy(&mut self) {
        self.flush_partial();
        self.end_thinking();
        self.running.clear();
        self.swarm = None;
        self.aside = None;
        self.retry = None;
        self.busy = None;
    }

    pub(super) fn finish_turn(
        &mut self,
        agent: &mut Agent,
        ended: Ended,
        before: Totals,
        after: Totals,
    ) {
        let elapsed = self.busy.map(|t| t.elapsed()).unwrap_or_default();
        self.release_prompt();
        self.end_busy();
        if let Some(asking) = self.asking.take() {
            let _ = asking.question.reply.send(String::new());
        }
        self.ask_past.clear();
        self.ask_view = None;
        self.ask_corrections.clear();
        // The typed copies of queued messages: Ctrl+S still sends them from here, and a
        // turn that ends without consuming them puts them (not their `@`-expansions) back.
        let typed = std::mem::take(&mut self.queued);
        let interrupted = matches!(ended, Ended::Interrupted);
        // The one line of the outcome worth putting in the notification, when there is one.
        let mut notice_line = String::new();
        match ended {
            Ended::Interrupted => {
                self.last_end = Some(LastEnd::Interrupted);
                self.begin(Kind::Note);
                if self.send_now {
                    self.commit(format!(
                        "{WARN}Interrupted{RESET} {DIM}to send your message now.{RESET}"
                    ));
                } else {
                    self.commit(format!(
                        "{WARN}Interrupted.{RESET} {DIM}Tell killi what to do instead.{RESET}"
                    ));
                    self.controls.steer.borrow_mut().clear();
                }
            }
            Ended::Done(Err(e)) => {
                self.last_end = Some(LastEnd::Interrupted);
                let why = gist(&format!("{e:#}"));
                notice_line = first_line(&why, 120);
                self.error_report(&e);
            }
            Ended::Done(Ok((text, stop))) => {
                self.last_end = Some(LastEnd::Done);
                notice_line = first_line(&text, 120);
                match stop {
                    Stop::MaxTokens => self.notice("The reply hit the output token limit."),
                    Stop::RepeatBreaker => self.notice("Stopped: the agent kept repeating the same tool call. Tell it what to read or run instead, or pick another model with /model."),
                    Stop::Completed => {}
                }
                if !self.later_noted && leaves_something_for_later(&text) {
                    self.notice("The reply leaves something for later that wasn't noted; ask killi to note it with `later`, or see /later.");
                }
            }
        }
        if self.send_now {
            self.queued = typed;
        } else if !interrupted && agent.pending_steer() {
            let steered = agent.take_steered();
            if !steered.is_empty() {
                let restored: Vec<Steered> = steered
                    .into_iter()
                    .enumerate()
                    .map(|(i, (expanded, images))| {
                        (typed.get(i).cloned().unwrap_or(expanded), images)
                    })
                    .collect();
                restore_steered(&mut self.editor, restored);
                self.notice("Your queued message wasn't sent; it's back in the prompt.");
            }
        }
        let prompt = after.prompt - before.prompt;
        let cached = (after.cached - before.cached) * 100 / prompt.max(1);
        self.commit(String::new());
        let requests = after.requests - before.requests;
        let quick = matches!(self.last_end, Some(LastEnd::Done))
            && elapsed < Duration::from_secs(3)
            && requests <= 1;
        let spent = prompt > 0 || after.completion > before.completion;
        if spent && !quick {
            self.commit(format!(
                "{DIM}{} · {requests} request{} · {} in · {cached}% cached · {} out{RESET}",
                clock(elapsed),
                if requests == 1 { "" } else { "s" },
                short(prompt),
                short(after.completion - before.completion)
            ));
        }
        self.last_kind = Kind::None;
        if self.wants_notify(elapsed) {
            notify(&format!("Done · {}", clock(elapsed)), &notice_line);
        }
    }

    pub(super) fn add_usage(&mut self, usage: &Usage, main: bool) {
        self.status.prompt += usage.prompt;
        self.status.cached += usage.cached;
        self.status.completion += usage.completion;
        if main {
            self.status.context = usage.prompt;
        }
    }

    fn reset_usage(&mut self) {
        self.status.prompt = 0;
        self.status.cached = 0;
        self.status.completion = 0;
        self.status.context = 0;
    }

    /// Starts the next conversation with a clean slate: the queue, the todos, the totals, the
    /// details, other sessions' notes, the snapshots and the context meter go, used by /new
    /// and /resume alike.
    pub(super) fn reset_conversation(&mut self, snapshots: &mut Snapshots, cwd: &Path) {
        // Messages from other sessions were about the conversation being left.
        self.controls.steer.borrow_mut().clear();
        self.queued.clear();
        self.send_now = false;
        self.todos.clear();
        self.reset_usage();
        self.details.clear();
        self.detail_back = 0;
        self.controls.peer_notes.borrow_mut().clear();
        self.peer_tells = 0;
        *snapshots = Snapshots::new(cwd);
    }

    /// Whether the turn just ended earns a desktop notification: a completed turn that ran
    /// long, not one the user ended themselves with Esc or Ctrl+S.
    pub(super) fn wants_notify(&self, elapsed: Duration) -> bool {
        matches!(self.last_end, Some(LastEnd::Done))
            && elapsed >= self.notify_after
            && !self.send_now
    }

    /// Prints the streamed line still being held and lays out the table it held back, ending
    /// the message so the next one starts with fresh Markdown state. True when text was held.
    pub(super) fn flush_partial(&mut self) -> bool {
        self.held_blank = false;
        let tail = self.stream.finish();
        if !tail.is_empty() {
            self.partial.push_str(&tail);
        }
        let mut flushed = false;
        if !self.partial.trim().is_empty() {
            let line = std::mem::take(&mut self.partial);
            let styled = self.markdown.parts(&line);
            flushed = true;
            self.commit_parts(styled);
        }
        self.partial.clear();
        let table = self.markdown.end_message();
        flushed |= !table.is_empty();
        self.commit_parts(table);
        self.release_asides(true);
        flushed
    }

    /// Ends a thought: one dim line when it took a while, with the full text behind Ctrl+O.
    pub(super) fn end_thinking(&mut self) {
        if let Some((started, mut text)) = self.thinking.take() {
            text.push_str(&self.reasoning.finish());
            let seconds = started.elapsed().as_secs_f32();
            let took = clock(started.elapsed());
            self.push_detail("Thinking".into(), text.clone());
            if seconds < 2.0 {
                return;
            }
            let first = first_sentence(&text);
            let gist = if first.is_empty() {
                String::new()
            } else {
                format!(": {first}")
            };
            let line = format!("{DIM}∴ Thought for {took}{gist}{RESET}");
            self.begin(Kind::Tool);
            self.emit(Line::Fit(line));
        }
    }

    pub(super) fn show_detail(&mut self) {
        if self.details.is_empty() {
            self.notice("Nothing to expand yet.");
            return;
        }
        let back = self.detail_back % self.details.len();
        let (title, body) = self.details[self.details.len() - 1 - back].clone();
        self.detail_back = back + 1;
        let place = if self.details.len() > 1 {
            format!(" ({}/{})", back + 1, self.details.len())
        } else {
            String::new()
        };
        self.begin(Kind::Note);
        self.commit(format!("{DIM}── {title}{place} ──{RESET}"));
        let lines: Vec<&str> = body.lines().collect();
        for line in lines.iter().take(400) {
            self.commit(format!("{DIM}{line}{RESET}"));
        }
        if lines.len() > 400 {
            self.commit(format!("{DIM}… {} more lines{RESET}", lines.len() - 400));
        }
        self.commit(String::new());
    }
}
