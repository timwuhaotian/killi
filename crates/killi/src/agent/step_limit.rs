//! A turn's step limit: how many requests it may make before it is asked to wrap up, and the
//! last reply that ends it.

use super::*;

/// The result of a call in the reply a turn's step limit asked for: it ends the turn, so the
/// call never runs, and the answer keeps every call in history paired with a result.
pub(super) const STEP_LIMIT_NOT_RUN: &str = "Not run: the turn reached its step limit.";

/// Told when a turn has made as many requests as its step limit allows: the next reply is its
/// last, so it says where the work stands instead of calling tools nobody will run.
pub(super) fn step_limit_note(cap: u32) -> String {
    format!(
        "<system-reminder>You reached this turn's step limit ({cap} requests). Don't call tools: reply now with what's done, what's left, and what the user must know.</system-reminder>"
    )
}

impl Agent {
    /// Requests one of this agent's turns may make before it is asked to wrap up (`None` for
    /// no limit), and the `[agent]` key that sets it: unlimited by default while the user
    /// watches, bounded when no one does.
    pub fn step_limit(&self) -> (Option<u32>, &'static str) {
        if self.depth == 0 && self.controls.interactive {
            (self.settings.max_steps_interactive, "max_steps_interactive")
        } else {
            (self.settings.max_steps, "max_steps")
        }
    }

    /// Asks for the turn's last reply once it has made its `cap` requests. The wrap-up request
    /// keeps the same tools, so the cached prefix still hits; the reminder asks for none.
    pub(super) fn ask_to_wrap_up(&mut self, cap: u32) -> Result<()> {
        (self.sink)(Event::Notice(format!(
            "The turn reached its step limit ({cap} requests). Asked the agent to wrap up."
        )));
        self.push(json!({"role": "user", "content": step_limit_note(cap)}))
    }

    /// Ends a turn on its wrap-up reply: the calls it made anyway are answered as not run,
    /// and the stop hook is told, not asked, since the turn ends whatever it says.
    pub(super) async fn end_wrap_up(
        &mut self,
        calls: &[(String, ToolCall)],
        hook_active: bool,
    ) -> Result<()> {
        for (id, _) in calls {
            self.push(json!({"role": "tool", "tool_call_id": id, "content": STEP_LIMIT_NOT_RUN}))?;
        }
        self.hook(
            self.stop_event(),
            None,
            json!({"stop_hook_active": hook_active}),
        )
        .await;
        Ok(())
    }

    /// The hook event a turn's end fires: `Stop` for the session the user talks to,
    /// `SubagentStop` for an agent another one started, a peer task included.
    pub(super) fn stop_event(&self) -> &'static str {
        if self.depth == 0 && !self.settings.peer_task {
            "Stop"
        } else {
            "SubagentStop"
        }
    }
}
