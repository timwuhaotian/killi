use super::*;

pub(super) const REPEAT_WARNINGS: [u32; 3] = [3, 5, 7];
pub(super) const REPEAT_STOP: u32 = 8;

/// The result of a call that didn't run because the user sent a message before its turn.
pub(super) const SKIPPED_FOR_MESSAGE: &str = "Not run: the user sent a message first, which follows. Run it again if it is still needed.";

impl Agent {
    /// Runs a reply's tool calls in order; consecutive read-only file/search calls and `agent`
    /// calls run in parallel, and an `agent_swarm` call runs its subagents in parallel. Tool hooks
    /// keep every call serial. A message typed meanwhile waits for the calls running now, not
    /// for the ones after them: those are answered as not run.
    /// Returns true when the repeat breaker trips.
    pub(super) async fn execute_calls(&mut self, calls: Vec<(String, ToolCall)>, truncated: bool) -> Result<bool> {
        if let Some(wait) = self.snapshot_wait.take() {
            let _ = wait.await;
        }
        let mut stop = false;
        let mut images = Vec::new();
        let mut i = 0;
        while i < calls.len() {
            if self.pending_steer() {
                for (id, _) in &calls[i..] {
                    self.push(json!({"role": "tool", "tool_call_id": id, "content": SKIPPED_FOR_MESSAGE}))?;
                }
                break;
            }
            if !self.settings.hooks.has_tool_hooks() && parallel_read(&calls[i].1) {
                let end = calls[i..].iter().position(|(_, c)| !parallel_read(c)).map_or(calls.len(), |n| i + n);
                let started = Instant::now();
                let outputs = self.execute_reads(&calls[i..end], truncated).await;
                let took = started.elapsed();
                for ((id, call), output) in calls[i..end].iter().zip(outputs) {
                    if let Some(image) = &output.image {
                        images.push((tools::summary(&call.name, &parse_args(call).unwrap_or_default(), &self.ctx.cwd), image.clone()));
                    }
                    stop |= self.finish_call(id, call, output, took)?;
                }
                i = end;
                continue;
            }
            if calls[i].1.name == "agent" && self.depth == 0 {
                let end = calls[i..].iter().position(|(_, c)| c.name != "agent").map_or(calls.len(), |n| i + n);
                let started = Instant::now();
                let outputs = self.run_subagents(&calls[i..end]).await;
                let took = started.elapsed();
                for ((id, call), output) in calls[i..end].iter().zip(outputs) {
                    stop |= self.finish_call(id, call, output, took)?;
                }
                i = end;
                continue;
            }
            let (id, call) = &calls[i];
            if call.name == "agent_swarm" && self.depth == 0 {
                let started = Instant::now();
                let output = self.run_swarm(id, call).await;
                stop |= self.finish_call(id, call, output, started.elapsed())?;
                i += 1;
                continue;
            }
            let started = Instant::now();
            let output = self.execute(id, call, truncated).await;
            let took = started.elapsed();
            if let Some(image) = &output.image {
                images.push((tools::summary(&call.name, &parse_args(call).unwrap_or_default(), &self.ctx.cwd), image.clone()));
            }
            stop |= self.finish_call(id, call, output, took)?;
            i += 1;
        }
        if !images.is_empty() {
            let mut parts = Vec::new();
            for (label, url) in images {
                parts.extend(image_parts(&format!("Image from {label}:"), &url));
            }
            self.push(json!({"role": "user", "content": parts}))?;
        }
        Ok(stop)
    }

    /// The images pasted into a prompt, as a user message right after it with each image
    /// behind its placeholder, so the prompt itself stays a plain text message.
    pub(super) fn push_images(&mut self, images: &[Image]) -> Result<()> {
        if images.is_empty() {
            return Ok(());
        }
        let parts: Vec<Value> = images.iter().flat_map(|image| image_parts(&format!("{}:", image.label), &image.url)).collect();
        self.push(json!({"role": "user", "content": parts}))
    }

    /// Runs a consecutive read-only group without a `Send` bound, then returns outputs in call
    /// order. Parsing, summaries and instruction-file discovery stay the same as for `execute`.
    async fn execute_reads(&mut self, group: &[(String, ToolCall)], truncated: bool) -> Vec<tools::Output> {
        let mut pending = Vec::with_capacity(group.len());
        for (id, call) in group {
            self.stats.tool_calls += 1;
            let mut invalid = None;
            let args = match parse_args(call) {
                Ok(args) => {
                    self.tool_start(id, &tools::summary(&call.name, &args, &self.ctx.cwd));
                    if let Some(path) = args["path"].as_str() {
                        self.touched(tools::resolve(&self.ctx, path));
                    }
                    args
                }
                Err(why) => {
                    self.tool_start(id, &format!("{} (invalid arguments)", call.name));
                    invalid = Some(invalid_arguments(call, &why, truncated));
                    json!({})
                }
            };
            let ctx = self.ctx.clone();
            let name = call.name.clone();
            let steer = (self.depth == 0).then(|| self.controls.steer.clone());
            pending.push(async move {
                if let Some(output) = invalid {
                    output
                } else {
                    dispatch_tool(&ctx, &name, args, steer).await
                }
            });
        }
        join_all(pending).await
    }

    /// Notes AGENTS.md files for `path`, including the one inside it when it is a directory.
    /// `touched` only walks the parents, so a directory is probed through a child that never
    /// matches a lookup itself.
    fn touched(&mut self, path: PathBuf) {
        self.instructions.touched(&path);
        if path.is_dir() {
            self.instructions.touched(&path.join("_"));
        }
    }

    pub(super) async fn execute(&mut self, id: &str, call: &ToolCall, truncated: bool) -> tools::Output {
        self.stats.tool_calls += 1;
        let args = match parse_args(call) {
            Ok(args) => args,
            Err(why) => {
                let output = invalid_arguments(call, &why, truncated);
                self.tool_start(id, &format!("{} (invalid arguments)", call.name));
                return output;
            }
        };
        self.tool_start(id, &tools::summary(&call.name, &args, &self.ctx.cwd));
        let before = self.hook("PreToolUse", Some(&call.name), tool_payload(&call.name, &args, None)).await;
        if let Some(reason) = before.block {
            return tools::Output::error(format!("A hook blocked this call: {reason}"));
        }
        if let Some(path) = args["path"].as_str() {
            self.touched(tools::resolve(&self.ctx, path));
        }
        if call.name == "bash"
            && let Some(cwd) = args["cwd"].as_str().filter(|cwd| !cwd.is_empty())
        {
            self.touched(tools::resolve(&self.ctx, cwd));
        }
        let mut output = self.dispatch(call, &args).await;
        self.post_hook(&call.name, &args, &mut output).await;
        output
    }

    pub(super) async fn dispatch(&mut self, call: &ToolCall, args: &Value) -> tools::Output {
        let args = args.clone();
        if self.controls.plan.get() && let Some(output) = self.plan_block(call, &args) {
            return output;
        }
        if let Some(output) = self.peer_block(call, &args) {
            return output;
        }
        match call.name.as_str() {
            "skill" => self.settings.skills.run(&args),
            "mcp" => self.settings.mcp.run(&args, self.ctx.images.get()).await,
            name if self.settings.mcp.is_native(name) => self.settings.mcp.call_native(name, args, self.ctx.images.get()).await,
            "todo_write" => self.todo_write(&args),
            "later" => self.later(&args),
            "ask_user_question" => self.ask(&args).await,
            "exit_plan_mode" => self.exit_plan(&args).await,
            "agent" | "agent_swarm" => tools::Output::error("Subagents can't start other subagents."),
            _ => {
                // Only the top-level agent hears the user; a subagent waits out its own commands.
                let steer = (self.depth == 0).then(|| self.controls.steer.clone());
                dispatch_tool(&self.ctx, &call.name, args, steer).await
            }
        }
    }

    /// What plan mode refuses before anything runs: writes, commands that aren't read-only,
    /// and MCP tool calls. Listing a server's tools stays allowed. `None` runs as usual.
    fn plan_block(&self, call: &ToolCall, args: &Value) -> Option<tools::Output> {
        if call.name == "mcp" {
            let lists_only = args["tool"].as_str().is_none_or(|tool| tool.is_empty());
            return (!lists_only).then(|| tools::Output::error(
                "Plan mode is on, so MCP tools can't run; listing a server's tools with `mcp` stays allowed. Finish the plan and call exit_plan_mode.",
            ));
        }
        if self.settings.mcp.is_native(&call.name) {
            return Some(tools::Output::error(
                "Plan mode is on, so MCP tools can't run; listing a server's tools with `mcp` stays allowed. Finish the plan and call exit_plan_mode.",
            ));
        }
        if matches!(call.name.as_str(), "write" | "edit") {
            return Some(tools::Output::error(format!(
                "Plan mode is on, so {} is blocked. Finish the plan and call exit_plan_mode.",
                call.name
            )));
        }
        if call.name == "bash"
            && !plan::bash_allowed(args["command"].as_str().unwrap_or_default(), &self.settings.plan_allow)
        {
            return Some(tools::Output::error("Plan mode is on: bash runs only read-only commands (ls, rg, git log, cargo check, …), without redirects. Finish the plan and call exit_plan_mode."));
        }
        None
    }

    /// What another session working in this tree refuses before anything runs: `bash`
    /// calls that move the tree (`checkout`, `switch`, `restore`, `reset`, `clean`, and
    /// `stash` other than `list`/`show`). Only the tool list's `bash` is checked, and the
    /// registry is read only when the command names such a verb. A linked worktree is a
    /// tree of its own, so it is not blocked; `[agent] peers = false` leaves `here` unset
    /// and turns this off. `None` runs as usual.
    fn peer_block(&self, call: &ToolCall, args: &Value) -> Option<tools::Output> {
        if call.name != "bash" {
            return None;
        }
        let command = args["command"].as_str().unwrap_or_default();
        let verb = super::peer_guard::moving_verb(command)?;
        let here = self.controls.here.as_ref()?;
        let base = args["cwd"]
            .as_str()
            .filter(|cwd| !cwd.is_empty())
            .map(|cwd| tools::resolve(&self.ctx, cwd))
            .unwrap_or_else(|| self.ctx.cwd.clone());
        let dir = super::peer_guard::command_dir(command)
            .map(|target| resolve_target(&target, &base))
            .unwrap_or(base);
        if live::work_tree(&dir) != here.tree {
            return None;
        }
        let peer = here.others().into_iter().next()?;
        Some(tools::Output::error(format!(
            "Not run: another killi session (pid {}) works in this work tree, and `git {verb}` can discard or move its changes. Use a separate git worktree (`git worktree add ../<name>`) or ask the user to run it.",
            peer.pid,
        )))
    }

    pub(super) async fn hook(&self, event: &str, tool: Option<&str>, payload: Value) -> crate::hooks::Outcome {
        if self.settings.hooks.is_empty() {
            return crate::hooks::Outcome::default();
        }
        let outcome = self.settings.hooks.run(event, tool, &self.ctx.cwd, &self.session.id, payload).await;
        for command in &outcome.failures {
            if self.hook_notified.borrow_mut().insert(command.clone()) {
                (self.sink)(Event::Notice(format!("Hook `{command}` timed out or failed to start, so it was skipped.")));
            }
        }
        outcome
    }

    pub(super) fn tool_start(&self, id: &str, summary: &str) {
        (self.sink)(Event::ToolStart { id: id.into(), summary: summary.into() });
    }

    /// Reports and records a call's result. Returns true when the repeat breaker trips.
    pub(super) fn finish_call(&mut self, id: &str, call: &ToolCall, mut output: tools::Output, took: Duration) -> Result<bool> {
        if self.partial_swarm.as_ref().is_some_and(|partial| partial.id == id) {
            self.partial_swarm = None;
        }
        if call.name == "todo_write" && !output.is_error {
            self.calls_since_todo = 0;
        } else {
            self.calls_since_todo += 1;
        }
        (self.sink)(Event::ToolEnd {
            id: id.into(),
            is_error: output.is_error,
            brief: output.brief.clone(),
            display: output.display.take(),
            output: output.text.clone(),
        });
        output.text = neutralize_reminder_tags(&output.text).into_owned();
        if output.text.len() > MAX_RESULT_BYTES {
            output.text = self.shorten(&output.text);
        }
        let result = hash_text(&output.text);
        let (count, same) = self.repeats.entry(repeat_key(call, result)).or_default().note(result);
        if REPEAT_WARNINGS.contains(&count) {
            let warning = if same {
                " and got the same result. Repeating it won't change the result; try a different approach."
            } else {
                ". Its result changed, so this is a repetition warning, not a stale-result loop."
            };
            output.text.push_str(&format!(
                "\n\n<system-reminder>You have made this exact {} call {count} times{warning}</system-reminder>",
                call.name
            ));
        }
        let meta = json!({"name": call.name, "is_error": output.is_error, "brief": output.brief, "ms": millis(took)});
        self.push_meta(json!({"role": "tool", "tool_call_id": id, "content": output.text}), Some(meta))?;
        Ok(same && count >= REPEAT_STOP)
    }

    /// Runs a tool call's PostToolUse hooks over its result, appending what they say.
    pub(super) async fn post_hook(&self, name: &str, args: &Value, output: &mut tools::Output) {
        let after = self.hook("PostToolUse", Some(name), tool_payload(name, args, Some(&output.text))).await;
        if let Some(note) = after.block.or_else(|| (!after.context.is_empty()).then(|| after.context.join("\n"))) {
            output.text.push_str(&format!("\n\n<system-reminder>A hook says: {note}</system-reminder>"));
        }
    }
}

/// Whether a built-in call only reads files or search results and can share a read-only batch.
pub(super) fn parallel_read(call: &ToolCall) -> bool {
    matches!(call.name.as_str(), "read" | "grep" | "glob")
}

/// A literal directory from a command, against the directory the call runs in: `~/…`
/// from home, an absolute path as is, anything else relative to the call's directory.
fn resolve_target(target: &str, base: &Path) -> PathBuf {
    match target.strip_prefix("~/") {
        Some(rest) => crate::config::home().join(rest),
        None => {
            let path = PathBuf::from(target);
            if path.is_absolute() { path } else { base.join(path) }
        }
    }
}

/// Dispatches the tools that need only their shared context, without borrowing the agent.
pub(super) async fn dispatch_tool(ctx: &tools::Context, name: &str, args: Value, steer: Option<Rc<RefCell<VecDeque<Steered>>>>) -> tools::Output {
    let wake = move || steer.as_ref().is_some_and(|steer| !steer.borrow().is_empty());
    tools::run(ctx, name, args, &wake).await
}

/// Untrusted tool output as it is stored in history: a literal reminder tag in it would read
/// with killi's authority, so the tag's `<` reads as text instead. This runs before killi's
/// own reminders are appended, so those stay real. Same tags as `tui::mention`'s file blocks.
pub(super) fn neutralize_reminder_tags(text: &str) -> Cow<'_, str> {
    if !(text.contains("<system-reminder") || text.contains("</system-reminder")) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace("<system-reminder", "&lt;system-reminder").replace("</system-reminder", "&lt;/system-reminder"))
}

fn hash_text(text: &str) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Identifies a tool call by its name and exact arguments, without keeping the arguments.
pub(super) fn hash_call(call: &ToolCall) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    (&call.name, &call.arguments).hash(&mut hasher);
    hasher.finish()
}

/// What the repeat breaker counts as the same call. A wait on a background task is a poll:
/// asking again is the point, and its result says how long the task has run, so it only repeats
/// when the result does too. Every other call repeats whatever it returns.
pub(super) fn repeat_key(call: &ToolCall, result: u64) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    if call.name != "task_output" {
        return hash_call(call);
    }
    let mut hasher = DefaultHasher::new();
    (hash_call(call), result).hash(&mut hasher);
    hasher.finish()
}

/// An image in a user message: a line of text that says what it is, then the image.
pub(super) fn image_parts(label: &str, url: &str) -> [Value; 2] {
    [json!({"type": "text", "text": label}), json!({"type": "image_url", "image_url": {"url": url}})]
}

pub(super) fn parse_args(call: &ToolCall) -> Result<Value, String> {
    match call.arguments.trim() {
        "" => Ok(json!({})),
        raw => match serde_json::from_str::<Value>(raw) {
            Ok(args) if args.is_object() => Ok(args),
            Ok(_) => Err("arguments must be a JSON object".into()),
            Err(e) => match repair_json(raw).and_then(|fixed| serde_json::from_str::<Value>(&fixed).ok()) {
                Some(args) if args.is_object() => Ok(args),
                _ => Err(e.to_string()),
            },
        },
    }
}

/// Fixes common slips in tool-call JSON: a code fence around it, or trailing commas.
fn repair_json(raw: &str) -> Option<String> {
    let mut text = raw.trim();
    if let Some(inner) = text.strip_prefix("```json").or_else(|| text.strip_prefix("```")) {
        text = inner.strip_suffix("```").unwrap_or(inner).trim();
    }
    let fixed = regex::Regex::new(r",\s*([}\]])").ok()?.replace_all(text, "$1").into_owned();
    (fixed != raw).then_some(fixed)
}

pub(super) fn invalid_arguments(call: &ToolCall, why: &str, truncated: bool) -> tools::Output {
    let cut = if truncated {
        " Your reply hit the output token limit, which cut the arguments off; send smaller pieces."
    } else {
        ""
    };
    tools::Output::error(format!("Invalid arguments for {}: {why}.{cut} You sent: {}", call.name, truncate(&call.arguments, 2000)))
}
