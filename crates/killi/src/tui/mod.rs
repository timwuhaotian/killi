//! The interactive terminal UI. By default the conversation is printed into the terminal's own
//! scrollback, so scrolling, selecting and copying work as usual; only a live area at the
//! bottom is redrawn: what is happening now, the task list, the prompt and a status line.
//!
//! In fullscreen mode (`[ui] fullscreen`, on by default in Kil) killi takes the alternate
//! screen instead: the live area stays on the bottom rows, the conversation scrolls above it
//! (wheel, PageUp and PageDown) and is laid out again whenever the window changes size. It is
//! kept in `history`, and printed to the terminal once more when killi quits. A drag across it
//! selects text, which is copied when the button is let go (`select`).

mod app;
pub(crate) mod banner;
mod commands;
mod draw;
mod editor;
mod events;
mod format;
mod history;
mod input;
mod layout;
mod menu;
mod mention;
mod notify;
mod palette;
mod paste;
mod picker;
mod peers;
mod rows;
mod run;
mod select;
mod start;
mod state;
mod swarm;
pub(crate) mod text;
mod turn;
mod undo;

use std::io::{Stdout, Write, stdout};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::ExitCode;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::cursor::{Hide, MoveDown, MoveTo, MoveToColumn, MoveUp, Show};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, queue};
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::agent::{Agent, Controls, Event, Image, Question, Sink, Slot, Steered, Stop, Todo};
use crate::config::{Config, Thinking};
use crate::setup::{Overview, Setup};
use crate::live;
use crate::llm::{Client, ErrorKind, ProviderError, Usage};
use crate::mcp::Mcp;
use crate::session::{Choice, Replayed};
use crate::snapshot::Snapshots;
use crate::tools::{CONTAINER, image_type, image_url};
use editor::Editor;
use history::{Entry as Line, History};
use menu::{Entry, Fill, Item, Suggestions};
use palette::Ground;
use text::{ADD, BOLD, BRANCH, DANGER, DEL, DIM, EFFORT, FAINT, FIN, LAGOON, Markdown, Part, RESET, RULE, SELECT, TINT, TOKENS, WARN, ellipsize, wrap, wrap_code, wrap_words};

use commands::{BUSY_COMMANDS, COMMANDS, KEYS, entries, effort_named, is_command, runs_at_once, submission};
pub use start::{install_crash_hook, restore_terminal};
pub(crate) use format::megabytes;
use format::{ago, changed_rows, clip, clock, dot, error_hint, fit, gauge, gist, move_rows, near_full, padding, precise_clock, short, thinking_parts, tint};
use input::newline_key;
use notify::{copy_to_clipboard, first_line, notify};
use picker::{Picker, SearchFlow};
use rows::{ask_rows, aside_rows, choice_rows, diff_rows, first_sentence, handoff, help_rows, hidden_row, hint_row, is_bookkeeping, key_hints, label, overview_rows, placeholder, prompt_rows, queued_row, question_rows, recap_rows, rule, shorten_path, snapshot_rows, tilde, title_rule, tool_summary, tool_text, working_rows, wrap_items};
use run::{handle_command, next_incoming, peer_label, prompt_turn};
use select::Selection;
use start::{CURSOR_ROW, ENHANCED, RESUMED, TerminalGuard, leave_fullscreen, query_ground, resume_command, resumed, terminal_gone, wants_fullscreen};
use state::{App, Asking, Ended, Idle, Input, Kind, LastEnd, PastAsk, PeerAnswer, PendingAside, Running, SinceBanner, Status, Totals};
use undo::{restore_steered, undo_empty, undo_note, undo_paths};

const MAX_INPUT_ROWS: usize = 10;
const MENU_ROWS: usize = 8;
/// Rows of an edit's diff shown under its line; the whole diff stays behind Ctrl+O.
const DIFF_ROWS: usize = 40;
/// Lines of a bash command's output shown under its line.
const PREVIEW_ROWS: usize = 3;
/// Tool outputs and thoughts Ctrl+O can step back through.
const DETAIL_KEPT: usize = 10;
/// How many of a resumed session's last prompts are shown again, with what followed each.
const REPLAY_PROMPTS: usize = 8;
/// Lines of a replayed prompt shown before the rest folds away: a skill's text can run to thousands.
const REPLAY_PROMPT_ROWS: usize = 6;
/// The frames of a running tool call's glyph, a step each tick.
const QUARTER: [char; 4] = ['◐', '◓', '◑', '◒'];
/// How long a branch reading stays fresh: the status line's two git spawns run at most this
/// often, and at most once per prompt, instead of after every main-loop iteration.
const BRANCH_TTL: Duration = Duration::from_secs(5);

/// Questions from other sessions answered at once; more wait for one to finish.
const MAX_PEER_ASKS: usize = 2;

/// Messages from other sessions taken between two prompts, so two running sessions can't keep
/// each other going.
const MAX_PEER_TELLS: usize = 3;

pub struct Start {
    pub model: Option<String>,
    /// A session log to continue.
    pub resume: Option<PathBuf>,
    /// Open the session picker first.
    pub pick_session: bool,
}

pub async fn run(config: Config, start: Start) -> Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let sink: Sink = Rc::new(move |event| {
        let _ = tx.send(event);
    });
    let mut controls = Controls { interactive: true, ..Controls::default() };
    controls.swarm.set(config.agent.swarm_on());
    let (live, peers) = match config.agent.peers_on().then(|| live::join(&cwd, true)).flatten() {
        Some((registration, peers)) => (Some(registration), peers),
        None => (None, None),
    };
    controls.here = live.as_ref().map(|l| Rc::new(l.here()));
    let fullscreen = wants_fullscreen(config.ui.fullscreen, &std::env::var("TERM_PROGRAM").unwrap_or_default());
    if terminal_gone(libc::STDIN_FILENO) {
        return Ok(ExitCode::SUCCESS);
    }
    let ground = if config.ui.theme.is_none() { query_ground() } else { None };
    text::init_theme(config.ui.theme.as_deref(), ground);
    let mut session = Setup::new(config, &cwd, start.model.as_deref()).await?;
    // Only once killi has a model to start on: a broken config ends on the reason instead.
    crate::config::command_guide_if_first()?;
    // The log parses once: the items below replay from the same load the resume moves.
    let mut start_replayed = Vec::new();
    let mut agent = match &start.resume {
        Some(log) => {
            let report = crate::session::load_reported(log)?;
            start_replayed = crate::session::replay(&report.loaded, &cwd, REPLAY_PROMPTS);
            session.resume_loaded(report, log, controls.clone(), sink.clone())?
        }
        None => session.start(controls.clone(), sink.clone())?,
    };
    let mut snapshots = Snapshots::new(&cwd);
    let mut app = App::new(&agent, controls.clone(), &cwd, session.skills().clone());
    app.live = live;
    app.peers = peers;
    app.live_sync(&agent);
    app.entries = entries(&session);
    app.refresh_choices(session.config());
    app.notify_after = Duration::from_secs(session.config().agent.notify_after.unwrap_or(10));
    app.auto_recap = session.config().ui.recap.unwrap_or(true);
    let terminal = TerminalGuard::enter(fullscreen)?;
    app.fullscreen = fullscreen;
    if !fullscreen {
        app.locate();
    }
    app.banner(&agent, &session, &snapshots, start.resume.is_some());
    if start.resume.is_some() {
        app.replay_items(start_replayed);
    }
    if start.pick_session {
        app.open_session_picker(&cwd);
    }
    app.draw()?;

    let mut keys = EventStream::new();
    loop {
        let Some(input) = app.read_input(&mut keys).await? else {
            // Ctrl+C twice or Ctrl+D on an empty prompt: leaving still waits for the goal.
            if let Some(note) = crate::goal::blocks("Quitting", agent.goal()) {
                app.notice(&note);
                app.draw()?;
                continue;
            }
            break;
        };
        match input {
            Input::Prompt(prompt, images) => {
                let sent = mention::expand(&cwd, &prompt);
                prompt_turn(&mut app, &mut agent, &mut snapshots, &mut keys, &mut rx, &prompt, &sent, &images).await?;
            }
            Input::Command(command, arg, images) => {
                if !handle_command(&mut app, &mut agent, &mut session, &mut snapshots, &mut keys, &mut rx, &cwd, &command, &arg, &images).await? {
                    break;
                }
            }
            Input::Resume(_) if agent.goal().is_some() => {
                let note = crate::goal::blocks("Resuming another session", agent.goal()).expect("a goal stands");
                app.notice(&note);
            }
            Input::Resume(path) if crate::session::is_locked(&path) => {
                app.notice("that session is still running; continue it there, or start /new");
            }
            Input::Resume(path) => match crate::session::load_reported(&path) {
                Err(e) => app.error_report(&e),
                Ok(report) => {
                    let replayed = crate::session::replay(&report.loaded, &cwd, REPLAY_PROMPTS);
                    match session.resume_loaded(report, &path, controls.clone(), sink.clone()) {
                        Ok(resumed) => {
                            app.reset_conversation(&mut snapshots, &cwd);
                            let stopped = agent.stop_tasks();
                            agent.discard_if_unused();
                            agent = resumed;
                            app.show_model(&agent, session.config());
                            app.status.context = agent.stats.context;
                            app.notice(&format!("Resumed the session started with “{}”.", agent.user_prompts().first().map_or("", |s| s.lines().next().unwrap_or(""))));
                            if let Some(note) = stopped_note(&stopped) {
                                app.notice(&note);
                            }
                            app.replay_items(replayed);
                            app.editor = Editor::with_history(agent.user_prompts());
                        }
                        Err(e) => app.error_report(&e),
                    }
                }
            },
            Input::Model(alias) => match session.config().resolve(Some(&alias)).and_then(Client::new) {
                Ok(client) => {
                    if let Err(e) = agent.switch_model(client) {
                        app.error_report(&e);
                    }
                    app.show_model(&agent, session.config());
                    app.redraw_banner(&agent, &session, &snapshots);
                    app.save_choice(&agent);
                    app.notice(&format!("Switched to {alias}, keeping the conversation so far. The cache starts over with the new model."));
                }
                Err(e) => app.error_report(&e),
            },
            Input::BackupModel(alias) => {
                let alias = alias.trim().to_string();
                if alias.is_empty() || matches!(alias.as_str(), "none" | "off" | "clear") {
                    if let Err(e) = agent.clear_backup() {
                        app.error_report(&e);
                    }
                    app.show_model(&agent, session.config());
                    app.redraw_banner(&agent, &session, &snapshots);
                    app.notice(&format!("This session has no backup model; it stays on {}.", agent.model_alias()));
                } else {
                    match session.config().resolve(Some(&alias)).and_then(Client::new) {
                        Ok(client) => {
                            if let Err(e) = agent.set_backup(client) {
                                app.error_report(&e);
                            }
                            app.show_model(&agent, session.config());
                            app.redraw_banner(&agent, &session, &snapshots);
                            app.notice(&format!("Backup model for this session: {alias}. A rate limit on {} switches to it; new sessions start without one.", agent.model_alias()));
                        }
                        Err(e) => app.error_report(&e),
                    }
                }
            }
            Input::Search(provider, url, key) => {
                let path = crate::config::killi_home().join("config.toml");
                let pick = crate::config::SearchConfig { provider: Some(provider.clone()), url: url.clone(), api_key: key.clone(), api_key_env: None };
                match crate::config::set_search(&path, &provider, url.as_deref(), key.as_deref()) {
                    Ok(()) => {
                        agent.set_search(pick.settings());
                        session.set_search(pick);
                        let what = match (provider.as_str(), url.as_deref(), key.is_some()) {
                            ("searxng", Some(url), true) => format!("SearXNG at {url}, with its key"),
                            ("searxng", Some(url), false) => format!("SearXNG at {url}"),
                            ("brave", _, false) => "Brave, reading its public results page".to_string(),
                            (other, _, true) => format!("{other}, with an API key"),
                            (other, _, false) => other.to_string(),
                        };
                        app.notice(&format!("Web search: {what}, here and in new sessions."));
                    }
                    Err(e) => app.error_from("/web-search-config", &e),
                }
            }
            Input::Mcp(name, from_list) => match session.mcp().toggle(&name) {
                Ok(on) => {
                    app.notice(&match on {
                        true => format!("MCP server {name} is on."),
                        false => format!("MCP server {name} is off, here and in new sessions."),
                    });
                    if from_list {
                        app.open_mcp_picker(session.mcp(), Some(&name));
                    }
                }
                Err(e) => app.error_report(&e),
            },
            Input::Prune(keep) => match crate::session::prune(keep, &agent.session.log_path) {
                Ok(pruned) if pruned.sessions == 0 => app.notice("Nothing to prune."),
                Ok(pruned) => app.notice(&format!(
                    "Removed {} session{} ({}). Kept the newest {keep} and this one.",
                    pruned.sessions,
                    if pruned.sessions == 1 { "" } else { "s" },
                    megabytes(pruned.bytes)
                )),
                Err(e) => app.error(&format!("Can't prune: {e:#}")),
            },
            Input::Effort(effort) => {
                let alias = agent.model_alias().to_string();
                let efforts = session.config().efforts(&alias);
                if efforts.contains(&effort) {
                    if let Err(e) = agent.set_thinking(Thinking::parse(&effort)) {
                        app.error_report(&e);
                    }
                    // The pick is the model's `default_effort` in the config file, so the next
                    // session on this model — and a /model switch in this one — starts on it.
                    session.set_effort(&alias, &effort);
                    let path = crate::config::killi_home().join("config.toml");
                    if let Err(e) = crate::config::set_model_effort(&path, &alias, &effort) {
                        app.error_report(&e);
                    }
                    app.show_model(&agent, session.config());
                    app.redraw_banner(&agent, &session, &snapshots);
                    app.save_choice(&agent);
                    app.notice(&format!("Thinking effort {effort}, saved as {alias}'s default."));
                } else {
                    app.error(&format!("{alias} has no effort “{effort}”; choose one of {}.", efforts.join(", ")));
                }
            }
        }
        app.refresh_branch_async().await;
        app.live_sync(&agent);
        app.draw()?;
    }
    app.finish_screen()?;
    let stopped = agent.stop_tasks();
    agent.session_end("exit").await;
    drop(terminal);
    if let Some(note) = stopped_note(&stopped) {
        println!("{DIM}{note}{RESET}");
    }
    agent.discard_if_unused();
    if !agent.user_prompts().is_empty() {
        // The command on its own line, so a triple-click or a drag copies it alone.
        println!("{DIM}To continue this session:{RESET}\n{}", resume_command(&agent.session.log_path));
    }
    Ok(ExitCode::SUCCESS)
}

/// Says which background commands were stopped as a session ended, if any.
pub fn stopped_note(stopped: &[String]) -> Option<String> {
    let list: Vec<String> = stopped.iter().map(|command| format!("`{command}`")).collect();
    (!list.is_empty()).then(|| format!("Stopped what was still running: {}.", list.join(", ")))
}

#[cfg(test)]
mod tests;
