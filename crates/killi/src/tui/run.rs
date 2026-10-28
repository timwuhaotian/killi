use super::*;

/// The next request from another session; never, for a session that takes none.
pub(super) async fn next_incoming(peers: &mut Option<mpsc::UnboundedReceiver<live::Incoming>>) -> Option<live::Incoming> {
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
/// do: an undo back to then would undo their changes too.
fn worked_since(tree: &Path, since: Option<u64>) -> Vec<live::Entry> {
    let Some(since) = since else { return Vec::new() };
    let me = std::process::id();
    live::list().into_iter().filter(|e| e.pid != me && e.tree == tree && (e.working || e.since >= since)).collect()
}

/// Runs one future that drives the agent, showing its events and handling keys meanwhile.
async fn run_turn<F>(
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
            Some(Ok(event)) = keys.next() => {
                if app.on_key_busy(event) {
                    break done.take().map_or(Ended::Interrupted, Ended::Done);
                }
            }
            result = &mut future, if done.is_none() => done = Some(result),
            Some(incoming) = next_incoming(&mut app.peers) => app.on_peer(incoming),
            Some(answered) = app.peer_asks.next(), if !app.peer_asks.is_empty() => app.on_peer_answer(answered),
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
            while let Ok(event) = rx.try_recv() {
                app.on_agent(event);
            }
            app.show_sent();
        }
        app.draw()?;
    };
    drop(future);
    while let Ok(event) = rx.try_recv() {
        app.on_agent(event);
    }
    app.show_sent();
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
        "exit" | "quit" | "q" => return Ok(false),
        "btw" if agent.user_prompts().is_empty() => app.notice("Nothing to ask about yet."),
        "btw" => {
            app.aside_asked = Some(arg.to_string());
            app.begin_busy();
            run_turn(app, keys, rx, std::future::ready(Ok((String::new(), Stop::Completed)))).await?;
            app.end_busy();
        }
        "recap" if agent.user_prompts().is_empty() => app.notice("Nothing to recap yet."),
        "recap" => {
            let ask = app.controls.aside.borrow().recap();
            let used = Rc::new(RefCell::new(None));
            let slot = used.clone();
            app.begin_busy();
            let ended = run_turn(app, keys, rx, async move {
                let (text, usage) = ask.await?;
                *slot.borrow_mut() = Some(usage);
                Ok((text, Stop::Completed))
            })
            .await?;
            app.end_busy();
            match (ended, used.take()) {
                (Ended::Done(Ok((text, _))), Some(usage)) => app.show_recap(&text, &usage),
                (Ended::Done(Err(e)), _) => app.error_from("/recap", &e),
                _ => {}
            }
        }
        "swarm" => {
            let was_on = app.controls.swarm.replace(true);
            let result = prompt_turn(app, agent, snapshots, keys, rx, &format!("/swarm {arg}"), &mention::expand(cwd, arg), images).await;
            // A /swarm off typed mid-turn stays off: only an untouched mode goes back.
            if app.controls.swarm.get() {
                app.controls.swarm.set(was_on);
            }
            result?;
        }
        "undo" => {
            // Said once: the next /undo goes ahead.
            let others = if app.undo_warned { Vec::new() } else { worked_since(snapshots.work_tree(), snapshots.last_taken()) };
            app.undo_warned = !others.is_empty();
            if !others.is_empty() {
                let who: Vec<String> = others.iter().map(|e| format!("session {}", e.pid)).collect();
                app.notice(&format!(
                    "{} also worked in this repository since the last turn began. Undo puts every file back as it was then, so those changes go too. /undo again to undo anyway.",
                    who.join(" and ")
                ));
                return Ok(true);
            }
            match snapshots.undo() {
                Ok(changes) if changes.is_empty() => app.notice("Undone. No files had changed in the last turn."),
                Err(_) if undo_empty(snapshots) => {
                    app.notice("Nothing to undo yet.");
                }
                Ok(changes) => {
                    app.notice(&format!("Undone: {} restored to how they were before the last turn.", crate::tools::count(changes.len(), "file")));
                    for change in changes.iter().take(20) {
                        app.commit(format!("  {DIM}{change}{RESET}"));
                    }
                    let first = agent
                        .user_prompts()
                        .last()
                        .map(|prompt| prompt.lines().next().unwrap_or("").trim().to_string())
                        .unwrap_or_default();
                    agent.note_for_next_prompt(undo_note(&first, &undo_paths(&changes)));
                }
                Err(e) => app.error(&format!("Can't undo: {e:#}")),
            }
        }
        "compact" if agent.user_prompts().is_empty() => app.notice("Nothing to compact yet."),
        "compact" => {
            let focus = arg.to_string();
            app.begin_busy();
            let ended = run_turn(app, keys, rx, async { agent.compact(Some(&focus)).await.map(|()| (String::new(), Stop::Completed)) }).await?;
            app.end_busy();
            match ended {
                Ended::Done(Err(e)) => app.error_from("Compaction failed", &e),
                Ended::Done(Ok(_)) => app.status.context = agent.stats.context,
                _ => {}
            }
        }
        "clear" | "new" => match session.start_on(agent.client(), agent.controls.clone(), agent_sink(agent)) {
            Ok(fresh) => {
                app.reset_conversation(snapshots, cwd);
                let saved = !agent.user_prompts().is_empty();
                let stopped = agent.stop_tasks();
                agent.discard_if_unused();
                *agent = fresh;
                app.show_model(agent, session.config());
                app.wipe_screen();
                app.banner(agent, session, snapshots, false);
                app.notice(if saved { "Started a new session. The last one is saved; /resume picks it up again." } else { "Started a new session." });
                if let Some(note) = stopped_note(&stopped) {
                    app.notice(&note);
                }
            }
            Err(e) => app.error_report(&e),
        },
        "resume" => app.open_session_picker(cwd),
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
            if ["brave", "tavily", "searxng"].contains(&provider.as_str()) {
                app.search = Some(SearchFlow { provider: None, docker: None, url: None, current: session.config().search.clone() });
                app.search_pick(&provider);
            } else {
                app.error("Pick brave, tavily or searxng; /web-search-config alone lists them.");
            }
        }
        "model" if arg.is_empty() => app.open_model_picker(session.config(), false),
        "model" => app.next_input = Some(Input::Model(arg.to_string())),
        "backup-model" if arg.is_empty() => app.open_model_picker(session.config(), true),
        "backup-model" => app.next_input = Some(Input::BackupModel(arg.to_string())),
        "effort" | "thinking" if arg.is_empty() => app.open_effort_picker(session.config()),
        "effort" | "thinking" => app.next_input = Some(Input::Effort(arg.to_lowercase())),
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
                prompt_turn(app, agent, snapshots, keys, rx, format!("/{other} {arg}").trim_end(), &sent, images).await?;
            }
            None => app.error(&format!("Unknown command /{other}. Type /help for the list.")),
        },
    }
    Ok(true)
}

/// Runs one turn started by a command: `shown` goes in the transcript, `prompt` to the agent,
/// with the images pasted beside the command.
#[allow(clippy::too_many_arguments)]
pub(super) async fn prompt_turn(
    app: &mut App,
    agent: &mut Agent,
    snapshots: &mut Snapshots,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    shown: &str,
    prompt: &str,
    images: &[Image],
) -> Result<()> {
    app.begin_turn(shown);
    app.draw()?;
    app.undo_warned = false;
    app.peer_tells = 0;
    app.live_turn(Some(shown));
    let before = Totals::of(agent);
    // The snapshot's git commands run on a blocking thread while the turn's first request
    // is already in flight; the agent waits for the tree before its first tool call, so the
    // tree still predates every change the turn makes.
    let ended = if let Some(work) = snapshots.start_take() {
        app.snapshotting = true;
        let (snap_tx, snap_rx) = oneshot::channel();
        agent.set_snapshot_wait(snap_rx);
        let turn = run_turn(app, keys, rx, agent.run(prompt, images));
        let snap = async {
            let tree = tokio::task::spawn_blocking(move || work.run())
                .await
                .map_err(|e| anyhow::anyhow!("the snapshot task ended: {e}"))
                .and_then(|tree| tree);
            snapshots.commit_take(tree);
            let _ = snap_tx.send(());
        };
        let (ended, _) = tokio::join!(turn, snap);
        app.snapshotting = false;
        ended?
    } else {
        run_turn(app, keys, rx, agent.run(prompt, images)).await?
    };
    if let Some(why) = &snapshots.skipped {
        app.notice(why);
    }
    let completed = matches!(ended, Ended::Done(Ok((_, Stop::Completed))));
    let after = Totals::of(agent);
    app.finish_turn(agent, ended, before, after);
    app.live_turn(None);
    if !app.controls.peer_notes.borrow().is_empty() {
        app.notice("A message from another session came as the turn ended; the agent gets it with your next prompt.");
    }
    if matches!(app.last_end, Some(LastEnd::Interrupted)) {
        agent.recover()?;
    }
    if wants_recap(app.auto_recap, completed, Totals::of(agent).tool_calls - before.tool_calls) {
        app.recap = Some(Box::pin(app.controls.aside.borrow().recap()));
    }
    Ok(())
}

/// Whether a finished turn gets a recap by itself: a turn that only answered is its own recap.
pub(super) fn wants_recap(on: bool, completed: bool, tool_calls: u32) -> bool {
    on && completed && tool_calls > 0
}

fn agent_sink(agent: &Agent) -> Sink {
    agent.sink()
}
