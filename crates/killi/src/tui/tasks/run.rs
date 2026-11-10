//! A task's agent at work: run to its report, its final line read for where the task stands,
//! and its steps sent to the streams that follow it.

use tokio::sync::{mpsc, oneshot};

use super::super::*;
use crate::agent::Aside;

/// Runs a task's agent to its report: this session's own model and tools on a prompt of its own,
/// in a log of its own. It stops when `killi cancel` asks for it or when its time is up, and
/// either way comes back with where the task stands.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_peer_task(
    view: Rc<RefCell<live::TaskView>>,
    number: usize,
    prompt: String,
    aside: Rc<RefCell<Aside>>,
    here: Option<Rc<live::Here>>,
    watchers: Rc<RefCell<Vec<mpsc::UnboundedSender<String>>>>,
    cancel: oneshot::Receiver<()>,
    timeout: Duration,
) -> String {
    let id = view.borrow().id.clone();
    {
        let mut view = view.borrow_mut();
        view.state = live::TaskState::Working;
        view.since = live::now();
    }
    let built = {
        let aside = aside.borrow();
        let sink = Rc::new(task_sink(watchers, view.clone()));
        crate::agent::task_agent(&aside, here, sink, number)
    };
    let mut agent = match built {
        Ok(agent) => agent,
        Err(e) => {
            let mut view = view.borrow_mut();
            view.state = live::TaskState::Failed;
            view.message = Some(format!("{e:#}"));
            return id;
        }
    };
    let asked = crate::agent::subagent_prompt(&prompt);
    let outcome = tokio::select! {
        result = agent.run(&asked, &[]) => match result {
            Ok((text, stop)) if !text.trim().is_empty() && stop != Stop::RepeatBreaker => Ok(text),
            Ok((_, stop)) => Err(format!("its agent stopped without a final report ({stop:?}).")),
            Err(e) => Err(format!("its agent failed: {e:#}")),
        },
        _ = cancel => Err("canceled".to_string()),
        _ = tokio::time::sleep(timeout) => {
            Err(format!("it didn't finish within {}s", timeout.as_secs()))
        }
    };
    // Dropping the agent otherwise leaves whatever it started in the background running.
    agent.stop_tasks();
    agent.session_end("peer task").await;
    let mut view = view.borrow_mut();
    view.log = Some(agent.session.log_path.clone());
    match outcome {
        Ok(text) => {
            let (state, message) = outcome_of(&text);
            view.state = state;
            view.report = Some(text);
            view.message = message;
        }
        Err(why) => {
            let canceled = why == "canceled";
            view.state = if canceled {
                live::TaskState::Canceled
            } else {
                live::TaskState::Failed
            };
            // `killi cancel` already said why; a failure explains itself.
            if !canceled {
                view.message = Some(why);
            }
        }
    }
    id
}

/// Where a task's final line puts it: `Status: blocked` or `waiting` needs an answer from the
/// session that sent it, which comes as a new task; anything else that isn't `ready` is a
/// failure the report explains. Models dress the line up — in backticks or bold, capitalized,
/// with a full stop or a colon after the word, with blank lines under it — so it is read
/// without its decoration: a failure that passed for a finished task would exit `killi send`
/// with success.
pub(super) fn outcome_of(text: &str) -> (live::TaskState, Option<String>) {
    let decoration = |c: char| c.is_whitespace() || matches!(c, '`' | '*' | '_' | '>' | '#');
    let last = text
        .lines()
        .rev()
        .map(|line| line.trim_matches(decoration))
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let Some(rest) = last
        .get(..7)
        .filter(|label| label.eq_ignore_ascii_case("status:"))
        .map(|_| last[7..].trim_start_matches(decoration))
    else {
        return (live::TaskState::Completed, None);
    };
    let end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    let word = rest[..end].to_ascii_lowercase();
    let why = rest[end..]
        .trim_start_matches(|c: char| decoration(c) || matches!(c, ':' | '.' | ',' | '-' | '—' | '–'))
        .trim_end();
    let why = || {
        if why.is_empty() {
            format!("it reported `Status: {word}`")
        } else {
            why.to_string()
        }
    };
    match word.as_str() {
        "ready" => (live::TaskState::Completed, None),
        "blocked" | "waiting" => (live::TaskState::InputRequired, Some(why())),
        _ => (live::TaskState::Failed, Some(why())),
    }
}

/// A task's events as its `--follow` streams see them: one line per step it takes. The model's
/// prose arrives as deltas, so it is not streamed; the report at the end is. The step also lands
/// in the task's own record, so a stream that joins mid-step says what it is on.
fn task_sink(
    watchers: Rc<RefCell<Vec<mpsc::UnboundedSender<String>>>>,
    view: Rc<RefCell<live::TaskView>>,
) -> impl Fn(Event) + 'static {
    move |event| match event {
        Event::ToolStart { summary, .. } => {
            view.borrow_mut().message = Some(crate::llm::truncate(&summary, 200).to_string());
            send_event(&watchers, live::TaskState::Working, Some(&summary))
        }
        Event::Notice(text) => send_event(&watchers, live::TaskState::Working, Some(&text)),
        _ => {}
    }
}

/// One event to every stream reading the task, dropping the ones that stopped reading.
fn send_event(
    watchers: &Rc<RefCell<Vec<mpsc::UnboundedSender<String>>>>,
    state: live::TaskState,
    message: Option<&str>,
) {
    let line = live::event_line(state, message);
    watchers
        .borrow_mut()
        .retain(|events| events.send(line.clone()).is_ok());
}
