mod drive;
mod provider;

use super::*;
#[cfg(test)]
pub(super) use drive::{finish_snapshot, wants_recap};
pub(super) use drive::{flush_recap, prompt_turn, wake_turn};
#[cfg(test)]
pub(super) use provider::provider_switch_notice;
pub(super) use provider::{provider_add, provider_custom, provider_custom_add};

/// The next request from another session; never, for a session that takes none.
pub(super) async fn next_incoming(
    peers: &mut Option<mpsc::UnboundedReceiver<live::Incoming>>,
) -> Option<live::Incoming> {
    match peers {
        Some(peers) => peers.recv().await,
        None => std::future::pending().await,
    }
}

/// Who a question or message came from, in two words.
pub(super) fn peer_label(from: Option<u32>) -> String {
    from.map_or_else(|| "a shell".to_string(), |pid| format!("session {pid}"))
}

/// The other live sessions in `tree` that worked since `since` (ms since the epoch), or still
/// do, on a turn of their own or on a task another session sent them: an undo back to then
/// would undo their changes too.
fn worked_since(tree: &Path, since: Option<u64>) -> Vec<live::Entry> {
    let Some(since) = since else {
        return Vec::new();
    };
    let me = std::process::id();
    live::list()
        .into_iter()
        .filter(|e| e.pid != me && e.tree == tree && e.worked_since(since))
        .collect()
}

/// Runs one future that drives the agent, showing its events and handling keys meanwhile.
pub(super) async fn run_turn<F>(
    app: &mut App,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    future: F,
) -> Result<Ended>
where
    F: Future<Output = Result<(String, Stop)>>,
{
    let mut future = Box::pin(future);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut edge = tokio::time::interval(Duration::from_millis(50));
    edge.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // A /btw question asked meanwhile; the turn waits for its answer before it ends.
    let mut aside: Option<PendingAside> = None;
    let mut done = None;
    let ended = loop {
        if let Some(question) = app.aside_asked.take() {
            if aside.is_some() {
                app.notice("Still answering the last /btw question.");
            } else {
                let ask = app.controls.aside.borrow().ask(&question);
                app.aside = Some((text::sanitize_row(&question).into_owned(), Instant::now()));
                aside = Some((question, Box::pin(ask)));
            }
        }
        tokio::select! {
            biased;
            Some(event) = rx.recv() => app.on_agent(event),
            // Before the keys: an Esc that lands as the work ends doesn't undo its result.
            result = &mut future, if done.is_none() => done = Some(result),
            Some(Ok(event)) = keys.next() => {
                if app.on_key_busy(event) {
                    break done.take().map_or(Ended::Interrupted, Ended::Done);
                }
            }
            Some(incoming) = next_incoming(&mut app.peers) => app.on_peer(incoming),
            Some(answered) = app.peer_asks.next(), if !app.peer_asks.is_empty() => app.on_peer_answer(answered),
            Some(done) = app.peer_running.next(), if !app.peer_running.is_empty() => app.on_peer_task_done(done),
            Some(work) = background::next_work(&mut app.bg_rx) => app.on_background_work(work),
            Some(id) = app.bg_running.next(), if !app.bg_running.is_empty() => app.on_background_done(id),
            answer = async { aside.as_mut().expect("a question is asked").1.as_mut().await }, if aside.is_some() => {
                let (question, _) = aside.take().expect("a question is asked");
                app.show_aside(&question, answer);
            }
            _ = tick.tick() => app.spinner += 1,
            _ = edge.tick(), if app.edge_scrolling() => app.edge_scroll(),
        }
        if aside.is_none()
            && let Some(result) = done.take()
        {
            break Ended::Done(result);
        }
        if app.queued.len() > app.controls.steer.borrow().len() {
            // The agent took queued messages between steps: what it did before that comes first.
            show_events(app, rx);
            app.show_sent();
        }
        app.draw()?;
    };
    drop(future);
    show_events(app, rx);
    app.show_sent();
    Ok(ended)
}

/// Shows the agent's events already sent: the end of a turn's, or the notes a resume leaves
/// before any turn runs.
pub(super) fn show_events(app: &mut App, rx: &mut mpsc::UnboundedReceiver<Event>) {
    while let Ok(event) = rx.try_recv() {
        app.on_agent(event);
    }
}

/// Runs a command's work — /btw, /recap, /compact, /schedule, /provider — as a busy spell
/// with the keys live. No agent turn takes what is typed meanwhile, so it goes back into the
/// prompt afterwards; Esc drops it and cancels the command.
pub(super) async fn command_turn<F>(
    app: &mut App,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    future: F,
) -> Result<Ended>
where
    F: Future<Output = Result<(String, Stop)>>,
{
    app.begin_busy();
    let ended = run_turn(app, keys, rx, future).await?;
    app.end_busy();
    app.restore_queued(&ended);
    Ok(ended)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_command(
    app: &mut App,
    agent: &mut Agent,
    session: &mut Setup,
    snapshots: &mut Snapshots,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    cwd: &Path,
    command: &str,
    arg: &str,
    images: &[Image],
) -> Result<bool> {
    if app.run_local(command, arg) {
        return Ok(true);
    }
    match command {
        "exit" | "quit" | "q" => {
            if let Some(note) = crate::goal::blocks("Quitting", agent.goal()) {
                app.notice(&note);
                return Ok(true);
            }
            if app.background_working() > 0 {
                app.open_quit_picker();
                return Ok(true);
            }
            return Ok(false);
        }
        "btw" if agent.user_prompts().is_empty() => app.notice("Nothing to ask about yet."),
        "btw" => {
            app.aside_asked = Some(arg.to_string());
            app.begin_busy();
            let ended = run_turn(
                app,
                keys,
                rx,
                std::future::ready(Ok((String::new(), Stop::Completed))),
            )
            .await?;
            // The spell waits only for the answer: a question still out was cut off by Esc.
            let ended = if app.aside.is_some() {
                Ended::Interrupted
            } else {
                ended
            };
            app.end_busy();
            app.restore_queued(&ended);
        }
        "recap" if agent.user_prompts().is_empty() => app.notice("Nothing to recap yet."),
        "recap" => {
            let ask = app.controls.aside.borrow().recap();
            let used = Rc::new(RefCell::new(None));
            let slot = used.clone();
            let ended = command_turn(app, keys, rx, async move {
                let (text, usage) = ask.await?;
                *slot.borrow_mut() = Some(usage);
                Ok((text, Stop::Completed))
            })
            .await?;
            match (ended, used.take()) {
                (Ended::Done(Ok((text, _))), Some(usage)) => {
                    app.show_recap(&text, &usage);
                    flush_recap(app, agent);
                }
                (Ended::Done(Err(e)), _) => app.error_from("/recap", &e),
                _ => {}
            }
        }
        "schedule" if !images.is_empty() => app.notice("Scheduled tasks take text only."),
        "schedule" => schedule::command(app, agent, keys, rx, arg).await?,
        "goal" => goal_command(app, agent, arg),
        "memory" | "remember" => memory_command(app, agent, command, arg),
        "swarm"
            if arg
                .split_whitespace()
                .next()
                .is_some_and(|word| word == "parallel") =>
        {
            let n = arg
                .split_whitespace()
                .nth(1)
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|n| *n > 0);
            let Some(n) = n else {
                app.error(
                    "Use /swarm parallel <n> with n at least 1, for example /swarm parallel 16.",
                );
                return Ok(true);
            };
            let path = crate::config::killi_home().join("config.toml");
            if let Err(e) = crate::config::set_swarm_parallel(&path, n) {
                app.error_report(&e);
                return Ok(true);
            }
            session.set_swarm_parallel(n);
            agent.set_swarm_parallel(n);
            app.notice(&format!(
                "Swarm parallel: {n} subagents at once; saved to {}.",
                path.display()
            ));
        }
        "pair" => {
            pair::pair_command(app, agent, session, snapshots, keys, rx, arg).await?;
        }
        "swarm" => {
            let was_on = app.controls.swarm.replace(true);
            let result = prompt_turn(
                app,
                agent,
                snapshots,
                keys,
                rx,
                &format!("/swarm {arg}"),
                &mention::expand(cwd, arg),
                images,
            )
            .await;
            // A /swarm off typed mid-turn stays off: only an untouched mode goes back.
            if app.controls.swarm.get() {
                app.controls.swarm.set(was_on);
            }
            result?;
        }
        "undo" => {
            if agent.pair_active() {
                app.notice("Undo waits for the pair: /pair stop ends it first.");
                return Ok(true);
            }
            let working = app.background_working();
            if working > 0 {
                app.notice(&format!(
                    "Undo waits: {} still {} in the background, and putting files back would pull them from under {}. Wait for {} to end, or ask the agent to stop {}.",
                    crate::tools::count(working, "subagent task"),
                    if working == 1 { "works" } else { "work" },
                    if working == 1 { "it" } else { "them" },
                    if working == 1 { "it" } else { "them" },
                    if working == 1 { "it" } else { "them" },
                ));
                return Ok(true);
            }
            // Said once: the next /undo goes ahead.
            // Other sessions, and the tasks this one ran for them: both change the tree
            // outside this session's turns.
            let who: Vec<String> = if app.undo_warned {
                Vec::new()
            } else {
                let since = snapshots.last_taken();
                worked_since(snapshots.work_tree(), since)
                    .iter()
                    .map(|e| format!("session {}", e.pid))
                    .chain(app.tasks_since(since))
                    .chain(app.background_since(since))
                    .collect()
            };
            app.undo_warned = !who.is_empty();
            if !who.is_empty() {
                app.notice(&format!(
                    "{} also worked in this repository since the last turn began. Undo puts every file back as it was then, so those changes go too. /undo again to undo anyway.",
                    who.join(" and ")
                ));
                return Ok(true);
            }
            match snapshots.undo() {
                Ok(changes) if changes.is_empty() => {
                    app.notice("Undone. No files had changed in the last turn.")
                }
                Err(_) if undo_empty(snapshots) => {
                    app.notice("Nothing to undo yet.");
                }
                Ok(changes) => {
                    app.notice(&format!(
                        "Undone: {} restored to how they were before the last turn.",
                        crate::tools::count(changes.len(), "file")
                    ));
                    for change in changes.iter().take(20) {
                        app.commit(format!("  {DIM}{change}{RESET}"));
                    }
                    let before = if std::mem::take(&mut app.undo_wake) {
                        "the turn you took when background work ended".to_string()
                    } else {
                        let first = agent
                            .user_prompts()
                            .last()
                            .map(|prompt| prompt.lines().next().unwrap_or("").trim().to_string())
                            .unwrap_or_default();
                        format!("\"{first}\"")
                    };
                    agent.note_for_next_prompt(undo_note(&before, &undo_paths(&changes)));
                }
                Err(e) => app.error(&format!("Can't undo: {e:#}")),
            }
        }
        "compact" if agent.user_prompts().is_empty() => app.notice("Nothing to compact yet."),
        "compact" => {
            let focus = arg.to_string();
            let ended = command_turn(app, keys, rx, async {
                agent
                    .compact(Some(&focus))
                    .await
                    .map(|()| (String::new(), Stop::Completed))
            })
            .await?;
            match ended {
                Ended::Done(Err(e)) => app.error_from("Compaction failed", &e),
                Ended::Done(Ok(_)) => app.status.context = agent.stats.context,
                _ => {}
            }
        }
        "clear" | "new" => {
            if let Some(note) = crate::goal::blocks("Starting a new session", agent.goal()) {
                app.notice(&note);
                return Ok(true);
            }
            if agent.pair_active() {
                app.notice("A pair is still running; /pair stop ends it before a new session.");
                return Ok(true);
            }
            session.refresh_prompt();
            match session.start_on(agent.client(), agent.controls.clone(), agent_sink(agent)) {
                Ok(fresh) => {
                    app.pause_background();
                    app.reset_conversation(snapshots, cwd);
                    let saved = !agent.user_prompts().is_empty();
                    let stopped = agent.stop_tasks();
                    agent.discard_if_unused();
                    *agent = fresh;
                    app.show_model(agent, session.config());
                    app.wipe_screen();
                    app.banner(agent, session, snapshots, false);
                    app.notice(if saved {
                        "Started a new session. The last one is saved; /resume picks it up again."
                    } else {
                        "Started a new session."
                    });
                    if let Some(note) = stopped_note(&stopped) {
                        app.notice(&note);
                    }
                }
                Err(e) => app.error_report(&e),
            }
        }
        "resume" => {
            if let Some(note) = crate::goal::blocks("Resuming another session", agent.goal()) {
                app.notice(&note);
                return Ok(true);
            }
            if agent.pair_active() {
                app.notice("A pair is still running; /pair stop ends it before a resume.");
                return Ok(true);
            }
            app.open_session_picker(cwd)
        }
        "prune" if arg.is_empty() => app.open_prune_picker(&agent.session.log_path),
        "prune" => match arg.parse::<usize>() {
            Ok(keep) => app.next_input = Some(Input::Prune(keep)),
            Err(_) => app.error("Say how many of the newest sessions to keep: /prune 20."),
        },
        "mcp" if arg.is_empty() => app.open_mcp_picker(session.mcp(), None),
        "mcp" => app.next_input = Some(Input::Mcp(arg.to_string(), false)),
        "web-search-config" if arg.is_empty() => app.open_search_picker(session.config()),
        "web-search-config" => {
            let provider = arg.to_lowercase();
            if ["brave", "tavily", "keenable", "searxng"].contains(&provider.as_str()) {
                app.search = Some(SearchFlow {
                    provider: None,
                    docker: None,
                    url: None,
                    current: session.config().search.clone(),
                });
                app.search_pick(&provider);
            } else {
                app.error(
                    "Pick brave, tavily, keenable or searxng; /web-search-config alone lists them.",
                );
            }
        }
        "model" if arg.is_empty() => app.open_model_picker(session.config(), ModelPick::Session),
        "model" => app.next_input = Some(Input::Model(arg.to_string())),
        "provider" if arg.is_empty() => app.open_provider_picker(session.config()),
        "provider" => app.provider_pick(arg),
        "backup-model" if arg.is_empty() => {
            app.open_model_picker(session.config(), ModelPick::Backup)
        }
        "backup-model" => app.next_input = Some(Input::BackupModel(arg.to_string())),
        "advisor" if arg.is_empty() => app.open_model_picker(session.config(), ModelPick::Advisor),
        "advisor" => app.next_input = Some(Input::Advisor(arg.to_string())),
        "effort" | "thinking" if arg.is_empty() => app.open_effort_picker(session.config()),
        "effort" | "thinking" => app.next_input = Some(Input::Effort(arg.to_lowercase())),
        "theme" if arg.is_empty() => app.open_theme_picker(),
        "theme" => app.next_input = Some(Input::Theme(arg.to_string())),
        "reload" => match session.reload() {
            Ok(()) => {
                session.reload_agent(agent)?;
                app.entries = entries(session);
                app.skills = session.skills().clone();
                app.refresh_choices(session.config());
                app.redraw_banner(agent, session, snapshots);
                app.notice("Reloaded the system prompt ([prompt] in config.toml), AGENTS.md files, skills and [search]. The next request starts the cache over; models, MCP servers and tools apply when killi restarts.");
                if session.skills_need_restart() {
                    app.notice("This session started without skills, so the model can't load them until killi restarts; /name runs one now.");
                }
            }
            Err(e) => app.error(&format!("Can't reload: {e:#}")),
        },
        "usage" | "cost" => app.usage_report(agent),
        "session" | "share" => app.share_session(agent),
        other => match session.skill_prompt(other.strip_prefix("skill:").unwrap_or(other), arg) {
            Some(prompt) => {
                let sent = match mention::attachment(cwd, arg) {
                    Some(files) => format!("{prompt}\n\n{files}"),
                    None => prompt,
                };
                prompt_turn(
                    app,
                    agent,
                    snapshots,
                    keys,
                    rx,
                    format!("/{other} {arg}").trim_end(),
                    &sent,
                    images,
                )
                .await?;
            }
            None => app.error(&format!(
                "Unknown command /{other}. Type /help for the list."
            )),
        },
    }
    Ok(true)
}

/// `/memory` lists what is saved, `/memory add <fact>` and `/remember <fact>` save one, and
/// `/memory forget N` removes one. The model hears a save or removal at its next prompt, since
/// the system prompt was built before it.
fn memory_command(app: &mut App, agent: &mut Agent, command: &str, arg: &str) {
    let arg = arg.trim();
    let (action, text) = match (command, arg) {
        ("remember", fact) => ("add", fact),
        ("memory", "") => ("list", ""),
        ("memory", rest) => rest
            .split_once(char::is_whitespace)
            .map_or((rest, ""), |(action, rest)| (action, rest.trim())),
        _ => ("list", ""),
    };
    match action {
        "list" => app.show_memory(),
        "add" if !text.is_empty() => match crate::memory::add(text) {
            Ok(crate::memory::Saved::Added(at)) => {
                app.notice(&format!("Remembered ({at}): {text}"));
                agent.refresh_memory();
                agent.note_for_next_prompt(format!(
                    "The user saved this to memory, and it holds in every project from now on: {text}"
                ));
            }
            Ok(crate::memory::Saved::Already(at)) => {
                app.notice(&format!("Already remembered as {at}: {text}"))
            }
            Err(e) => app.error(&format!("Can't remember that: {e:#}")),
        },
        "forget" => match text.parse::<usize>() {
            Ok(number) => match crate::memory::forget(number) {
                Ok((item, left)) => {
                    app.notice(&format!("Forgot {number}: {item} ({left} saved)"));
                    agent.refresh_memory();
                    agent.note_for_next_prompt(format!(
                        "The user removed this saved memory, so it no longer applies: {item}"
                    ));
                }
                Err(e) => app.error(&format!("Can't forget that: {e:#}")),
            },
            Err(_) => app.error("Say which one: /memory forget N, with N as /memory counts them."),
        },
        _ => app.error("Use /memory, /memory add <fact>, /memory forget N, or /remember <fact>."),
    }
}

/// `/goal [<text>|done|clear]`: shows the session goal, sets one the model hears on its
/// next step, or ends it. The session can't end while a goal stands.
fn goal_command(app: &mut App, agent: &mut Agent, arg: &str) {
    let arg = arg.trim();
    if arg.is_empty() {
        match agent.goal() {
            Some(goal) => app.notice(&format!("Session goal: {goal}")),
            None => app.notice(crate::goal::EMPTY),
        }
        return;
    }
    let (verb, rest) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
    match verb {
        "done" if rest.trim().is_empty() => match agent.goal() {
            Some(goal) => match agent.clear_goal() {
                Ok(()) => app.notice(&format!("Goal done: {goal}")),
                Err(e) => app.error_report(&e),
            },
            None => app.notice("No session goal to finish."),
        },
        "clear" => match agent.goal() {
            Some(goal) => match agent.clear_goal() {
                Ok(()) => {
                    let reason = rest.trim();
                    app.notice(&if reason.is_empty() {
                        format!("Goal cleared: {goal}")
                    } else {
                        format!("Goal cleared: {goal} ({reason})")
                    });
                }
                Err(e) => app.error_report(&e),
            },
            None => app.notice("No session goal to clear."),
        },
        _ => match agent.goal() {
            Some(current) => match agent.set_goal(arg) {
                Ok(()) => app.notice(&format!("Goal replaced: {current} → {arg}")),
                Err(e) => app.error_report(&e),
            },
            None => match agent.set_goal(arg) {
                Ok(()) => app.notice(&format!("Session goal: {arg}")),
                Err(e) => app.error_report(&e),
            },
        },
    }
}

fn agent_sink(agent: &Agent) -> Sink {
    agent.sink()
}
