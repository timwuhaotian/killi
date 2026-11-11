use super::*;

/// `/pair ...` while the agent is idle (while one drives, `run_local` stops it or says where
/// it stands): bare starts the setup, `mentor|executor` reconfigures a role, `stop` ends
/// the pair, `resume` drives on, and empty or `status` says where it stands.
#[allow(clippy::too_many_arguments)]
pub(super) async fn pair_command(
    app: &mut App,
    agent: &mut Agent,
    session: &mut Setup,
    snapshots: &mut Snapshots,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    arg: &str,
) -> Result<()> {
    let (verb, _) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
    match verb {
        "" => {
            if agent.pair_active() {
                pair_status(app, agent);
            } else if app.pair_setup.is_some() {
                app.pair_setup = None;
                app.notice("Pair setup cancelled.");
            } else {
                pair_setup_start(app, session.config(), None);
            }
        }
        "status" => pair_status(app, agent),
        "stop" => {
            if agent.pair_active() {
                agent.pair_stop();
                app.pair_ctl = None;
                pair_status(app, agent);
            } else if agent.pair_outcome().is_some() {
                app.notice("The pair already ended; /pair starts another.");
            } else {
                app.notice("No pair is running.");
            }
        }
        "resume" => {
            if agent.pair_active() {
                app.pair_ctl = agent.pair_ctl();
                pair_turn(app, agent, snapshots, keys, rx, None, false).await?;
            } else if agent.pair_outcome().is_some() {
                app.notice("The pair already ended; /pair starts another.");
            } else {
                app.notice("No pair is running.");
            }
        }
        "mentor" | "executor" => {
            if agent.pair_active() {
                app.error("The pair keeps its models while it runs; /pair stop ends it first.");
            } else {
                let role = if verb == "mentor" {
                    Role::Mentor
                } else {
                    Role::Executor
                };
                pair_setup_start(app, session.config(), Some(role));
            }
        }
        _ => app.error("Use /pair to start one, or /pair mentor|executor|stop|resume|status."),
    }
    Ok(())
}

/// Where the pair stands, or that none does.
fn pair_status(app: &mut App, agent: &Agent) {
    match agent.pair_status() {
        Some(status) => {
            app.notice(&status);
            if agent.pair_active() {
                app.notice("Type to interject, /pair resume to continue, /pair stop to end.");
            }
        }
        None => app.notice("No pair is running."),
    }
}

/// Starts the model pickers: both roles for a new pair, one role for
/// `/pair mentor|executor`. Each pick opens the next picker; the last one waits for the
/// task, or saves the role when only reconfiguring.
fn pair_setup_start(app: &mut App, config: &Config, config_only: Option<Role>) {
    let role = config_only.unwrap_or(Role::Mentor);
    let remembered = app.pair_cfg.clone();
    let current = app.status.model.clone();
    let model = match role {
        Role::Mentor => pair_default_model(&remembered.mentor_model, &current),
        Role::Executor => pair_default_model(&remembered.executor_model, &current),
    };
    let step = match role {
        Role::Mentor => PairStep::MentorModel,
        Role::Executor => PairStep::ExecutorModel,
    };
    app.pair_setup = Some(PairSetup {
        step,
        mentor_model: remembered.mentor_model,
        mentor_effort: remembered.mentor_effort,
        executor_model: remembered.executor_model,
        executor_effort: remembered.executor_effort,
        config_only,
    });
    app.open_pair_model_picker(config, role, &model);
}

/// The remembered model, or the session's own for a first pair.
fn pair_default_model(remembered: &str, current: &str) -> String {
    if remembered.is_empty() {
        current.to_string()
    } else {
        remembered.to_string()
    }
}

/// The remembered effort when the model still offers it, else the session's own, else the
/// middle of what the model offers.
fn pair_default_effort(config: &Config, alias: &str, remembered: &str, current: &str) -> String {
    let efforts = config.efforts(alias);
    for effort in [remembered, current] {
        if efforts.iter().any(|offered| offered == effort) {
            return effort.to_string();
        }
    }
    efforts.get(efforts.len() / 2).cloned().unwrap_or_default()
}

/// A /pair setup's pick: stores it and opens the next picker, or finishes the setup.
/// A pick for another step is stale — the setup moved on — and is dropped.
pub(super) fn pair_pick(app: &mut App, config: &Config, role: Role, pick: &str, effort: bool) {
    enum Next {
        Effort(Role, String, String),
        Model(Role, String),
        Task,
        Saved(Role),
    }
    let next = (|| {
        let setup = app.pair_setup.as_mut()?;
        let expected = matches!(
            (setup.step, role, effort),
            (PairStep::MentorModel, Role::Mentor, false)
                | (PairStep::MentorEffort, Role::Mentor, true)
                | (PairStep::ExecutorModel, Role::Executor, false)
                | (PairStep::ExecutorEffort, Role::Executor, true)
        );
        if !expected {
            return None;
        }
        Some(match (role, effort) {
            (Role::Mentor, false) => {
                setup.mentor_model = pick.to_string();
                setup.step = PairStep::MentorEffort;
                Next::Effort(role, pick.to_string(), setup.mentor_effort.clone())
            }
            (Role::Mentor, true) => {
                setup.mentor_effort = pick.to_string();
                if setup.config_only == Some(Role::Mentor) {
                    Next::Saved(role)
                } else {
                    setup.step = PairStep::ExecutorModel;
                    Next::Model(Role::Executor, setup.executor_model.clone())
                }
            }
            (Role::Executor, false) => {
                setup.executor_model = pick.to_string();
                setup.step = PairStep::ExecutorEffort;
                Next::Effort(role, pick.to_string(), setup.executor_effort.clone())
            }
            (Role::Executor, true) => {
                setup.executor_effort = pick.to_string();
                if setup.config_only == Some(Role::Executor) {
                    Next::Saved(role)
                } else {
                    setup.step = PairStep::Task;
                    Next::Task
                }
            }
        })
    })();
    match next {
        None => {}
        Some(Next::Effort(role, alias, current)) => {
            let current = pair_default_effort(config, &alias, &current, &app.status.thinking);
            app.open_pair_effort_picker(config, role, &alias, &current);
        }
        Some(Next::Model(role, remembered)) => {
            let current = pair_default_model(&remembered, &app.status.model);
            app.open_pair_model_picker(config, role, &current);
        }
        Some(Next::Task) => {
            if let Some(setup) = app.pair_setup.as_ref() {
                app.pair_cfg = PairConfig {
                    mentor_model: setup.mentor_model.clone(),
                    mentor_effort: setup.mentor_effort.clone(),
                    executor_model: setup.executor_model.clone(),
                    executor_effort: setup.executor_effort.clone(),
                };
            }
            let cfg = app.pair_cfg.clone();
            app.notice(&format!(
                "Pair ready: mentor {} ({}) + executor {} ({}). Type the task and press Enter. (/pair cancels.)",
                cfg.mentor_model, cfg.mentor_effort, cfg.executor_model, cfg.executor_effort,
            ));
        }
        Some(Next::Saved(role)) => {
            if let Some(setup) = app.pair_setup.take() {
                app.pair_cfg = PairConfig {
                    mentor_model: setup.mentor_model,
                    mentor_effort: setup.mentor_effort,
                    executor_model: setup.executor_model,
                    executor_effort: setup.executor_effort,
                };
            }
            let cfg = app.pair_cfg.clone();
            let (model, effort) = match role {
                Role::Mentor => (cfg.mentor_model, cfg.mentor_effort),
                Role::Executor => (cfg.executor_model, cfg.executor_effort),
            };
            app.notice(&format!(
                "Pair {what} is {model} ({effort}) for the next pair.",
                what = role.label().to_lowercase(),
            ));
        }
    }
}

/// Starts the pair the setup readied on the typed task.
#[allow(clippy::too_many_arguments)]
pub(super) async fn pair_kickoff(
    app: &mut App,
    agent: &mut Agent,
    session: &Setup,
    snapshots: &mut Snapshots,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    shown: &str,
    sent: &str,
    images: &[Image],
) -> Result<()> {
    let cfg = app.pair_cfg.clone();
    let client = |alias: &str, effort: &str| {
        session
            .config()
            .resolve(Some(alias))
            .and_then(Client::new)
            .map(|mut client| {
                client.model.thinking = Thinking::parse(effort);
                client
            })
    };
    let (mentor, executor) = match (
        client(&cfg.mentor_model, &cfg.mentor_effort),
        client(&cfg.executor_model, &cfg.executor_effort),
    ) {
        (Ok(mentor), Ok(executor)) => (mentor, executor),
        (Err(e), _) | (_, Err(e)) => {
            app.pair_setup = None;
            app.error_report(&e);
            return Ok(());
        }
    };
    if let Err(e) = agent.pair_begin(&cfg, mentor, executor, sent, images.to_vec()) {
        app.pair_setup = None;
        app.error_report(&e);
        return Ok(());
    }
    app.pair_setup = None;
    app.pair_ctl = agent.pair_ctl();
    pair_turn(
        app,
        agent,
        snapshots,
        keys,
        rx,
        Some((shown.to_string(), images.to_vec())),
        true,
    )
    .await
}

/// Drives the pair: `run_turn` carries each stretch, Esc pauses it, a typed message hands a
/// replan to the mentor and drives on, and /pair stop ends it. `shown` holds the kickoff or
/// the interjection a resume answers; `snapshot` takes the /undo tree once, at kickoff.
#[allow(clippy::too_many_arguments)]
pub(super) async fn pair_turn(
    app: &mut App,
    agent: &mut Agent,
    snapshots: &mut Snapshots,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    shown: Option<(String, Vec<Image>)>,
    snapshot: bool,
) -> Result<()> {
    let live = match &shown {
        Some((text, _)) => format!("/pair {text}"),
        None => "/pair resume".to_string(),
    };
    match &shown {
        Some((text, images)) => app.begin_turn(text, images),
        None => app.begin_busy(),
    }
    app.draw()?;
    app.undo_warned = false;
    app.live_turn(Some(&live));
    let before = agent.pair_split();
    if snapshot && let Some(work) = snapshots.start_take() {
        // Sequential, before either role runs: the tree predates every change the pair makes.
        let tree = tokio::task::spawn_blocking(move || work.run())
            .await
            .map_err(|e| anyhow::anyhow!("the snapshot task ended: {e}"))
            .and_then(|tree| tree);
        snapshots.commit_take(tree);
        if let Some(why) = &snapshots.skipped {
            app.notice(why);
        }
    }
    loop {
        app.pair_driving = true;
        let ended = run_turn(app, keys, rx, agent.pair_drive()).await;
        app.pair_driving = false;
        let ended = ended?;
        agent.pair_recover()?;
        let stop = app.pair_ctl.as_ref().is_some_and(|ctl| ctl.stop.take());
        if stop {
            agent.pair_stop();
            break;
        }
        if agent.pair_interject() {
            continue;
        }
        match ended {
            Ended::Interrupted => {
                app.notice(
                    "Pair paused. Type to interject, /pair resume to continue, /pair stop to end.",
                );
                break;
            }
            Ended::Done(Ok(_)) if agent.pair_outcome().is_none() => {
                app.notice(
                    "Pair paused. Type to interject, /pair resume to continue, /pair stop to end.",
                );
                break;
            }
            Ended::Done(Ok(_)) => break,
            Ended::Done(Err(e)) => {
                app.error_report(&e);
                app.notice(
                    "The pair is paused where it failed; /pair resume retries, /pair stop ends it.",
                );
                break;
            }
        }
    }
    let after = agent.pair_split();
    let elapsed = app.busy.map(|t| t.elapsed()).unwrap_or_default();
    app.release_prompt();
    app.end_busy();
    if let Some(outcome) = agent.pair_outcome() {
        if let Some(status) = agent.pair_status() {
            app.notice(&status);
        }
        if outcome == Outcome::Budget {
            app.notice(&format!(
                "The pair hit its {MAX_ROUNDS}-round budget without the mentor's {DONE_MARK}; /pair starts another.",
            ));
        }
        app.pair_ctl = None;
    }
    // Nothing queues for the session's own agent while a pair drives: typed messages and
    // skills interject to the mentor instead, so the queue and the steer box stay empty.
    if let (Some(before), Some(after)) = (before, after) {
        let total = |counts: &[RoleCounts; 2]| {
            (
                counts[0].requests + counts[1].requests,
                counts[0].prompt + counts[1].prompt,
                counts[0].cached + counts[1].cached,
                counts[0].completion + counts[1].completion,
            )
        };
        let (requests, prompt, cached, completion) = total(&after);
        let (requests_before, prompt_before, cached_before, completion_before) = total(&before);
        let requests = requests.saturating_sub(requests_before);
        let prompt = prompt.saturating_sub(prompt_before);
        let cached = cached.saturating_sub(cached_before) * 100 / prompt.max(1);
        let completion = completion.saturating_sub(completion_before);
        if prompt > 0 || completion > 0 {
            app.commit(String::new());
            app.commit(format!(
                "{DIM}{} · {requests} request{} · {} in · {cached}% cached · {} out{RESET}",
                clock(elapsed),
                if requests == 1 { "" } else { "s" },
                short(prompt),
                short(completion)
            ));
        }
    }
    app.last_kind = Kind::None;
    app.live_turn(None);
    Ok(())
}
