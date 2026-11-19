//! The terminal's side of background subagents: it polls their work while it waits for keys
//! and while a turn runs, shows one live row for each, prints a line when one ends, and starts
//! a turn by itself when a report arrives while the session sits idle.

use super::*;
use crate::agent::{BgFuture, BgState, BgTask, MAX_BACKGROUND};
use picker::PickerChoice;

/// Turns started by background reports between two prompts the user typed. A report that
/// lands after that waits for the next prompt, so subagents that keep dispatching more work
/// can't keep the session running on its own.
pub(super) const MAX_WAKES: usize = 10;

/// Background tasks shown in the live area; the rest are counted.
const SHOWN: usize = 4;

/// The next piece of background work to poll; never, for a session that takes none.
pub(super) async fn next_work(
    rx: &mut Option<mpsc::UnboundedReceiver<BgFuture>>,
) -> Option<BgFuture> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

impl App {
    /// Background tasks not ended yet.
    pub(super) fn background_working(&self) -> usize {
        self.controls.background.borrow().working()
    }

    /// Whether the idle loop should wake each second: to move the live rows' clocks, and to
    /// start a turn once a report waits and the prompt is free.
    pub(super) fn background_ticking(&self) -> bool {
        let registry = self.controls.background.borrow();
        registry.working() > 0 || (registry.undelivered() > 0 && self.wakes < MAX_WAKES)
    }

    /// Starts polling a piece of background work.
    pub(super) fn on_background_work(&mut self, work: BgFuture) {
        self.bg_running.push(work);
        self.live_tasks();
    }

    /// A background task ended: one line in the conversation, and this session's file says
    /// how many still work.
    pub(super) fn on_background_done(&mut self, id: u32) {
        self.live_tasks();
        let line = {
            let registry = self.controls.background.borrow();
            let Some(task) = registry.tasks.get(&id) else {
                return;
            };
            let took = clock(task.started.elapsed());
            let label = clip(&text::sanitize_row(&task.label), 48);
            match (task.state, &task.report) {
                (BgState::Done, Some((_, false))) => {
                    format!("{LAGOON}✓{RESET} {DIM}task {id} · {label} · finished in {took}{RESET}")
                }
                (BgState::Done, _) => {
                    format!("{WARN}✗{RESET} {DIM}task {id} · {label} · failed after {took}{RESET}")
                }
                _ => format!(
                    "{FAINT}■{RESET} {DIM}task {id} · {label} · stopped after {took}{RESET}"
                ),
            }
        };
        // Mid-paragraph, it waits with the answered asides for the paragraph to end.
        if self.busy.is_some() && self.last_kind == Kind::Text && !self.held_blank {
            self.held_asides.push(Line::Fit(line));
        } else {
            self.begin(Kind::Tool);
            self.emit(Line::Fit(line));
        }
        if self.busy.is_none() && self.wakes >= MAX_WAKES {
            let waiting = self.controls.background.borrow().undelivered();
            if waiting > 0 {
                self.notice(&format!(
                    "{} waiting: background work started {MAX_WAKES} turns since your last message, so the agent reads {} with your next one.",
                    tools_count(waiting, "report"),
                    if waiting == 1 { "it" } else { "them" }
                ));
            }
        }
    }

    /// A turn to start by itself: a report waits, the prompt is free, and fewer than
    /// `MAX_WAKES` such turns ran since the user's last message.
    pub(super) fn take_wake(&mut self) -> Option<Input> {
        let waiting = self.controls.background.borrow().undelivered() > 0;
        if !(waiting && self.wakes < MAX_WAKES && self.schedule_ready()) {
            return None;
        }
        self.recap = None;
        Some(Input::Wake)
    }

    /// The live rows: one per background task still working, oldest first.
    pub(super) fn background_rows(&self, width: usize) -> Vec<String> {
        let registry = self.controls.background.borrow();
        let working: Vec<(&u32, &BgTask)> = registry
            .tasks
            .iter()
            .filter(|(_, task)| !task.ended())
            .collect();
        let mut rows = Vec::new();
        for (id, task) in working.iter().take(SHOWN) {
            let label = clip(&text::sanitize_row(&task.label), 40);
            let row = match task.state {
                BgState::Queued => format!(
                    "  {FAINT}○{RESET} {DIM}task {id} · {label} · waiting for a slot ({MAX_BACKGROUND} run at once){RESET}"
                ),
                _ => {
                    let frame = QUARTER[self.spinner % QUARTER.len()];
                    let members = if task.members > 1 {
                        format!(" · {}/{} done", task.finished_members, task.members)
                    } else {
                        String::new()
                    };
                    let last = task
                        .last
                        .as_deref()
                        .map(|last| format!(" · {}", gist(last)))
                        .unwrap_or_default();
                    format!(
                        "  {LAGOON}{frame}{RESET} {DIM}task {id} · {label} · {}{members} · {}{last}{RESET}",
                        clock(task.started.elapsed()),
                        tools_count(task.calls as usize, "call")
                    )
                }
            };
            rows.push(fit(&row, width));
        }
        if working.len() > SHOWN {
            rows.push(fit(
                &format!(
                    "  {DIM}… {} more in the background{RESET}",
                    working.len() - SHOWN
                ),
                width,
            ));
        }
        rows
    }

    /// Leaves the background work of a conversation the session moves away from: nothing
    /// polls it anymore, and the conversation's log keeps it open so resuming it continues.
    pub(super) fn pause_background(&mut self) {
        let working = self.background_working();
        self.bg_running = FuturesUnordered::new();
        // Work handed over but not polled yet belongs to the same conversation.
        if let Some(rx) = &mut self.bg_rx {
            while rx.try_recv().is_ok() {}
        }
        self.controls.background.borrow_mut().clear();
        self.wakes = 0;
        self.live_tasks();
        if working > 0 {
            self.notice(&format!(
                "Paused {} working in the background; resuming that session continues {}.",
                tools_count(working, "subagent task"),
                if working == 1 { "it" } else { "them" }
            ));
        }
    }

    /// Takes up the background work a resumed session left open, and says so.
    pub(super) fn resume_background(&mut self, agent: &mut Agent) {
        let resumed = agent.resume_background();
        if resumed > 0 {
            self.live_tasks();
            self.notice(&format!(
                "Continuing {} paused when this session stopped.",
                tools_count(resumed, "background subagent task")
            ));
        }
    }

    /// Ids of background tasks that worked in the tree since `since`, for /undo's warning.
    pub(super) fn background_since(&self, since: Option<u64>) -> Vec<String> {
        let Some(since) = since else {
            return Vec::new();
        };
        self.controls
            .background
            .borrow()
            .worked_since(since)
            .into_iter()
            .map(|id| format!("background task {id}"))
            .collect()
    }

    /// The question asked before leaving while background subagents work.
    pub(super) fn open_quit_picker(&mut self) {
        let working = self.background_working();
        let options = vec![
            (
                "Pause them".to_string(),
                "quit; resuming this session continues them".to_string(),
                PickerChoice::Quit(true),
            ),
            (
                "Stop them".to_string(),
                "quit and drop their work".to_string(),
                PickerChoice::Quit(false),
            ),
            (
                "Stay".to_string(),
                "keep the session open".to_string(),
                PickerChoice::Stay,
            ),
        ];
        self.picker = Some(Picker::new(
            format!(
                "{} still working in the background",
                tools_count(working, "subagent task")
            ),
            options,
            0,
        ));
    }
}

fn tools_count(n: usize, what: &str) -> String {
    crate::tools::count(n, what)
}
