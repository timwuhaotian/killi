use super::*;

use std::collections::BTreeMap;

use super::process::{Exit, Process};

#[derive(Default)]
pub struct Tasks {
    next: u32,
    pub(super) list: BTreeMap<u32, Task>,
}

pub(super) struct Task {
    pub(super) command: String,
    pub(super) process: Process,
    pub(super) started: Instant,
    /// How much of the output the model has already been shown.
    pub(super) read_to: usize,
    pub(super) reported: bool,
    /// The model was told at the end of a turn that this still runs, or stopped it.
    pub(super) reminded: bool,
}

impl Task {
    /// Lets go of a finished task's output: all of it once the model has read it, else the last
    /// megabyte. A task that ran for hours would otherwise hold up to 16 MB until the session ends.
    pub(super) fn release(&mut self) {
        let Some(exit) = self.process.exit() else {
            return;
        };
        if self.process.running() {
            return;
        }
        let mut captured = self.process.output.lock().expect("output lock");
        let read = self.read_to >= captured.dropped + captured.bytes.len();
        captured.trim(if read { 0 } else { FINISHED_KEEP });
        // A failed task's first read still needs the output's head for its root cause.
        if read || matches!(exit, Exit::Code(0)) {
            captured.head = Vec::new();
        }
    }
}

impl Tasks {
    /// Registers a running command; `read_to` is how much of its output was already shown. A
    /// command whose shell already exited was reported as it ended.
    pub(super) fn add(&mut self, command: &str, process: Process, read_to: usize) -> u32 {
        self.next += 1;
        let reported = process.exit().is_some();
        let task = Task {
            command: command.to_string(),
            process,
            started: Instant::now(),
            read_to,
            reported,
            reminded: false,
        };
        self.list.insert(self.next, task);
        self.next
    }

    /// Tasks still running that the model wasn't told about yet at the end of a turn, as
    /// `task N \`command\` (Ns)`. Each comes up once, so one kept on purpose isn't raised again.
    pub fn left_running(&mut self) -> Vec<String> {
        let mut left = Vec::new();
        for (id, task) in &mut self.list {
            if !task.reminded && task.process.running() {
                task.reminded = true;
                left.push(format!(
                    "task {id} `{}` ({}s)",
                    first_line(&task.command),
                    task.started.elapsed().as_secs()
                ));
            }
        }
        left
    }

    /// Kills every task still running and gives their commands' first lines.
    pub fn stop_all(&mut self) -> Vec<String> {
        let mut stopped = Vec::new();
        for task in self.list.values() {
            if task.process.running() {
                task.process.kill();
                stopped.push(first_line(&task.command).to_string());
            }
        }
        stopped
    }

    /// Background tasks that finished since the last call, as reminder lines.
    pub fn newly_finished(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        for (id, task) in &mut self.list {
            task.release();
            if let Some(exit) = task.process.exit().filter(|_| !task.reported) {
                task.reported = true;
                lines.push(format!(
                    "Background task {id} (`{}`) {}.",
                    first_line(&task.command),
                    exit.describe()
                ));
            }
        }
        lines
    }
}
