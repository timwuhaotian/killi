//! A group of subagents run at most `limit` at once, as a value of its own: it holds no
//! borrow of the agent that started it, so the group can finish inside that agent's tool call
//! or be handed, part or whole, to the background.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use super::*;

use crate::agent::background::Steer;

/// A subagent ready to run: the agent, the text it is sent, and where its events go.
pub(in crate::agent) struct Child {
    pub(in crate::agent) agent: Agent,
    pub(in crate::agent) sent: String,
    pub(in crate::agent) route: Route,
}

/// A subagent ready to run, or the result of a call that couldn't start one.
pub(in crate::agent) type Job = std::result::Result<Child, tools::Output>;

/// How one subagent ended: its report, its counts, and the provider limit that ended it when
/// one did. `stats` is `None` for a subagent that never started.
pub(in crate::agent) struct ChildDone {
    pub(in crate::agent) index: usize,
    pub(in crate::agent) output: tools::Output,
    pub(in crate::agent) stats: Option<Stats>,
    pub(in crate::agent) limited: Option<ErrorKind>,
}

/// How a group of subagents ended: every report in order, and the items a provider limit kept
/// from ever starting.
pub(in crate::agent) struct Children {
    pub(in crate::agent) outputs: Vec<tools::Output>,
    pub(in crate::agent) unrun: Vec<usize>,
}

/// A running subagent's future with its place in the group.
pub(in crate::agent) struct Tagged {
    pub(in crate::agent) index: usize,
    fut: Pin<Box<dyn Future<Output = ChildDone>>>,
}

impl Future for Tagged {
    type Output = ChildDone;

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<ChildDone> {
        self.fut.as_mut().poll(cx)
    }
}

/// What a subagent of the group is when the group is split: started, or still waiting.
pub(in crate::agent) enum Unfinished {
    Started(Tagged),
    Waiting(Box<Job>),
}

/// What stays known about a member once its agent moved into its future: where its events
/// go, and its message queue for `agent_message`.
pub(in crate::agent) struct Meta {
    pub(in crate::agent) route: Route,
    pub(in crate::agent) steer: Rc<RefCell<VecDeque<Steered>>>,
    /// Its log file's name in the session folder.
    pub(in crate::agent) log: String,
    /// What it is sent first.
    pub(in crate::agent) sent: String,
}

pub(in crate::agent) struct Crew {
    pending: VecDeque<(usize, Job)>,
    running: FuturesUnordered<Tagged>,
    limit: usize,
    limited: Option<(usize, ErrorKind)>,
    outputs: Vec<Option<tools::Output>>,
    pub(in crate::agent) meta: Vec<Option<Meta>>,
    /// The context of the agent that started the group: a member that ends leaves what it
    /// still runs to it.
    parent: Rc<tools::Context>,
}

impl Crew {
    /// Starts the first `limit` of `jobs`.
    pub(in crate::agent) fn new(jobs: Vec<Job>, limit: usize, parent: Rc<tools::Context>) -> Self {
        let meta = jobs
            .iter()
            .map(|job| {
                job.as_ref().ok().map(|child| Meta {
                    route: child.route.clone(),
                    steer: child.agent.controls.steer.clone(),
                    log: log_name(&child.agent),
                    sent: child.sent.clone(),
                })
            })
            .collect();
        let mut crew = Self {
            outputs: (0..jobs.len()).map(|_| None).collect(),
            pending: jobs.into_iter().enumerate().collect(),
            running: FuturesUnordered::new(),
            limit: limit.max(1),
            limited: None,
            meta,
            parent,
        };
        while crew.running.len() < crew.limit && crew.start_next() {}
        crew
    }

    fn start_next(&mut self) -> bool {
        let Some((index, job)) = self.pending.pop_front() else {
            return false;
        };
        self.running.push(tag(index, job, self.parent.clone()));
        true
    }

    /// The next member to end, or none once every started one has. A member the provider
    /// rate-limits or whose quota is spent stops the queue: the ones after it would fail the
    /// same way.
    pub(in crate::agent) async fn next(&mut self) -> Option<ChildDone> {
        let done = self.running.next().await?;
        if self.limited.is_none()
            && let Some(kind) = done.limited
        {
            self.limited = Some((done.index, kind));
        }
        if self.limited.is_none() {
            self.start_next();
        }
        Some(done)
    }

    /// Members that ended so far.
    pub(in crate::agent) fn done(&self) -> usize {
        self.outputs
            .iter()
            .filter(|output| output.is_some())
            .count()
    }

    /// Keeps member `index`'s result.
    pub(in crate::agent) fn store(&mut self, index: usize, output: tools::Output) {
        if let Some(slot) = self.outputs.get_mut(index) {
            *slot = Some(output);
        }
    }

    /// Every result in order; members a provider limit kept from starting say so.
    pub(in crate::agent) fn finish(self) -> Children {
        let mut outputs = self.outputs;
        let mut unrun = Vec::new();
        if let Some((hit, kind)) = self.limited {
            let note = not_run_note(hit, kind);
            for (index, slot) in outputs.iter_mut().enumerate() {
                if slot.is_none() {
                    unrun.push(index);
                    *slot = Some(tools::Output::error(note.clone()));
                }
            }
        }
        Children {
            outputs: outputs
                .into_iter()
                .map(|output| output.unwrap_or_else(|| tools::Output::error("No result.")))
                .collect(),
            unrun,
        }
    }

    /// The results so far, and every member not finished: started ones with their futures,
    /// the rest still waiting to start.
    pub(in crate::agent) fn split(self) -> (Vec<Option<tools::Output>>, Vec<(usize, Unfinished)>) {
        let mut unfinished: Vec<(usize, Unfinished)> = self
            .running
            .into_iter()
            .map(|tagged| (tagged.index, Unfinished::Started(tagged)))
            .collect();
        unfinished.extend(
            self.pending
                .into_iter()
                .map(|(index, job)| (index, Unfinished::Waiting(Box::new(job)))),
        );
        unfinished.sort_by_key(|(index, _)| *index);
        (self.outputs, unfinished)
    }

    /// The message queues of the members not ended yet, for `agent_message` to the group.
    pub(in crate::agent) fn live_steers(&self) -> Vec<Steer> {
        let running = self.running.iter().map(|tagged| tagged.index);
        let waiting = self.pending.iter().map(|(index, _)| *index);
        running
            .chain(waiting)
            .filter_map(|index| {
                self.meta
                    .get(index)
                    .and_then(Option::as_ref)
                    .map(|meta| meta.steer.clone())
            })
            .collect()
    }
}

/// `job` as a running future with its index.
pub(in crate::agent) fn tag(index: usize, job: Job, parent: Rc<tools::Context>) -> Tagged {
    Tagged {
        index,
        fut: Box::pin(run_child(index, job, parent)),
    }
}

/// Adds `stats` to `total`.
pub(in crate::agent) fn add_stats(total: &mut Stats, stats: &Stats) {
    total.usage.prompt += stats.usage.prompt;
    total.usage.cached += stats.usage.cached;
    total.usage.completion += stats.usage.completion;
    total.usage.reasoning += stats.usage.reasoning;
    total.requests += stats.requests;
    total.tool_calls += stats.tool_calls;
}

/// Runs one subagent to its report. What it still runs when it ends moves to `parent`, and
/// the report says under which ids.
async fn run_child(index: usize, job: Job, parent: Rc<tools::Context>) -> ChildDone {
    let Child {
        mut agent, sent, ..
    } = match job {
        Ok(child) => child,
        Err(output) => {
            return ChildDone {
                index,
                output,
                stats: None,
                limited: None,
            };
        }
    };
    (agent.sink)(Event::Started);
    let (mut output, limited) = match agent.run(&sent, &[]).await {
        Ok((text, Stop::MaxTokens)) if !text.trim().is_empty() => (
            tools::Output::error(format!(
                "The subagent's report was truncated (MaxTokens). Partial report:\n{text}"
            )),
            None,
        ),
        Ok((text, stop)) if !text.trim().is_empty() && stop != Stop::RepeatBreaker => {
            (tools::Output::ok(text), None)
        }
        Ok((_, stop)) => (
            tools::Output::error(format!(
                "The subagent stopped without a final report ({stop:?})."
            )),
            None,
        ),
        Err(e) => (
            tools::Output::error(format!("The subagent failed: {e:#}")),
            provider_limit(&e),
        ),
    };
    if !Rc::ptr_eq(&agent.ctx, &parent) {
        let adopted = parent
            .tasks
            .borrow_mut()
            .adopt(&mut agent.ctx.tasks.borrow_mut());
        if !adopted.is_empty() {
            let list: Vec<String> = adopted
                .iter()
                .map(|(id, command)| format!("task {id} `{command}`"))
                .collect();
            output.text.push_str(&format!(
                "\n[It left running, now your background tasks: {}. task_output reads them, task_stop ends them.]",
                list.join(", ")
            ));
        }
    }
    (agent.sink)(Event::Finished {
        is_error: output.is_error,
    });
    // Only its counts outlive it: its conversation and log file are freed here, not when the
    // slowest sibling of a swarm finishes.
    ChildDone {
        index,
        output,
        stats: Some(std::mem::take(&mut agent.stats)),
        limited,
    }
}
