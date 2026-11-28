//! Writing the history: appending messages in an order every provider accepts, answering the
//! calls an interruption left open, and the notes and steered messages that wait for the next
//! prompt.

use super::*;

impl Agent {
    /// After an interrupted turn, answers every tool call left without a result so the
    /// history stays valid, and notes the interruption for the next prompt.
    pub fn recover(&mut self) -> Result<()> {
        let missing = unanswered(&self.messages);
        let partial = self.partial_swarm.take();
        for id in &missing {
            if let Some((text, is_error)) = self.partial_agents.get(id).cloned() {
                let text = neutralize_reminder_tags(&text).into_owned();
                let content = if text.len() > MAX_RESULT_BYTES {
                    self.shorten(&text)
                } else {
                    text
                };
                self.push_meta(
                    json!({"role": "tool", "tool_call_id": id, "content": content}),
                    Some(json!({"name": "agent", "is_error": is_error})),
                )?;
                self.partial_agents.remove(id);
                continue;
            }
            let content = match &partial {
                Some(partial) if partial.id == *id => {
                    let outputs: Vec<tools::Output> = partial
                        .results
                        .iter()
                        .map(|result| match result {
                            Some((text, false)) => tools::Output::ok(text.clone()),
                            Some((text, true)) => tools::Output::error(text.clone()),
                            None => tools::Output::error("Interrupted by the user before this subagent finished, so it has no result."),
                        })
                        .collect();
                    neutralize_reminder_tags(
                        &swarm::report(
                            &self.ctx,
                            &partial.items,
                            &outputs,
                            &partial.logs,
                            MAX_RESULT_BYTES,
                        )
                        .text,
                    )
                    .into_owned()
                }
                _ if self.messages.iter().flat_map(|message| {
                    message["tool_calls"].as_array().into_iter().flatten()
                }).any(|call| call["id"] == *id && call["function"]["name"] == "agent") => {
                    "Interrupted before this agent finished. It may have performed work or caused side effects; inspect its subagent log before retrying.".to_string()
                }
                _ => "Interrupted before this finished; it may have partly run, so check its effects before running it again.".to_string(),
            };
            self.push(json!({"role": "tool", "tool_call_id": id, "content": content}))?;
        }
        if !missing.is_empty()
            || self
                .messages
                .last()
                .is_some_and(|m| m["role"] == "user" || m["role"] == "tool")
        {
            self.interrupted = true;
        }
        Ok(())
    }

    /// `recover` after a turn that stopped on `error` rather than by the user, so the next
    /// prompt's note names the error instead of an interruption nobody made.
    pub fn recover_failed(&mut self, error: &str) -> Result<()> {
        self.recover()?;
        self.failed_on = self.interrupted.then(|| error.to_string());
        Ok(())
    }

    /// Queues a note for the model's next prompt, through the same channel as the
    /// interruption note. After /undo the terminal calls this with what changed instead of
    /// `mark_interrupted`, so the model hears what was restored.
    /// For the terminal, which owns that call.
    pub fn note_for_next_prompt(&mut self, note: String) {
        self.next_notes.push(note);
    }

    /// Messages typed during the last turn that never reached the model. The terminal drains
    /// these back into the prompt at any turn end other than Esc, which discards them by design.
    /// For the terminal, which owns that call.
    pub fn take_steered(&mut self) -> Vec<Steered> {
        self.controls.steer.borrow_mut().drain(..).collect()
    }

    /// Whether messages typed during the last turn still wait for the next step.
    pub fn pending_steer(&self) -> bool {
        !self.controls.steer.borrow().is_empty()
    }

    /// Keeps the first 4096 and last 1024 bytes, each cut at a character boundary, and saves
    /// the whole result to a file.
    pub(super) fn shorten(&self, text: &str) -> String {
        let head = truncate(text, 4096);
        let mut tail_start = text.len() - 1024;
        while !text.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        let saved = match tools::spill(&self.ctx, "result", text) {
            Some(path) => format!(
                "The full result is saved at {}; read it with offset and limit.",
                path.display()
            ),
            None => "The full result could not be saved.".into(),
        };
        format!(
            "{head}\n\n[… {} characters omitted. {saved}]\n\n{}",
            text[head.len()..tail_start].chars().count(),
            &text[tail_start..]
        )
    }

    /// Tool-call IDs limited to `[A-Za-z0-9_-]` and 64 characters, unique in the session.
    pub(super) fn unique_id(&mut self, raw: &str) -> String {
        let mut id: String = raw
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .take(64)
            .collect();
        if id.is_empty() {
            id = format!("call_{}", self.ids.len() + 1);
        }
        let base = id.chars().take(58).collect::<String>();
        let mut n = 2;
        while !self.ids.insert(id.clone()) {
            id = format!("{base}__{n}");
            n += 1;
        }
        id
    }

    pub(super) fn push(&mut self, message: Value) -> Result<()> {
        self.push_meta(message, None)
    }

    /// Appends a message. The log keeps `meta` beside a tool result, outside the message itself:
    /// how it ended and how long it took, which a resumed session shows again. A user message
    /// pushed while the last reply's calls still await results waits until the last result is
    /// recorded: between a call and its result it reads as a missing result to providers.
    pub(super) fn push_meta(&mut self, message: Value, meta: Option<Value>) -> Result<()> {
        if message["role"] == "user" && !unanswered(&self.messages).is_empty() {
            self.deferred_messages.push((message, meta));
            return Ok(());
        }
        let tool = message["role"] == "tool";
        self.append_message(message, meta)?;
        if tool && !self.deferred_messages.is_empty() && unanswered(&self.messages).is_empty() {
            for (message, meta) in std::mem::take(&mut self.deferred_messages) {
                self.append_message(message, meta)?;
            }
        }
        Ok(())
    }

    fn append_message(&mut self, message: Value, meta: Option<Value>) -> Result<()> {
        let mut entry = json!({"type": "message", "message": message});
        if let Some(meta) = meta {
            entry["tool"] = meta;
        }
        self.session.record(&entry)?;
        Rc::make_mut(&mut self.messages).push(message);
        self.share();
        Ok(())
    }
}
