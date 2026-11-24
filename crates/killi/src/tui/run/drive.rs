//! A turn of the agent's from start to end: the undo snapshot beside it, and what follows.

use super::super::*;
use crate::snapshot::{Cancel, Taken};

/// Runs one turn started by a command: `shown` goes in the transcript, `prompt` to the agent,
/// with the images pasted beside the command.
#[allow(clippy::too_many_arguments)]
pub(in crate::tui) async fn prompt_turn(
    app: &mut App,
    agent: &mut Agent,
    snapshots: &mut Snapshots,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    shown: &str,
    prompt: &str,
    images: &[Image],
) -> Result<()> {
    app.begin_turn(shown, images);
    app.draw()?;
    app.peer_tells = 0;
    // A scheduled task is no message from the user: it leaves the count of turns background
    // reports started where it was.
    if app.scheduled.is_none() {
        app.wakes = 0;
        app.wake_held = false;
    }
    app.undo_wake = false;
    drive_turn(
        app,
        agent,
        snapshots,
        keys,
        rx,
        shown,
        Some((prompt, images)),
    )
    .await
}

/// Runs a turn the session starts by itself because background work ended while it sat idle:
/// no prompt of the user's is shown or sent, and the reports come first.
pub(in crate::tui) async fn wake_turn(
    app: &mut App,
    agent: &mut Agent,
    snapshots: &mut Snapshots,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
) -> Result<()> {
    app.wakes += 1;
    app.undo_wake = true;
    app.drop_suggestion();
    app.notice("Background work ended; the agent reads its report.");
    app.begin_busy();
    app.draw()?;
    drive_turn(
        app,
        agent,
        snapshots,
        keys,
        rx,
        "background work ended",
        None,
    )
    .await
}

/// The body of a turn: snapshot, run, and what follows its end. `prompt` is the user's message
/// with its images, or none for a turn background reports started.
async fn drive_turn(
    app: &mut App,
    agent: &mut Agent,
    snapshots: &mut Snapshots,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    shown: &str,
    prompt: Option<(&str, &[Image])>,
) -> Result<()> {
    app.undo_warned = false;
    app.live_turn(Some(shown));
    let before = Totals::of(agent);
    let taken = snapshots.taken();
    // The snapshot's git commands run on a blocking thread while the turn's first request
    // is already in flight; the agent waits for the tree before its first tool call, so the
    // tree still predates every change the turn makes.
    let ended = if let Some(work) = snapshots.start_take() {
        app.snapshotting = true;
        app.snapshot_seq += 1;
        let cancel = work.canceler();
        // However this turn is left, a snapshot still running then is given up on: its git
        // would otherwise hold the session's index lock, and the exit, after no one waits.
        let _give_up = GiveUp(cancel.clone());
        let (snap_tx, snap_rx) = oneshot::channel();
        agent.set_snapshot_wait(snap_rx);
        let mut snap = Some(tokio::task::spawn_blocking(move || work.run()));
        let mut snap_tx = Some(snap_tx);
        let ended = {
            let turn = run_turn(app, keys, rx, start(agent, prompt));
            tokio::pin!(turn);
            loop {
                tokio::select! {
                    ended = &mut turn => break ended,
                    joined = async { snap.as_mut().expect("the snapshot runs").await }, if snap.is_some() => {
                        snap = None;
                        snapshots.commit_take(snapshot_tree(joined));
                        if let Some(tx) = snap_tx.take() {
                            let _ = tx.send(());
                        }
                    }
                }
            }
        };
        // A turn or a wait that failed still gives the turn its place in the undo list.
        let failed = || anyhow::anyhow!("the turn failed before it finished");
        let finished = match (&ended, snap) {
            (Ok(_), Some(snap)) => Some(finish_snapshot(app, keys, snap, &cancel).await),
            (Err(_), Some(_)) => Some(Ok(Err(failed()))),
            (_, None) => None,
        };
        app.snapshotting = false;
        match finished {
            Some(Ok(tree)) => snapshots.commit_take(tree),
            Some(Err(e)) => {
                snapshots.commit_take(Err(failed()));
                return Err(e);
            }
            None => {}
        }
        ended?
    } else {
        run_turn(app, keys, rx, start(agent, prompt)).await?
    };
    if matches!(ended, Ended::Interrupted) && app.take_back(agent)? {
        app.wake_held = true;
        // Nothing ran, so /undo still goes back to the turn before this one.
        snapshots.forget_after(taken);
        app.live_turn(None);
        return Ok(());
    }
    if let Some(why) = &snapshots.skipped {
        app.notice(why);
    }
    let completed = matches!(ended, Ended::Done(Ok((_, Stop::Completed))));
    // A turn that ended with the model's answer leaves the prompt empty, so it can offer
    // what to ask next.
    let answered = matches!(ended, Ended::Done(Ok(_)));
    let failed_on = match &ended {
        Ended::Done(Err(error)) => Some(first_line(&gist(&format!("{error:#}")), 200)),
        _ => None,
    };
    let after = Totals::of(agent);
    app.finish_turn(agent, ended, before, after);
    app.live_turn(None);
    if !app.controls.peer_notes.borrow().is_empty() {
        app.notice("A message from another session came as the turn ended; the agent gets it with your next prompt.");
    }
    match failed_on {
        Some(error) => agent.recover_failed(&error)?,
        None if !matches!(app.last_end, Some(LastEnd::Done)) => agent.recover()?,
        None => {}
    }
    if wants_recap(
        app.auto_recap,
        completed,
        Totals::of(agent).tool_calls - before.tool_calls,
    ) {
        app.recap = Some(Box::pin(app.controls.aside.borrow().recap()));
    }
    if answered {
        app.ask_suggestion();
    }
    Ok(())
}

/// The tree a snapshot's blocking task came back with, or why there is none.
fn snapshot_tree(joined: Result<Result<Taken>, tokio::task::JoinError>) -> Result<Taken> {
    joined
        .map_err(|e| anyhow::anyhow!("the snapshot task ended: {e}"))
        .and_then(|tree| tree)
}

/// Waits for an undo snapshot the turn ended before, as after an early Esc or a quick reply
/// in a big tree, and returns its tree: the live area says what it waits for, keeps drawing,
/// and the prompt stays editable. Esc or Ctrl+C gives up on it, so this turn has no snapshot
/// to undo to, and `cancel` stops its git; Enter waits with the text in the prompt rather
/// than queueing it for a turn that is over.
pub(in crate::tui) async fn finish_snapshot<K, S>(
    app: &mut App,
    keys: &mut K,
    snap: S,
    cancel: &Cancel,
) -> Result<Result<Taken>>
where
    K: futures_util::Stream<Item = std::io::Result<TermEvent>> + Unpin,
    S: Future<Output = Result<Result<Taken>, tokio::task::JoinError>>,
{
    tokio::pin!(snap);
    // The turn's tool calls are over: the row says what is left instead.
    app.running.clear();
    app.running.push(Running {
        id: "undo-snapshot".into(),
        summary: SNAPSHOT_WAIT.into(),
        label: None,
        started: Instant::now(),
    });
    // A snapshot that finishes in a blink is waited for without a frame of its row.
    let mut tick = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_millis(150),
        Duration::from_millis(100),
    );
    let tree = loop {
        tokio::select! {
            joined = &mut snap => break snapshot_tree(joined),
            Some(Ok(event)) = keys.next() => {
                if snapshot_key(app, event) {
                    cancel.cancel();
                    break Err(anyhow::anyhow!("Esc ended the wait for it"));
                }
                app.draw()?;
            }
            _ = tick.tick() => {
                app.spinner += 1;
                app.draw()?;
            }
        }
    };
    app.running.clear();
    Ok(tree)
}

/// Gives up on a turn's snapshot when the turn is left, whichever way: by then it was either
/// recorded or no one waits for it.
struct GiveUp(Cancel);

impl Drop for GiveUp {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// The live area's row while a turn waits for its undo snapshot.
const SNAPSHOT_WAIT: &str = "Finishing the undo snapshot… Esc skips it";

/// A key while the snapshot is awaited: true to give up on it. Sending is held back, since
/// the turn a message would join is over.
fn snapshot_key(app: &mut App, event: TermEvent) -> bool {
    if let TermEvent::Key(key) = &event {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let send = (key.code == KeyCode::Enter && !newline_key(key))
            || (ctrl && key.code == KeyCode::Char('s'));
        if send {
            return false;
        }
    }
    app.on_key_busy(event)
}

/// A turn's run of the agent, whichever way it started.
type TurnRun<'a> = Pin<Box<dyn Future<Output = Result<(String, Stop)>> + 'a>>;

/// The agent's run for a turn: the user's prompt, or a wake when there is none.
fn start<'a>(agent: &'a mut Agent, prompt: Option<(&'a str, &'a [Image])>) -> TurnRun<'a> {
    match prompt {
        Some((prompt, images)) => Box::pin(agent.run(prompt, images)),
        None => Box::pin(agent.wake()),
    }
}

/// Whether a finished turn gets a recap by itself: a turn that only answered is its own recap.
pub(in crate::tui) fn wants_recap(on: bool, completed: bool, tool_calls: u32) -> bool {
    on && completed && tool_calls > 0
}

/// Keeps a shown recap in the session's log, so a resume shows it again. Best effort:
/// a failed save stays unsaved for the next flush, and never interrupts the session.
pub(in crate::tui) fn flush_recap(app: &mut App, agent: &mut Agent) {
    if let Some(text) = app.recap_unsaved.clone()
        && agent.record_recap(&text).is_ok()
    {
        app.recap_unsaved = None;
    }
}
