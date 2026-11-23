//! Takes up the background work a resumed session's log left open. Each subagent goes on from
//! its own log, where its whole conversation is, with a note that the session stopped under
//! it; one whose log already ends on its report just delivers it.

use super::*;

use crate::agent::subagent::{Child, Crew, Job, add_stats, tag};
use crate::session::BgRecord;

/// Told to a subagent that goes on after its session stopped and resumed.
const RESUMED: &str = "<system-reminder>The session you work for stopped while you were working and has now resumed. Your last tool calls may not have finished: check the state of what you were doing, then finish your task and end with your report as you were asked.</system-reminder>";

/// A paused subagent, as its log left it.
enum Paused {
    /// Its log ends on its report.
    Reported(tools::Output),
    /// It goes on from where it stopped.
    Child(Box<Child>),
}

impl Agent {
    /// Continues the background work this session's log left open, under the ids the model
    /// knows, and tells the model at its next prompt. Gives how many tasks went on.
    pub fn resume_background(&mut self) -> usize {
        if !self.background_ready() || self.paused.is_empty() {
            return 0;
        }
        let paused = std::mem::take(&mut self.paused);
        let mut names = Vec::new();
        for (id, record) in paused {
            names.push(format!("task {id} (“{}”)", record.label));
            self.resume_task(id, record);
        }
        self.note_for_next_prompt(format!(
            "Background work you started was paused when this session stopped and goes on now: {}. Each report arrives as a reminder when it ends.",
            names.join(", ")
        ));
        names.len()
    }

    fn resume_task(&mut self, id: u32, record: BgRecord) {
        let swarm = !record.items.is_empty();
        let mut jobs: Vec<Job> = Vec::new();
        let mut reported = 0;
        for (n, log) in record.logs.iter().enumerate() {
            let label = if swarm {
                format!("{} #{}", record.label, n + 1)
            } else {
                record.label.clone()
            };
            let sent = record.sent.get(n).map(String::as_str).unwrap_or_default();
            match self.paused_child(log, &label, sent) {
                Ok(Paused::Child(child)) => jobs.push(Ok(*child)),
                Ok(Paused::Reported(output)) => {
                    reported += 1;
                    jobs.push(Err(output));
                }
                Err(e) => {
                    reported += 1;
                    jobs.push(Err(tools::Output::error(format!(
                        "Could not go on after the session resumed: {e:#}"
                    ))));
                }
            }
        }
        let routes: Vec<(Route, Steer)> = jobs
            .iter()
            .filter_map(|job| {
                job.as_ref().ok().map(|child| {
                    (child.route.clone(), child.agent.controls.steer.clone())
                })
            })
            .collect();
        let steers = routes.iter().map(|(_, steer)| steer.clone()).collect();
        let ctx = self.ctx.clone();
        let work: BgWork = if swarm {
            let limit = self.swarm_parallel.get();
            let (items, logs) = (record.items.clone(), record.logs.clone());
            Box::pin(async move {
                let mut crew = Crew::new(jobs, limit, ctx.clone());
                let mut stats = Stats::default();
                while let Some(done) = crew.next().await {
                    if let Some(used) = &done.stats {
                        add_stats(&mut stats, used);
                    }
                    crew.store(done.index, done.output);
                }
                let children = crew.finish();
                let report =
                    swarm::report(&ctx, &items, &children.outputs, &logs, MAX_RESULT_BYTES);
                (report, stats)
            })
        } else {
            let job = jobs
                .into_iter()
                .next()
                .unwrap_or_else(|| Err(tools::Output::error("Its log is gone.")));
            Box::pin(async move {
                let done = tag(0, job, ctx).await;
                (done.output, done.stats.unwrap_or_default())
            })
        };
        self.launch_as(
            id,
            BgStart {
                label: record.label.clone(),
                members: record.logs.len().max(1),
                logs: record.logs.clone(),
                sent: record.sent.clone(),
                items: record.items.clone(),
                steers,
                started: false,
                since: Instant::now(),
            },
            work,
        );
        if let Some(task) = self.controls.background.borrow_mut().tasks.get_mut(&id) {
            task.finished_members = reported;
        }
        for (route, steer) in routes {
            route.detach(&self.controls.background, id, Some(steer));
        }
    }

    /// The subagent whose log is `log` in this session's folder, ready to go on: sent what
    /// it was first sent if it never got that, else told the session resumed.
    fn paused_child(&self, log: &str, label: &str, sent: &str) -> Result<Paused> {
        if log.is_empty() {
            bail!("it never started");
        }
        let (session, report) = Session::resume(&self.session.dir.join(log))?;
        let context = report.loaded.context;
        let messages = report.loaded.messages;
        let got = !sent.is_empty()
            && messages.iter().any(|message| {
                message["role"] == "user"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.ends_with(sent))
            });
        if got
            && let Some(last) = messages.last()
            && last["role"] == "assistant"
            && last["tool_calls"].as_array().is_none_or(Vec::is_empty)
            && let Some(text) = last["content"].as_str().filter(|t| !t.trim().is_empty())
        {
            return Ok(Paused::Reported(tools::Output::ok(text)));
        }
        let route = Route::default();
        let len = messages.len();
        let mut agent = self.child_agent(
            session,
            Rc::new(messages),
            label,
            None,
            &route,
            context,
            len,
        );
        // Calls it made as the session stopped get their results first, or the note below
        // would wait behind them. Those results read as interrupted; the note right after says
        // why, so the interruption's own note, which speaks of the user, stays unsaid.
        agent.recover()?;
        agent.interrupted = false;
        Ok(Paused::Child(Box::new(Child {
            agent,
            sent: if got {
                RESUMED.to_string()
            } else {
                sent.to_string()
            },
            route,
        })))
    }
}
