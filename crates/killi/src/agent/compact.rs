use super::*;

/// Where compaction aims as a share of the model's context window when `compact_at` is unset.
const COMPACT_AT_PERCENT: u64 = 60;

/// The compaction limit for a model whose context window is unknown.
pub(super) const DEFAULT_COMPACT_AT: u64 = 256_000;
/// User prompts kept word for word through a compaction, in characters (about 20k tokens).
const KEPT_PROMPTS_CHARS: usize = 80_000;

/// One kept user request, whole when it fits, cut when it embeds a whole skill or document:
/// the words are the user's, but a pasted body is reloadable and its bulk would floor every
/// later summary at a size no compaction can shrink.
const KEPT_PROMPT_CHARS: usize = 4_000;

pub(super) const DEFAULT_OUTPUT_RESERVE: u64 = 4_096;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum CompactMode {
    Full,
    Reduced,
}

/// Characters of a tool result kept in a conversation cut down to be summarized.
pub(super) const REDUCED_RESULT_CHARS: usize = 2_000;
pub(super) const REDUCED_TEXT_CHARS: usize = 4_000;
const REDUCED_REASONING_CHARS: usize = 1_000;
const REDUCED_RECENT_MESSAGES: usize = 8;

impl Agent {
    pub(super) fn compaction_due(&self) -> bool {
        self.compaction_pressure().is_some_and(|(context, suffix, limit, reserve)| {
            let size = context.saturating_add(suffix);
            // Compacting again is worth it only once the conversation has grown by a fifth of
            // the limit since the last compaction: a summary that left the history at that
            // size would otherwise compact before every step, each time to the same result.
            size > limit - reserve && size >= self.compact_floor + limit / 5
        })
    }

    /// Why compaction is or isn't due: billed context and estimated suffix against the limit
    /// minus room for one reply (capped at a quarter of the limit, since the output cap already
    /// fits each request into the window). The log keeps these numbers at each compaction so a
    /// session that compacts too often shows which part forced it. `None` at limit 0 (off).
    fn compaction_pressure(&self) -> Option<(u64, u64, u64, u64)> {
        // A share of the window by default, an explicit `compact_at` instead, and never above
        // 85%, where the reply and the framing overhead start to squeeze the request.
        let limit = match self.client.model.context_window {
            Some(window) => self
                .settings
                .compact_at
                .unwrap_or(window * COMPACT_AT_PERCENT / 100)
                .min(window * 85 / 100),
            None => self.settings.compact_at.unwrap_or(DEFAULT_COMPACT_AT),
        };
        if limit == 0 {
            return None;
        }
        let suffix = estimate_tokens(&self.messages[self.accounted_messages.min(self.messages.len())..]);
        let reserve = self.client.model.max_output.unwrap_or(DEFAULT_OUTPUT_RESERVE).min(limit / 4);
        Some((self.stats.context, suffix, limit, reserve))
    }

    /// Replaces the conversation with the user's prompts, word for word, and a summary of
    /// the work. The only time earlier messages change.
    pub async fn compact(&mut self, focus: Option<&str>) -> Result<()> {
        self.compact_with(focus, CompactMode::Full).await
    }

    pub(super) async fn compact_with(&mut self, focus: Option<&str>, mode: CompactMode) -> Result<()> {
        (self.sink)(Event::Notice("Compacting the conversation…".into()));
        // Taken before the summary request accounts its own usage over these numbers.
        let (context, suffix, limit, reserve) = self.compaction_pressure().unwrap_or_default();
        let mut request = match mode {
            CompactMode::Full => self.messages.to_vec(),
            CompactMode::Reduced => reduced(&self.messages),
        };
        let mut instruction = COMPACT_INSTRUCTION.to_string();
        if let Some(focus) = focus.filter(|f| !f.trim().is_empty()) {
            instruction.push_str(&format!("\nThe user asked the summary to focus on: {focus}"));
        }
        let instruction = json!({"role": "user", "content": instruction});
        request.push(instruction.clone());
        let reply = match self.client.complete(&request, &self.tools, &self.session.id, &mut |_| {}).await {
            // Too long to be read even to summarize it: summarize it with its bulk cut.
            Err(error) if mode == CompactMode::Full && is_overflow(&error) => {
                request = reduced(&self.messages);
                request.push(instruction);
                self.client.complete(&request, &self.tools, &self.session.id, &mut |_| {}).await?
            }
            reply => reply?,
        };
        self.stats.requests += 1;
        self.account(&reply.usage, None, None, reply.reasoning_kept(), reasoning_evidence(&reply))?;
        if reply.text.trim().is_empty() {
            let instead = if reply.tool_calls.is_empty() {
                match reply.reasoning.is_empty() {
                    true => "returned an empty summary".to_string(),
                    false => format!("only reasoned, {} characters, and wrote no summary", reply.reasoning.chars().count()),
                }
            } else {
                let names: Vec<&str> = reply.tool_calls.iter().map(|call| call.name.as_str()).collect();
                format!("called {} instead of writing a summary", names.join(", "))
            };
            self.session.record(&json!({"type": "warn", "message": format!("Compaction: the model {instead}.")}))?;
            bail!("the model {instead}, so the conversation was left as it was");
        }
        let mut kept = Vec::new();
        let mut size = 0;
        for prompt in self.user_prompts().into_iter().rev() {
            size += prompt.chars().count().min(KEPT_PROMPT_CHARS);
            if size > KEPT_PROMPTS_CHARS && !kept.is_empty() {
                break;
            }
            kept.push(prompt);
        }
        kept.reverse();
        let requests: String = kept.iter().map(|p| format!("<request>\n{}\n</request>\n", kept_prompt(p))).collect();
        let summary = format!(
            "{mark} to save space. Below are the user's requests so far, word for word, and a summary of the work. Continue from where it left off; don't redo finished work.</system-reminder>\n\n<user-requests>\n{requests}</user-requests>\n\n<summary>\n{}\n</summary>",
            reply.text.trim(),
            mark = crate::session::COMPACTED_MARK,
        );
        self.messages = Rc::new(vec![self.messages[0].clone(), json!({"role": "user", "content": summary})]);
        // The reminders for plan mode, swarm mode and the other sessions went with the old
        // messages: the next step tells the model again what is still on.
        self.plan_announced = false;
        self.swarm_announced = false;
        self.peers_announced.clear();
        self.share();
        self.session.record(&json!({
            "type": "compaction",
            "cause": {"context": context, "suffix": suffix, "limit": limit, "reserve": reserve, "kept_chars": summary.chars().count()},
            "messages": *self.messages,
        }))?;
        self.after_compaction(limit);
        (self.sink)(Event::Notice("Compacted.".into()));
        Ok(())
    }

    /// Where a fresh compaction leaves the accounting: the next request starts the count over,
    /// and the size the history can't compact below is remembered, so proactive compaction
    /// waits for real growth instead of running again before every step. When that size alone
    /// is over `limit` (0: compaction is off), one notice says it can't shrink further.
    pub(super) fn after_compaction(&mut self, limit: u64) {
        self.stats.context = 0;
        self.accounted_messages = self.messages.len();
        self.compact_floor = estimate_tokens(&self.messages);
        if limit > 0 && self.compact_floor > limit {
            let floor = self.compact_floor;
            (self.sink)(Event::Notice(format!(
                "Compacted, but the system prompt and kept requests alone exceed compact_at (about {floor} of {limit} tokens), so it can't shrink the conversation further."
            )));
        }
    }

    /// After an interrupted turn, answers every tool call left without a result so the
    /// history stays valid, and notes the interruption for the next prompt.
    pub fn recover(&mut self) -> Result<()> {
        let missing = unanswered(&self.messages);
        let partial = self.partial_swarm.take();
        for id in &missing {
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
                    neutralize_reminder_tags(&swarm::report(&self.ctx, &partial.items, &outputs, MAX_RESULT_BYTES).text).into_owned()
                }
                _ => "Interrupted by the user before this finished.".to_string(),
            };
            self.push(json!({"role": "tool", "tool_call_id": id, "content": content}))?;
        }
        if !missing.is_empty()
            || self.messages.last().is_some_and(|m| m["role"] == "user" || m["role"] == "tool")
        {
            self.interrupted = true;
        }
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

    /// Keeps the first 4096 and last 1024 characters and saves the whole result to a file.
    pub(super) fn shorten(&self, text: &str) -> String {
        let head = truncate(text, 4096);
        let mut tail_start = text.len() - 1024;
        while !text.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        let saved = match tools::spill(&self.ctx, "result", text) {
            Some(path) => format!("The full result is saved at {}; read it with offset and limit.", path.display()),
            None => "The full result could not be saved.".into(),
        };
        format!("{head}\n\n[… {} characters omitted. {saved}]\n\n{}", tail_start - head.len(), &text[tail_start..])
    }

    /// Tool-call IDs limited to `[A-Za-z0-9_-]` and 64 characters, unique in the session.
    pub(super) fn unique_id(&mut self, raw: &str) -> String {
        let mut id: String =
            raw.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).take(64).collect();
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
    /// how it ended and how long it took, which a resumed session shows again.
    pub(super) fn push_meta(&mut self, message: Value, meta: Option<Value>) -> Result<()> {
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

pub(super) fn kept_prompt(prompt: &str) -> String {
    let chars = prompt.chars().count();
    if chars <= KEPT_PROMPT_CHARS {
        return prompt.to_string();
    }
    format!("{}\n[… {} characters cut]", truncate(prompt, KEPT_PROMPT_CHARS), chars - KEPT_PROMPT_CHARS)
}

/// A conservative character-based estimate for history appended since the last usage report.
/// The billed fixed prefix stays in `stats.context`; this estimates only the new suffix. An
/// inline image counts as `IMAGE_TOKEN_RESERVE`, not its base64: a 300 KB screenshot bills
/// about 1.5k tokens, and counted as text it compacted conversations of 8k tokens.
pub(super) fn estimate_tokens(messages: &[Value]) -> u64 {
    messages.iter().fold(0_u64, |total, message| {
        total.saturating_add(estimate_chars(message).div_ceil(2)).saturating_add(4)
    })
}

/// Characters of `value` serialized, with each inline image's data replaced by its reserve.
fn estimate_chars(value: &Value) -> u64 {
    match value {
        Value::String(text) if text.starts_with("data:") && text.contains(";base64,") => IMAGE_TOKEN_RESERVE * 2,
        Value::Array(values) => values.iter().fold(2, |total, value| total.saturating_add(estimate_chars(value)).saturating_add(1)),
        Value::Object(values) => values.iter().fold(2, |total, (key, value)| {
            total.saturating_add(key.chars().count() as u64).saturating_add(4).saturating_add(estimate_chars(value))
        }),
        other => other.to_string().chars().count() as u64,
    }
}

/// `messages` with their bulk cut for a reduced summary request. The system prompt and recent
/// text stay verbatim; old prose keeps both ends, and old signed reasoning is dropped rather
/// than edited. This reduces the request conservatively, without promising a token-level fit.
pub(super) fn reduced(messages: &[Value]) -> Vec<Value> {
    let recent_from = messages.len().saturating_sub(REDUCED_RECENT_MESSAGES);
    messages
        .iter()
        .enumerate()
        .map(|(index, original)| {
            let mut message = original.clone();
            let role = message["role"].as_str().unwrap_or_default().to_string();
            if role == "system" {
                return message;
            }
            if role == "tool" {
                if let Some(text) = message["content"].as_str().filter(|t| t.len() > REDUCED_RESULT_CHARS) {
                    message["content"] = format!("{}\n[… cut to fit the context window]", truncate(text, REDUCED_RESULT_CHARS)).into();
                }
                return message;
            }
            if let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) {
                parts.retain(|part| part["type"] != "image_url");
            }
            if index >= recent_from {
                return message;
            }
            let protected_user = role == "user"
                && message["content"].as_str().is_some_and(|text| {
                    text.starts_with(crate::session::COMPACTED_MARK) || text.contains("<system-reminder>")
                });
            if matches!(role.as_str(), "assistant" | "user") && !protected_user {
                match message.get_mut("content") {
                    Some(Value::String(text)) => reduce_text(text, REDUCED_TEXT_CHARS),
                    Some(Value::Array(parts)) => {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                let text = text.to_string();
                                part["text"] = cut_middle(&text, REDUCED_TEXT_CHARS).into();
                            }
                        }
                    }
                    _ => {}
                }
            }
            if role == "assistant" {
                // Reasoning is replayed verbatim. An oversized old reasoning bundle is dropped
                // as one bundle, never truncated or left inconsistent with its signed items.
                let reasoning_size = crate::llm::REASONING_KEYS
                    .iter()
                    .map(|key| message.get(*key).map_or(0, |value| value.to_string().len()))
                    .sum::<usize>()
                    + message.get("reasoning_items").map_or(0, |value| value.to_string().len());
                if reasoning_size > REDUCED_REASONING_CHARS {
                    let object = message.as_object_mut().expect("a message is an object");
                    for key in crate::llm::REASONING_KEYS {
                        object.remove(key);
                    }
                    object.remove("reasoning_items");
                }
            }
            message
        })
        .collect()
}

fn reduce_text(text: &mut String, limit: usize) {
    if text.len() > limit {
        *text = cut_middle(text, limit);
    }
}

/// Cuts the middle so both the request and constraints often placed at the end survive.
fn cut_middle(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    const MARK: &str = "\n[… middle cut for reduced compaction …]\n";
    let budget = limit.saturating_sub(MARK.len());
    let head = truncate(text, budget * 3 / 4);
    let tail_len = budget - head.len();
    let mut start = text.len() - tail_len;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("{head}{MARK}{}", &text[start..])
}

pub(super) const COMPACT_INSTRUCTION: &str = "<system-reminder>The conversation is about to be compacted to save space. Write a summary that lets you continue the work without the earlier messages. Don't call tools. Use these sections:
## Goal
What the user wants, with their constraints and preferences.
## Progress
What is done and what is in progress.
## Decisions
Choices made and why.
## Files
Files read or changed, and what changed in each.
## Next steps
Exactly what to do next.
Be specific: names, paths, commands, error messages. A request shown as `/name …` ran that skill, whose text isn't kept: load it again with the `skill` tool if the work still needs it. Keep the whole summary under about 1,500 tokens: the newest state matters more than the history, and command transcripts don't belong in it. Write in the user's language.</system-reminder>";
