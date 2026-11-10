use super::*;

/// A fresh agent for a task another session sent: the session's model, tools, system prompt and
/// settings, with no history and no user to ask. Its conversation lives in a log of its own in
/// the session's folder, `task-N.jsonl` from `number` on, and it reports with the subagent's
/// status convention. Built from `Aside`, the same seam a side question uses, so it never
/// borrows the session's own agent.
pub(crate) fn task_agent(
    aside: &Aside,
    here: Option<Rc<live::Here>>,
    sink: Sink,
    number: usize,
) -> Result<Agent> {
    let Some(client) = aside.client.clone() else {
        bail!("this session hasn't asked its model anything yet, so it can't take a task");
    };
    let Some(settings) = aside.settings.clone() else {
        bail!("this session hasn't loaded its settings yet, so it can't take a task");
    };
    let Some(ctx) = aside.ctx.upgrade() else {
        bail!("this session's tools are gone, so it can't take a task");
    };
    // The system prompt is the history's first message, as the session sent it, so a task's
    // requests hit the same cached prefix as its own.
    let system = aside
        .messages
        .upgrade()
        .and_then(|history| history.first().cloned())
        .and_then(|message| message["content"].as_str().map(str::to_string))
        .unwrap_or_default();
    // The session's folder is where its tools spill their output, and where its subagents log.
    let session = Session::task(&ctx.spill_dir, number)?;
    let controls = Controls {
        steer: Rc::default(),
        // A task was sent to be carried out, so it never starts in plan mode.
        plan: Rc::default(),
        swarm: Rc::default(),
        goal: Rc::default(),
        interactive: false,
        aside: Rc::default(),
        // Where this session works, so a task's git commands are refused while another session
        // works in the same tree.
        here,
        peer_notes: Rc::default(),
    };
    let mut agent = Agent::begin(
        (*client).clone(),
        &ctx.cwd,
        &system,
        (*aside.tools).clone(),
        (*settings).clone(),
        controls,
        sink,
        session,
    )?;
    // A task the provider rate-limits finishes on the backup, like a subagent of this session.
    agent.backup = aside.backup.clone();
    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_agent_needs_a_conversation_to_copy() {
        let aside = Aside::default();
        let why = match task_agent(&aside, None, Rc::new(|_| {}), 1) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("no client, no task agent"),
        };
        assert!(why.contains("hasn't asked its model"), "{why}");
    }
}
