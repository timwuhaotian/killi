use super::*;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::process::Captured;

/// Foreground calls that haven't returned: a `bash` waiting for its command and a `task_output`
/// waiting for a task. `/btw` tells the model what they are and for how long, so a long wait
/// can be told from a hang.
#[derive(Default)]
pub struct Running {
    next: u32,
    list: BTreeMap<u32, Wait>,
}

struct Wait {
    what: String,
    started: Instant,
    output: Arc<Mutex<Captured>>,
}

/// Takes its call out of `Running` when the call returns or an interrupt drops it.
pub struct Waiting<'a> {
    running: &'a RefCell<Running>,
    id: u32,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.running.borrow_mut().list.remove(&self.id);
    }
}

pub(super) fn waiting<'a>(ctx: &'a Context, what: String, output: Arc<Mutex<Captured>>) -> Waiting<'a> {
    let mut running = ctx.running.borrow_mut();
    running.next += 1;
    let id = running.next;
    running.list.insert(id, Wait { what, started: Instant::now(), output });
    Waiting { running: &ctx.running, id }
}

impl Running {
    /// What is in flight, one entry per call with how long it has run, how long it has been
    /// quiet and its last lines; `None` when nothing is.
    pub fn describe(&self) -> Option<String> {
        let entries: Vec<String> = self.list.values().map(Wait::describe).collect();
        (!entries.is_empty()).then(|| entries.join("\n"))
    }
}

impl Wait {
    fn describe(&self) -> String {
        let captured = self.output.lock().expect("output lock");
        let quiet = match captured.last {
            Some(at) => format!("last output {} ago", span(at.elapsed())),
            None => "no output yet".to_string(),
        };
        let from = captured.bytes.len().saturating_sub(2048);
        let recent = String::from_utf8_lossy(&captured.bytes[from..]);
        let mut lines: Vec<&str> = recent.trim_end().lines().rev().take(5).collect();
        lines.reverse();
        let lines: String = lines.iter().map(|line| format!("\n  {}", crate::llm::truncate(line, 160))).collect();
        format!("{} has run {}, {quiet}.{lines}", self.what, span(self.started.elapsed()))
    }
}

fn span(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 { format!("{secs}s") } else { format!("{}m{:02}s", secs / 60, secs % 60) }
}
