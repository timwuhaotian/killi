use super::*;

impl Agent {
    pub(super) async fn request(&mut self) -> Result<Reply> {
        let sink = self.sink.clone();
        let mut on_delta = |delta: Delta| {
            sink(match delta {
                Delta::Text(text) => Event::Text(text.to_string()),
                Delta::Reasoning(text) => Event::Reasoning(text.to_string()),
                Delta::Retry {
                    attempt,
                    wait,
                    reason,
                } => Event::Retry {
                    attempt,
                    wait,
                    reason: reason.to_string(),
                },
            })
        };
        let started = Instant::now();
        let reply = match self
            .client
            .complete(&self.messages, &self.tools, &self.session.id, &mut on_delta)
            .await
        {
            Ok(reply) => reply,
            // The primary spent its retries on a failure that is about it, not the request:
            // this session switches to the backup it was given and asks it once. The switch is
            // the switch `switch_model` makes, so the backup becomes the model the session runs
            // on and the log notes it.
            Err(error) => match Self::fallback_reason(&error)
                .and_then(|reason| Some((reason, self.backup.take()?)))
            {
                Some((reason, backup)) => {
                    let from = self.client.model.alias.clone();
                    self.switch_client((*backup).clone())?;
                    // A subagent that falls back keeps the session on its own model, so it
                    // says what it did itself.
                    let note = if self.depth == 0 {
                        format!(
                            "{from} {reason}, so this session switched to its backup {}.",
                            self.client.model.alias
                        )
                    } else {
                        format!(
                            "{from} {reason}, so this subagent switched to the backup {}.",
                            self.client.model.alias
                        )
                    };
                    (self.sink)(Event::Notice(note));
                    match self
                        .client
                        .complete(&self.messages, &self.tools, &self.session.id, &mut on_delta)
                        .await
                    {
                        Ok(reply) => reply,
                        Err(error) => {
                            self.session.record(&json!({"type": "error", "message": format!("{error:#}"), "ms": millis(started.elapsed())}))?;
                            return Err(error);
                        }
                    }
                }
                None => {
                    self.session.record(&json!({"type": "error", "message": format!("{error:#}"), "ms": millis(started.elapsed())}))?;
                    return Err(error);
                }
            },
        };
        let took = started.elapsed();
        self.stats.requests += 1;
        self.account(
            &reply.usage,
            Some(&reply.finish_reason),
            Some(took),
            reply.reasoning_kept(),
            reasoning_evidence(&reply),
        )?;
        self.note_hidden_reasoning(&reply)?;
        Ok(reply)
    }

    /// The provider rejected the conversation's media: replace every image part with a
    /// placeholder and retry once, recording the rewrite as a compaction-kind entry so a
    /// resumed session replays the stripped conversation.
    pub(super) fn strip_images(&mut self, error: &anyhow::Error) -> Result<()> {
        self.pending_prompt = None;
        let messages: &mut Vec<Value> = Rc::make_mut(&mut self.messages);
        let stripped = strip_images_from(messages);
        self.share();
        self.session.record(&json!({
            "type": "compaction",
            "cause": {"media_rejected": format!("{error:#}"), "stripped": stripped},
            "messages": *self.messages,
        }))?;
        (self.sink)(Event::Notice(format!(
            "The provider rejected the conversation's images, so {stripped} {} replaced with a placeholder and the request was retried.",
            if stripped == 1 {
                "image was"
            } else {
                "images were"
            },
        )));
        Ok(())
    }

    /// Some providers count reasoning tokens but don't send the reasoning on their chat API, so
    /// it can't be kept or replayed and the model starts each step without its own thinking.
    /// The log says so once, to explain a model that forgets what it just did.
    pub(super) fn note_hidden_reasoning(&mut self, reply: &Reply) -> Result<()> {
        if hides_reasoning(reply) && !std::mem::replace(&mut self.reasoning_noted, true) {
            let counted = reply.usage.reasoning;
            let advice = " If it offers the Responses API, set `type = \"openai_responses\"` on its provider in config.toml.";
            let why = match reply.reasoning_key {
                Some(key) => format!(
                    "The provider counted {counted} reasoning tokens but its `{key}` field was empty, so there was nothing to keep."
                ),
                None if reply.unread_fields.is_empty() => {
                    format!(
                        "The provider counted {counted} reasoning tokens but sent no reasoning, so it can't be kept.{advice}"
                    )
                }
                None => {
                    let fields = reply
                        .unread_fields
                        .iter()
                        .map(|f| format!("`{f}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "The provider counted {counted} reasoning tokens but sent none of it back, and the stream carried fields killi doesn't read: {fields}.{advice}"
                    )
                }
            };
            self.session.record(&json!({
                "type": "warn",
                "message": why,
            }))?;
        }
        Ok(())
    }

    /// `reasoning_kept` says whether this reply's reasoning came back in a form the log holds;
    /// billed but not kept is the dialect-agnostic mark of a provider hiding it on this API.
    /// `reasoning_seen` carries the evidence: which field the stream used and which fields it
    /// sent that no adapter reads, beside the message (history stays byte-identical).
    pub(super) fn account(
        &mut self,
        usage: &Usage,
        finish_reason: Option<&Option<String>>,
        took: Option<Duration>,
        reasoning_kept: bool,
        reasoning_seen: Option<Value>,
    ) -> Result<()> {
        self.stats.first_prompt.get_or_insert(usage.prompt);
        self.stats.context = usage.prompt;
        self.accounted_messages = self.messages.len();
        let total = &mut self.stats.usage;
        total.prompt += usage.prompt;
        total.cached += usage.cached;
        total.completion += usage.completion;
        total.reasoning += usage.reasoning;
        (self.sink)(Event::Usage(usage.clone()));
        let mut record = json!({
            "type": "usage",
            "model": self.client.model.alias,
            "prompt": usage.prompt,
            "cached": usage.cached,
            "completion": usage.completion,
            "reasoning": usage.reasoning,
            "reasoning_kept": reasoning_kept,
            "finish_reason": finish_reason.cloned().flatten(),
            "ms": took.map(millis),
        });
        if let Some(seen) = reasoning_seen {
            record["reasoning_seen"] = seen;
        }
        self.session.record(&record)
    }

    /// Appends the assistant message and returns its tool calls under their final IDs.
    pub(super) fn record_reply(&mut self, reply: &Reply) -> Result<Vec<(String, ToolCall)>> {
        self.pending_prompt = None;
        let calls: Vec<_> = reply
            .tool_calls
            .iter()
            .map(|call| (self.unique_id(&call.id), call.clone()))
            .collect();
        let mut message = json!({"role": "assistant"});
        if !reply.text.is_empty() || calls.is_empty() {
            message["content"] = reply.text.clone().into();
        }
        if let Some(key) = reply.reasoning_key.filter(|_| !reply.reasoning.is_empty()) {
            message[key] = reply.reasoning.clone().into();
        }
        if !reply.reasoning_items.is_empty() {
            message["reasoning_items"] = reply.reasoning_items.clone().into();
        }
        if !calls.is_empty() {
            message["tool_calls"] = calls
                .iter()
                .map(|(id, call)| {
                    json!({"id": id, "type": "function", "function": {"name": call.name, "arguments": call.arguments}})
                })
                .collect();
        }
        self.current_reply = self.messages.len();
        self.push(message)?;
        Ok(calls)
    }
}

/// Whether a reply was billed for reasoning that never came back, on either channel.
pub(super) fn hides_reasoning(reply: &Reply) -> bool {
    reply.usage.reasoning > 0 && !reply.reasoning_kept()
}

/// What the stream carried on a reply whose reasoning was billed but not kept, or that sent
/// fields no adapter reads: the field the provider used for reasoning (present but empty) and
/// the unread field names. `None` when there is nothing to say.
pub(super) fn reasoning_evidence(reply: &Reply) -> Option<Value> {
    if !hides_reasoning(reply) && reply.unread_fields.is_empty() {
        return None;
    }
    Some(json!({
        "field": reply.reasoning_key,
        "unread": reply.unread_fields,
    }))
}

/// Whether a provider error says the conversation no longer fits the context window. A
/// typed failure decides it; only an untyped error still falls back to matching words.
pub(super) fn is_overflow(error: &anyhow::Error) -> bool {
    if let Some(provider) = error.downcast_ref::<ProviderError>() {
        return provider.kind == ErrorKind::Overflow;
    }
    let text = format!("{error:#}").to_ascii_lowercase();
    [
        "context length",
        "context_length",
        "maximum context",
        "context window",
        "prompt is too long",
        "too many tokens",
        "input is too long",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// Whether the provider rejected an image or other media in the conversation.
pub(super) fn is_media(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ProviderError>()
        .is_some_and(|provider| provider.kind == ErrorKind::Media)
}

/// Replaces every image part in history with a placeholder, so the request the provider
/// just rejected for its media can be retried without it. Recorded as a compaction-kind
/// entry, the one history rewrite besides compaction the log understands, so a resumed
/// session replays the stripped conversation.
fn strip_images_from(messages: &mut [Value]) -> usize {
    let mut stripped = 0;
    for message in messages.iter_mut() {
        let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in parts.iter_mut() {
            if part["type"] == "image_url" {
                *part = json!({"type": "text", "text": IMAGE_OMITTED});
                stripped += 1;
            }
        }
    }
    stripped
}
