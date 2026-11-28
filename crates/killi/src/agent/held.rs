//! What killi holds itself and says again after a compaction. The summary is the model's own
//! account of the conversation; the task list and the work still running in the background
//! are state the harness keeps, exact only here, and the calls that told the model of them
//! went with the old messages.

use super::*;

impl Agent {
    /// The reminder that follows a compaction's summary: the task list while it has open
    /// items, and the commands and subagents still running in the background. None when
    /// killi holds nothing the summary would have to carry.
    pub(super) fn held_state_note(&self) -> Option<String> {
        let mut parts: Vec<String> = tasks_note(&self.todos).into_iter().collect();
        let mut running = self.ctx.tasks.borrow().running();
        // Background subagents belong to the session; a subagent's own never outlive it.
        if self.depth == 0 {
            running.extend(
                self.controls
                    .background
                    .borrow()
                    .tasks
                    .iter()
                    .filter(|(_, task)| !task.ended())
                    .map(|(id, task)| format!("task {id} (subagent: {})", task.label)),
            );
        }
        if !running.is_empty() {
            parts.push(format!(
                "Still running in the background: {}. Read one with task_output; stop with task_stop what nothing needs anymore.",
                running.join(", ")
            ));
        }
        if parts.is_empty() {
            return None;
        }
        let state = neutralize_reminder_tags(&parts.join("\n\n")).into_owned();
        Some(format!(
            "<system-reminder>What killi itself kept through the compaction, exactly as it stands:\n\n{state}</system-reminder>"
        ))
    }
}
