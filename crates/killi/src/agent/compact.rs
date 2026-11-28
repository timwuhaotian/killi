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

/// The newest steps kept word for word through a compaction, in estimated tokens (about
/// 80,000 characters): the results the model was just working from. In the logs before this,
/// 43% of the calls after a compaction went to files it had already read.
const KEPT_TAIL_TOKENS: u64 = 40_000;

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
        self.compaction_pressure()
            .is_some_and(|(context, suffix, limit, reserve)| {
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
    pub(super) fn compaction_pressure(&self) -> Option<(u64, u64, u64, u64)> {
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
        let suffix =
            estimate_tokens(&self.messages[self.accounted_messages.min(self.messages.len())..]);
        let reserve = self
            .client
            .model
            .max_output
            .unwrap_or(DEFAULT_OUTPUT_RESERVE)
            .min(limit / 4);
        Some((self.stats.context, suffix, limit, reserve))
    }

    /// Replaces the conversation with the user's prompts, word for word, and a summary of
    /// the work. The only time earlier messages change.
    pub async fn compact(&mut self, focus: Option<&str>) -> Result<()> {
        self.compact_with(focus, CompactMode::Full).await
    }

    pub(super) async fn compact_with(
        &mut self,
        focus: Option<&str>,
        mode: CompactMode,
    ) -> Result<()> {
        (self.sink)(Event::Notice("Compacting the conversation…".into()));
        // Taken before the summary request accounts its own usage over these numbers.
        let pressure = self.compaction_pressure().unwrap_or_default();
        self.summarize(focus, mode, pressure).await
    }

    /// Asks the model for a summary and replaces the conversation with it, as `compact_with`
    /// describes; `pressure` is what the log keeps as the compaction's cause.
    async fn summarize(
        &mut self,
        focus: Option<&str>,
        mode: CompactMode,
        (context, suffix, limit, reserve): (u64, u64, u64, u64),
    ) -> Result<()> {
        let mut request = match mode {
            CompactMode::Full => self.messages.to_vec(),
            CompactMode::Reduced => reduced(&self.messages),
        };
        let mut instruction = COMPACT_INSTRUCTION.to_string();
        if let Some(focus) = focus.filter(|f| !f.trim().is_empty()) {
            instruction.push_str(&format!(
                "\nThe user asked the summary to focus on: {focus}"
            ));
        }
        let instruction = json!({"role": "user", "content": instruction});
        request.push(instruction.clone());
        // The summary goes through the backup like any other request, its retries shown and
        // logged; only its text isn't streamed, since it isn't a reply to the user.
        let mut unsettled = super::request::Unsettled::new(&self.session, Some("compaction"));
        let reply = match self
            .complete_with_fallback(&request, false, &mut unsettled)
            .await
        {
            // Too long to be read even to summarize it: summarize it with its bulk cut.
            Err(error) if mode == CompactMode::Full && is_overflow(&error) => {
                request = reduced(&self.messages);
                request.push(instruction);
                self.complete_with_fallback(&request, false, &mut unsettled)
                    .await
            }
            reply => reply,
        };
        self.settle(&mut unsettled)?;
        let reply = reply?;
        self.stats.requests += 1;
        self.account(
            &reply.usage,
            None,
            None,
            None,
            reply.reasoning_kept(),
            reasoning_evidence(&reply),
        )?;
        if reply.text.trim().is_empty() {
            let instead = if reply.tool_calls.is_empty() {
                match reply.reasoning.is_empty() {
                    true => "returned an empty summary".to_string(),
                    false => format!(
                        "only reasoned, {} characters, and wrote no summary",
                        reply.reasoning.chars().count()
                    ),
                }
            } else {
                let names: Vec<&str> = reply
                    .tool_calls
                    .iter()
                    .map(|call| call.name.as_str())
                    .collect();
                format!("called {} instead of writing a summary", names.join(", "))
            };
            self.session.record(
                &json!({"type": "warn", "message": format!("Compaction: the model {instead}.")}),
            )?;
            bail!("the model {instead}, so the conversation was left as it was");
        }
        // The steps kept as they were follow the summary; a request typed inside them stays
        // there, so it is not listed with the earlier ones as well.
        let tail_from = tail_start(&self.messages, self.tail_budget(limit));
        let (old, tail) = self
            .messages
            .split_at(tail_from.unwrap_or(self.messages.len()));
        let mut kept = Vec::new();
        let mut size = 0;
        for prompt in crate::session::user_prompts(old).into_iter().rev() {
            size += prompt.chars().count().min(KEPT_PROMPT_CHARS);
            if size > KEPT_PROMPTS_CHARS && !kept.is_empty() {
                break;
            }
            kept.push(prompt);
        }
        kept.reverse();
        let requests: String = kept
            .iter()
            .map(|prompt| {
                // What a cut takes is the user's own words: the whole request stays readable.
                let saved = cut_at(prompt).and_then(|_| tools::spill(&self.ctx, "request", prompt));
                format!(
                    "<request>\n{}\n</request>\n",
                    kept_prompt(prompt, saved.as_deref())
                )
            })
            .collect();
        let goes_on = if tail.is_empty() {
            "Continue from where it left off; don't redo finished work."
        } else {
            "The last steps before the compaction follow this message as they were: continue from where they leave off, and don't redo finished work."
        };
        let summary = format!(
            "{mark} to save space. Below are the user's requests so far, word for word, and a summary of the work. {goes_on}</system-reminder>\n\n<user-requests>\n{requests}</user-requests>\n\n<summary>\n{}\n</summary>",
            reply.text.trim(),
            mark = crate::session::COMPACTED_MARK,
        );
        let kept_chars = summary.chars().count();
        let kept_messages = tail.len();
        let mut messages = vec![
            self.messages[0].clone(),
            json!({"role": "user", "content": summary}),
        ];
        messages.extend(tail.iter().map(without_bound_thinking));
        self.session.record(&json!({
            "type": "compaction",
            "cause": {"context": context, "suffix": suffix, "limit": limit, "reserve": reserve, "kept_chars": kept_chars, "kept_messages": kept_messages},
            "messages": messages,
            // The calls that set the task list are gone from `messages`: a resume reads it here.
            "todos": self.todos,
        }))?;
        self.messages = Rc::new(messages);
        // The reminders for plan, swarm and reply mode, the goal and the other sessions went with
        // the old messages: the next step tells the model again what is still on.
        self.plan_announced = false;
        self.swarm_announced = false;
        self.swarm_cap.set(0);
        self.concise_announced = false;
        self.orchestrate_announced = false;
        self.goal_announced = None;
        self.model_announced = None;
        self.peers_announced.clear();
        // The repeat breaker refuses a call because the model already has its result, and a
        // nested instruction file is named once because the reminder stays in view: neither
        // holds once the messages are gone.
        self.repeats.clear();
        self.near_repeats.clear();
        self.instructions.forget_told();
        self.share();
        self.after_compaction(limit);
        if let Some(note) = self.held_state_note() {
            self.push(json!({"role": "user", "content": note}))?;
        }
        // A child's step never announces plan mode, so a child in it hears it again here.
        if self.depth > 0 && self.controls.plan.get() {
            self.push(json!({"role": "user", "content": plan::ON}))?;
            self.plan_announced = true;
        }
        // The pair's executor may delegate; every other child was told it can't.
        if self.depth > 0 && !self.pair_exec {
            self.push(json!({"role": "user", "content": super::subagent::CHILD_REMINDER}))?;
        }
        (self.sink)(Event::Notice("Compacted.".into()));
        Ok(())
    }

    /// How much of the newest history a compaction keeps word for word, in estimated tokens:
    /// `KEPT_TAIL_TOKENS`, at most a tenth of `limit` (0: compaction is off, so no such bound)
    /// and at most a quarter of the history, so a compaction always shrinks it.
    fn tail_budget(&self, limit: u64) -> u64 {
        let whole = estimate_tokens(self.messages.get(1..).unwrap_or_default());
        let cap = match limit {
            0 => KEPT_TAIL_TOKENS,
            limit => KEPT_TAIL_TOKENS.min(limit / 10),
        };
        cap.min(whole / 4)
    }

    /// Where a fresh compaction leaves the accounting: the next request starts the count over,
    /// and the size the history can't compact below is remembered, so proactive compaction
    /// waits for real growth instead of running again before every step. When that size alone
    /// is over `limit` (0: compaction is off), one notice says it can't shrink further. The
    /// turn's prompt is in the summary now, so it can no longer be taken back.
    pub(super) fn after_compaction(&mut self, limit: u64) {
        self.pending_prompt = None;
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
}

/// Where the steps a compaction keeps word for word begin: the earliest assistant message
/// from which the rest of the history fits `budget` estimated tokens, so every kept call has
/// its result and the summary is followed by a reply, as providers expect. None when not even
/// the last step fits, or a call is still unanswered.
pub(super) fn tail_start(messages: &[Value], budget: u64) -> Option<usize> {
    if !unanswered(messages).is_empty() {
        return None;
    }
    let mut size = 0_u64;
    let mut start = None;
    for (index, message) in messages.iter().enumerate().skip(1).rev() {
        size = size.saturating_add(estimate_tokens(std::slice::from_ref(message)));
        if size > budget {
            break;
        }
        if message["role"] == "assistant" {
            start = Some(index);
        }
    }
    start
}

/// A kept message as it follows the summary: whole, but without its signed thinking blocks.
/// A signature binds a block to the history that produced it, which the compaction just
/// replaced, and an API that checks it rejects the request; Anthropic's guidance for a
/// compaction that keeps its tail is to drop them and keep the text and the calls. Reasoning
/// in the other dialects is not bound to what came before it and stays as stored.
fn without_bound_thinking(message: &Value) -> Value {
    let mut message = message.clone();
    let Some(object) = message.as_object_mut() else {
        return message;
    };
    let emptied = object
        .get_mut("reasoning_items")
        .and_then(Value::as_array_mut)
        .is_some_and(|items| {
            items.retain(|item| {
                !matches!(
                    item["type"].as_str(),
                    Some("thinking" | "redacted_thinking")
                )
            });
            items.is_empty()
        });
    if emptied {
        object.remove("reasoning_items");
    }
    message
}

/// How the note that ends a cut request starts.
const CUT_NOTE: &str = "\n[… ";

/// Where a kept request is cut: None when it fits, and when an earlier compaction already cut
/// it. Its last line is then that cut's note, which a second cut would replace with a count of
/// the note's own characters.
pub(super) fn cut_at(prompt: &str) -> Option<usize> {
    let (end, _) = prompt.char_indices().nth(KEPT_PROMPT_CHARS)?;
    let rest = &prompt[end..];
    let noted = rest.starts_with(CUT_NOTE) && rest.ends_with(']') && !rest[1..].contains('\n');
    (!noted).then_some(end)
}

/// A request as a compaction keeps it: whole, or cut with a note of how much was cut and,
/// when the whole text was saved, the file that holds it.
pub(super) fn kept_prompt(prompt: &str, saved: Option<&Path>) -> String {
    let Some(end) = cut_at(prompt) else {
        return prompt.to_string();
    };
    let saved = saved.map_or_else(String::new, |path| {
        format!("; the whole request is saved at {}", path.display())
    });
    format!(
        "{}{CUT_NOTE}{} characters cut{saved}]",
        &prompt[..end],
        prompt[end..].chars().count()
    )
}

/// A conservative character-based estimate for history appended since the last usage report.
/// The billed fixed prefix stays in `stats.context`; this estimates only the new suffix. An
/// inline image counts as `IMAGE_TOKEN_RESERVE`, not its base64: a 300 KB screenshot bills
/// about 1.5k tokens, and counted as text it compacted conversations of 8k tokens.
pub(super) fn estimate_tokens(messages: &[Value]) -> u64 {
    messages.iter().fold(0_u64, |total, message| {
        total
            .saturating_add(estimate_chars(message).div_ceil(2))
            .saturating_add(4)
    })
}

/// Characters of `value` serialized, with each inline image's data replaced by its reserve.
fn estimate_chars(value: &Value) -> u64 {
    match value {
        Value::String(text) if text.starts_with("data:") && text.contains(";base64,") => {
            IMAGE_TOKEN_RESERVE * 2
        }
        Value::Array(values) => values.iter().fold(2, |total, value| {
            total
                .saturating_add(estimate_chars(value))
                .saturating_add(1)
        }),
        Value::Object(values) => values.iter().fold(2, |total, (key, value)| {
            total
                .saturating_add(key.chars().count() as u64)
                .saturating_add(4)
                .saturating_add(estimate_chars(value))
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
                if let Some(text) = message["content"]
                    .as_str()
                    .filter(|t| t.len() > REDUCED_RESULT_CHARS)
                {
                    message["content"] = format!(
                        "{}\n[… cut to fit the context window]",
                        truncate(text, REDUCED_RESULT_CHARS)
                    )
                    .into();
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
                    text.starts_with(crate::session::COMPACTED_MARK)
                        || text.contains("<system-reminder>")
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
                    + message
                        .get("reasoning_items")
                        .map_or(0, |value| value.to_string().len());
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
