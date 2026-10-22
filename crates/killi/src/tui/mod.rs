//! The interactive terminal UI. The conversation is printed into the terminal's own
//! scrollback, so scrolling, selecting and copying work as usual; only a live area at the
//! bottom is redrawn: what is happening now, the task list, the prompt and a status line.

mod banner;
mod editor;
mod menu;
mod paste;
mod text;

use std::io::{Stdout, Write, stdout};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::cursor::{Hide, MoveDown, MoveToColumn, MoveUp, Show};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate};
use crossterm::{execute, queue};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::agent::{Agent, Controls, Event, Image, Question, Sink, Stop, Todo};
use crate::config::{Config, Thinking};
use crate::setup::{Overview, Setup};
use crate::llm::{Client, Usage};
use crate::mcp::Mcp;
use crate::session::{Choice, Replayed};
use crate::snapshot::Snapshots;
use editor::Editor;
use menu::{Entry, Item, Suggestions};
use text::{BOLD, BRANCH, DANGER, DIM, EFFORT, FAINT, FIN, LAGOON, Markdown, RESET, TINT, TOKENS, WARN, wrap, wrap_words};

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

pub struct Start {
    pub model: Option<String>,
    /// A session log to continue.
    pub resume: Option<PathBuf>,
    /// Open the session picker first.
    pub pick_session: bool,
}

/// A /btw question and its answer on the way.
type PendingAside = (String, Pin<Box<dyn Future<Output = Result<(String, Usage)>>>>);

/// What `run_turn` ended with.
enum Ended {
    Done(Result<(String, Stop)>),
    Interrupted,
}

pub async fn run(config: Config, start: Start) -> Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let sink: Sink = Rc::new(move |event| {
        let _ = tx.send(event);
    });
    let controls = Controls { interactive: true, ..Controls::default() };
    let background = if config.ui.theme.is_none() { query_background() } else { None };
    text::init_theme(config.ui.theme.as_deref(), background);
    let mut session = Setup::new(config, &cwd, start.model.as_deref()).await?;
    let mut agent = match &start.resume {
        Some(log) => session.resume(log, controls.clone(), sink.clone())?,
        None => session.start(controls.clone(), sink.clone())?,
    };
    let mut snapshots = Snapshots::new(&cwd);
    let mut app = App::new(&agent, controls.clone(), &cwd);
    app.entries = entries(&session);
    app.refresh_choices(session.config());
    app.notify_after = Duration::from_secs(session.config().agent.notify_after.unwrap_or(10));
    let terminal = TerminalGuard::enter()?;
    app.banner(&agent, &session, &snapshots, start.resume.is_some());
    if let Some(log) = &start.resume {
        app.replay_log(log);
    }
    if start.pick_session {
        app.open_session_picker(&cwd);
    }
    app.draw()?;

    let mut keys = EventStream::new();
    loop {
        let Some(input) = app.read_input(&mut keys).await? else { break };
        match input {
            Input::Prompt(prompt, images) => {
                app.begin_turn(&prompt);
                app.draw()?;
                snapshots.take();
                let before = Totals::of(&agent);
                let ended = run_turn(&mut app, &mut keys, &mut rx, agent.run(&prompt, &images)).await?;
                app.finish_turn(ended, before, Totals::of(&agent));
                if matches!(app.last_end, Some(LastEnd::Interrupted)) {
                    agent.recover()?;
                }
            }
            Input::Command(command, arg) => {
                if !handle_command(&mut app, &mut agent, &mut session, &mut snapshots, &mut keys, &mut rx, &cwd, &command, &arg).await? {
                    break;
                }
            }
            Input::Resume(path) => match session.resume(&path, controls.clone(), sink.clone()) {
                Ok(resumed) => {
                    agent.discard_if_unused();
                    agent = resumed;
                    app.show_model(&agent, session.config());
                    app.status.context = agent.stats.context;
                    app.notice(&format!("Resumed the session started with “{}”.", agent.user_prompts().first().map_or("", |s| s.as_str())));
                    app.replay_log(&path);
                    app.editor = Editor::with_history(agent.user_prompts());
                }
                Err(e) => app.error(&format!("{e:#}")),
            },
            Input::Model(alias) => match session.config().resolve(Some(&alias)).and_then(Client::new) {
                Ok(client) => {
                    if let Err(e) = agent.switch_model(client) {
                        app.error(&format!("{e:#}"));
                    }
                    app.show_model(&agent, session.config());
                    app.redraw_banner(&agent, &session, &snapshots);
                    app.save_choice(&agent);
                    app.notice(&format!("Switched to {alias}, keeping the conversation so far. The cache starts over with the new model."));
                }
                Err(e) => app.error(&format!("{e:#}")),
            },
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
                Err(e) => app.error(&format!("{e:#}")),
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
                let efforts = session.config().efforts(agent.model_alias());
                if efforts.contains(&effort) {
                    if let Err(e) = agent.set_thinking(Thinking::parse(&effort)) {
                        app.error(&format!("{e:#}"));
                    }
                    app.show_model(&agent, session.config());
                    app.redraw_banner(&agent, &session, &snapshots);
                    app.save_choice(&agent);
                    app.notice(&format!("Thinking effort {effort}. New sessions start on it too."));
                } else {
                    app.error(&format!("{} has no effort “{effort}”; choose one of {}.", agent.model_alias(), efforts.join(", ")));
                }
            }
        }
        app.refresh_branch();
        app.draw()?;
    }
    app.finish_screen()?;
    drop(terminal);
    agent.discard_if_unused();
    if !agent.user_prompts().is_empty() {
        println!("{DIM}To continue this session: {}{RESET}", resume_command(&agent.session.log_path));
    }
    Ok(ExitCode::SUCCESS)
}

/// Asks the terminal for its background color (OSC 11) and waits up to 200 ms for the answer,
/// so a light terminal gets the light palette without `COLORFGBG` or `[ui] theme`. Nothing
/// when stdin isn't a terminal or it doesn't answer; `[ui] theme` skips the question.
fn query_background() -> Option<(u8, u8, u8)> {
    use std::io::{IsTerminal, Read};
    if !std::io::stdin().is_terminal() || !stdout().is_terminal() {
        return None;
    }
    terminal::enable_raw_mode().ok()?;
    let answer = (|| {
        let mut out = stdout();
        write!(out, "\x1b]11;?\x1b\\").ok()?;
        out.flush().ok()?;
        let mut reply = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let mut poll = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
            // SAFETY: `poll` points at one pollfd that lives for the whole call.
            if unsafe { libc::poll(&mut poll, 1, left.as_millis() as i32) } <= 0 {
                break;
            }
            let mut bytes = [0u8; 64];
            let n = std::io::stdin().lock().read(&mut bytes).ok()?;
            if n == 0 {
                break;
            }
            reply.extend_from_slice(&bytes[..n]);
            if reply.ends_with(b"\x07") || reply.ends_with(b"\x1b\\") {
                break;
            }
        }
        parse_background(&String::from_utf8_lossy(&reply))
    })();
    let _ = terminal::disable_raw_mode();
    answer
}

/// The color in a terminal's OSC 11 answer, `rgb:rrrr/gggg/bbbb` with 1 to 4 hex digits
/// per channel, scaled to 8 bits.
fn parse_background(reply: &str) -> Option<(u8, u8, u8)> {
    let start = reply.find("rgb:")? + 4;
    let end = reply[start..].find(['\x07', '\x1b']).map_or(reply.len(), |i| start + i);
    let channel = |hex: &str| {
        let value = u32::from_str_radix(hex, 16).ok()?;
        Some(match hex.len() {
            1 => (value * 17) as u8,
            n @ 2..=4 => (value >> (4 * (n - 2))) as u8,
            _ => return None,
        })
    };
    let mut channels = reply[start..end].split('/').map(channel);
    let (r, g, b) = (channels.next()??, channels.next()??, channels.next()??);
    Some((r, g, b))
}

/// The command that reopens the session logged at `log`, quoted for the shell when needed.
fn resume_command(log: &Path) -> String {
    let path = log.to_string_lossy();
    let plain = path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-@+:,=".contains(c));
    if plain { format!("killi -r {path}") } else { format!("killi -r '{}'", path.replace('\'', r"'\''")) }
}

/// Runs one future that drives the agent, showing its events and handling keys meanwhile.
async fn run_turn<F>(
    app: &mut App,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    future: F,
) -> Result<Ended>
where
    F: Future<Output = Result<(String, Stop)>>,
{
    let mut future = Box::pin(future);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    // A /btw question asked meanwhile; the turn waits for its answer before it ends.
    let mut aside: Option<PendingAside> = None;
    let mut done = None;
    let ended = loop {
        if let Some(question) = app.aside_asked.take() {
            if aside.is_some() {
                app.notice("Still answering the last /btw question.");
            } else {
                let ask = app.controls.aside.borrow().ask(&question);
                app.aside = Some((question.clone(), Instant::now()));
                aside = Some((question, Box::pin(ask)));
            }
        }
        tokio::select! {
            biased;
            Some(event) = rx.recv() => app.on_agent(event),
            Some(Ok(event)) = keys.next() => {
                if app.on_key_busy(event) {
                    break done.take().map_or(Ended::Interrupted, Ended::Done);
                }
            }
            result = &mut future, if done.is_none() => done = Some(result),
            answer = async { aside.as_mut().expect("a question is asked").1.as_mut().await }, if aside.is_some() => {
                let (question, _) = aside.take().expect("a question is asked");
                app.show_aside(&question, answer);
            }
            _ = tick.tick() => app.spinner += 1,
        }
        if aside.is_none()
            && let Some(result) = done.take()
        {
            break Ended::Done(result);
        }
        app.draw()?;
    };
    drop(future);
    while let Ok(event) = rx.try_recv() {
        app.on_agent(event);
    }
    Ok(ended)
}

#[allow(clippy::too_many_arguments)]
async fn handle_command(
    app: &mut App,
    agent: &mut Agent,
    session: &mut Setup,
    snapshots: &mut Snapshots,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    cwd: &Path,
    command: &str,
    arg: &str,
) -> Result<bool> {
    match command {
        "exit" | "quit" | "q" => return Ok(false),
        "help" | "h" | "?" => app.help(),
        "btw" if arg.is_empty() => app.error("Ask with /btw <question>."),
        "btw" => {
            app.aside_asked = Some(arg.to_string());
            app.begin_busy();
            run_turn(app, keys, rx, std::future::ready(Ok((String::new(), Stop::Completed)))).await?;
            app.end_busy();
        }
        "plan" => app.toggle_plan(),
        "swarm" => match arg {
            "" => app.set_swarm(!app.controls.swarm.get()),
            "on" => app.set_swarm(true),
            "off" => app.set_swarm(false),
            task => {
                let was_on = app.controls.swarm.replace(true);
                let result = prompt_turn(app, agent, snapshots, keys, rx, &format!("/swarm {task}"), task).await;
                app.controls.swarm.set(was_on);
                result?;
            }
        },
        "undo" => match snapshots.undo() {
            Ok(changes) if changes.is_empty() => app.notice("Undone. No files had changed in the last turn."),
            Ok(changes) => {
                app.notice(&format!("Undone: {} file(s) restored to how they were before the last turn.", changes.len()));
                for change in changes.iter().take(20) {
                    app.commit(format!("  {DIM}{change}{RESET}"));
                }
                agent.mark_interrupted();
            }
            Err(e) => app.error(&format!("Can't undo: {e:#}")),
        },
        "compact" => {
            let focus = arg.to_string();
            app.begin_busy();
            let ended = run_turn(app, keys, rx, async { agent.compact(Some(&focus)).await.map(|()| (String::new(), Stop::Completed)) }).await?;
            app.end_busy();
            if let Ended::Done(Err(e)) = ended {
                app.error(&format!("Compaction failed: {e:#}"));
            }
        }
        "clear" | "new" => match session.start_on(agent.client(), agent.controls.clone(), agent_sink(agent)) {
            Ok(fresh) => {
                let saved = !agent.user_prompts().is_empty();
                agent.discard_if_unused();
                *agent = fresh;
                app.show_model(agent, session.config());
                app.todos.clear();
                app.reset_usage();
                app.wipe_screen();
                app.banner(agent, session, snapshots, false);
                app.notice(if saved { "Started a new session. The last one is saved; /resume picks it up again." } else { "Started a new session." });
            }
            Err(e) => app.error(&format!("{e:#}")),
        },
        "resume" => app.open_session_picker(cwd),
        "later" if arg.is_empty() => app.show_later(cwd),
        "later" => match arg.strip_prefix("done").and_then(|rest| rest.trim().parse::<usize>().ok()) {
            Some(number) => match crate::later::done(cwd, number) {
                Ok(item) => app.notice(&format!("Crossed off: {item}")),
                Err(e) => app.error(&format!("{e:#}")),
            },
            None => app.error("Use /later to see the list, or /later done 3 to cross one off."),
        },
        "prune" if arg.is_empty() => app.open_prune_picker(&agent.session.log_path),
        "prune" => match arg.parse::<usize>() {
            Ok(keep) => app.next_input = Some(Input::Prune(keep)),
            Err(_) => app.error("Say how many of the newest sessions to keep: /prune 20."),
        },
        "mcp" if arg.is_empty() => app.open_mcp_picker(session.mcp(), None),
        "mcp" => app.next_input = Some(Input::Mcp(arg.to_string(), false)),
        "model" if arg.is_empty() => app.open_model_picker(session.config()),
        "model" => app.next_input = Some(Input::Model(arg.to_string())),
        "effort" | "thinking" if arg.is_empty() => app.open_effort_picker(session.config()),
        "effort" | "thinking" => app.next_input = Some(Input::Effort(arg.to_lowercase())),
        "reload" => match session.reload() {
            Ok(()) => {
                session.reload_agent(agent)?;
                app.entries = entries(session);
                app.refresh_choices(session.config());
                app.redraw_banner(agent, session, snapshots);
                app.notice("Reloaded the system prompt ([prompt] in config.toml), AGENTS.md files and skills. The next request starts the cache over.");
                if session.skills_need_restart() {
                    app.notice("This session started without skills, so the model can't load them until killi restarts; /name runs one now.");
                }
            }
            Err(e) => app.error(&format!("Can't reload: {e:#}")),
        },
        "usage" | "cost" => app.usage_report(agent),
        "session" | "share" => app.share_session(agent),
        other => match session.skill_prompt(other.strip_prefix("skill:").unwrap_or(other), arg) {
            Some(prompt) => prompt_turn(app, agent, snapshots, keys, rx, format!("/{other} {arg}").trim_end(), &prompt).await?,
            None => app.error(&format!("Unknown command /{other}. Type /help for the list.")),
        },
    }
    Ok(true)
}

/// Runs one turn started by a command: `shown` goes in the transcript, `prompt` to the agent.
async fn prompt_turn(
    app: &mut App,
    agent: &mut Agent,
    snapshots: &mut Snapshots,
    keys: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Event>,
    shown: &str,
    prompt: &str,
) -> Result<()> {
    app.begin_turn(shown);
    app.draw()?;
    snapshots.take();
    let before = Totals::of(agent);
    let ended = run_turn(app, keys, rx, agent.run(prompt, &[])).await?;
    app.finish_turn(ended, before, Totals::of(agent));
    if matches!(app.last_end, Some(LastEnd::Interrupted)) {
        agent.recover()?;
    }
    Ok(())
}

fn agent_sink(agent: &Agent) -> Sink {
    agent.sink()
}

/// What the slash menu searches: the commands, then every skill not named like one.
fn entries(session: &Setup) -> Vec<Entry> {
    let commands = COMMANDS.iter().map(|c| Entry {
        name: c.name.to_string(),
        aliases: c.aliases.iter().map(|a| a.to_string()).collect(),
        description: if c.args.is_empty() { c.about.to_string() } else { format!("{} — {}", c.args, c.about) },
    });
    let skills = session.skills().iter().filter(|s| !is_command(&s.name)).map(|s| Entry {
        name: s.name.clone(),
        aliases: Vec::new(),
        description: s.description.clone(),
    });
    commands.chain(skills).collect()
}

/// Session totals before and after a turn, to report what the turn cost.
#[derive(Clone, Copy)]
struct Totals {
    requests: u32,
    prompt: u64,
    cached: u64,
    completion: u64,
}

impl Totals {
    fn of(agent: &Agent) -> Self {
        let usage = &agent.stats.usage;
        Self { requests: agent.stats.requests, prompt: usage.prompt, cached: usage.cached, completion: usage.completion }
    }
}

enum Input {
    /// A message to the model, with the images pasted into it.
    Prompt(String, Vec<Image>),
    Command(String, String),
    Resume(PathBuf),
    Model(String),
    Effort(String),
    /// Turns an MCP server on or off; true when picked from the /mcp list, which then reopens.
    Mcp(String, bool),
    /// Deletes every session but the newest this many and the current one.
    Prune(usize),
}

enum LastEnd {
    Done,
    Interrupted,
}

struct Running {
    id: String,
    summary: String,
    label: Option<String>,
    started: Instant,
}

struct Asking {
    question: Question,
    selected: usize,
}

struct Picker {
    title: String,
    /// Each option's name, a note shown beside it, and what choosing it does.
    options: Vec<(String, String, PickerChoice)>,
    /// What has been typed to narrow the options, matched against their names.
    query: String,
    /// Indexes into `options` of those matching `query`, best first.
    shown: Vec<usize>,
    /// Position in `shown`.
    selected: usize,
}

impl Picker {
    fn new(title: String, options: Vec<(String, String, PickerChoice)>, selected: usize) -> Picker {
        let shown = (0..options.len()).collect();
        Picker { title, options, query: String::new(), shown, selected }
    }

    /// Narrows the options to those fuzzy-matching `query` and selects the best.
    fn filter(&mut self, query: String) {
        let all: Vec<usize> = (0..self.options.len()).collect();
        self.shown = menu::fuzzy_filter(&all, &query, |&i| &self.options[i].0);
        self.query = query;
        self.selected = 0;
    }

    fn choice(&self) -> Option<&PickerChoice> {
        self.shown.get(self.selected).map(|&i| &self.options[i].2)
    }
}

#[derive(Clone)]
enum PickerChoice {
    Session(PathBuf),
    Model(String),
    Effort(String),
    Mcp(String),
    Prune(usize),
}

#[derive(Default)]
struct Status {
    model: String,
    /// The thinking effort, as `Thinking::label` names it.
    thinking: String,
    window: Option<u64>,
    context: u64,
    branch: Option<String>,
    prompt: u64,
    cached: u64,
    completion: u64,
}

struct App {
    out: Stdout,
    width: usize,
    height: usize,
    /// Rows from the top of the live area to the cursor, to find the area again.
    cursor_row: usize,
    /// The live area's rows and cursor column as last painted, so a tick repaints only what
    /// changed and the rows that didn't, such as the status line, never blink.
    shown: Vec<String>,
    shown_col: usize,
    /// Lines waiting to be printed above the live area.
    pending: Vec<String>,
    editor: Editor,
    markdown: Markdown,
    /// Streamed text after the last line break.
    partial: String,
    /// A blank line in the model's text, printed only if more text follows it.
    held_blank: bool,
    /// The last line printed was blank.
    last_blank: bool,
    last_kind: Kind,
    thinking: Option<(Instant, String)>,
    running: Vec<Running>,
    todos: Vec<Todo>,
    asking: Option<Asking>,
    picker: Option<Picker>,
    /// Input to act on before reading keys again, such as `/model <alias>`.
    next_input: Option<Input>,
    /// What the slash menu searches, and the choices it offers after /model and /effort.
    entries: Vec<Entry>,
    model_choices: Vec<Item>,
    effort_choices: Vec<Item>,
    menu_selected: usize,
    /// The text the menu's selection belongs to; another text starts it over.
    menu_text: String,
    /// The text the menu was closed on with Esc; it opens again when the text changes.
    menu_closed: Option<String>,
    busy: Option<Instant>,
    /// The latest tool outputs and thoughts, for Ctrl+O.
    details: Vec<(String, String)>,
    /// How far back Ctrl+O has gone from the newest.
    detail_back: usize,
    status: Status,
    controls: Controls,
    spinner: usize,
    exit_armed: Option<Instant>,
    queued: Vec<String>,
    /// A /btw question for the running turn to send.
    aside_asked: Option<String>,
    /// The running turn called `later`, so a reply that leaves something for later noted it.
    later_noted: bool,
    /// The /btw question being answered and since when, for the live area.
    aside: Option<(String, Instant)>,
    /// Answered /btw blocks waiting for the model's paragraph to end before they are printed.
    held_asides: Vec<Vec<String>>,
    last_end: Option<LastEnd>,
    cwd: PathBuf,
    notify_after: Duration,
    /// Whether the model takes images, so a pasted one is attached or turned away.
    sees_images: bool,
    /// The banner and what was printed after it, while its top row is on screen.
    since_banner: Option<SinceBanner>,
}

/// The lines printed from the banner on, so it can be drawn again in place.
#[derive(Default)]
struct SinceBanner {
    /// How many of `lines` the banner itself is.
    banner: usize,
    lines: Vec<String>,
    /// The screen rows `lines` take.
    rows: usize,
    resumed: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum Kind {
    None,
    Text,
    /// Tool calls, thoughts and retries: the work between prose.
    Tool,
    /// A notice, an error, a question or a block like /help: set apart from what is around it.
    Note,
}

impl App {
    fn new(agent: &Agent, controls: Controls, cwd: &Path) -> Self {
        let (columns, rows) = terminal::size().unwrap_or((100, 40));
        let mut app = Self {
            out: stdout(),
            width: usize::from(columns).max(20),
            height: usize::from(rows).max(5),
            cursor_row: 0,
            shown: Vec::new(),
            shown_col: 0,
            pending: Vec::new(),
            editor: Editor::with_history(agent.user_prompts()),
            markdown: Markdown::new(usize::from(columns).max(20)),
            partial: String::new(),
            held_blank: false,
            last_blank: false,
            last_kind: Kind::None,
            thinking: None,
            running: Vec::new(),
            todos: Vec::new(),
            asking: None,
            picker: None,
            next_input: None,
            entries: Vec::new(),
            model_choices: Vec::new(),
            effort_choices: Vec::new(),
            menu_selected: 0,
            menu_text: String::new(),
            menu_closed: None,
            busy: None,
            details: Vec::new(),
            detail_back: 0,
            status: Status {
                model: agent.model_alias().to_string(),
                thinking: agent.thinking().label().to_string(),
                window: agent.context_window(),
                context: agent.stats.context,
                ..Status::default()
            },
            controls,
            spinner: 0,
            exit_armed: None,
            queued: Vec::new(),
            aside_asked: None,
            later_noted: false,
            aside: None,
            held_asides: Vec::new(),
            last_end: None,
            cwd: cwd.to_path_buf(),
            notify_after: Duration::from_secs(10),
            sees_images: agent.sees_images(),
            since_banner: None,
        };
        app.refresh_branch();
        app
    }

    fn banner(&mut self, agent: &Agent, session: &Setup, snapshots: &Snapshots, resumed: bool) {
        let cwd = tilde(&self.cwd);
        let room = self.width.saturating_sub(41).max(20);
        let cwd = if text::width(&cwd) > room {
            let tail: String = cwd.chars().rev().take(room - 1).collect::<Vec<_>>().into_iter().rev().collect();
            format!("…{tail}")
        } else {
            cwd
        };
        let thinking = match thinking_parts(agent.thinking().label()) {
            Some((word, value)) => format!(" {DIM}· {word}{RESET} {EFFORT}{value}{RESET}"),
            None => String::new(),
        };
        let overview = session.overview();
        let mut side = vec![
            format!("{BOLD}killi{RESET} {DIM}{}{RESET}", env!("CARGO_PKG_VERSION")),
            format!("{DIM}a coding agent for the terminal{RESET}"),
            String::new(),
            format!("{}{BOLD}{}{RESET}{thinking}", label("model"), agent.model_alias()),
            format!("{}{cwd}", label("folder")),
        ];
        side.extend(overview_rows(&overview));
        self.since_banner = self.pending.is_empty().then(|| SinceBanner { resumed, ..SinceBanner::default() });
        self.commit(String::new());
        for row in banner::render(self.width, &side) {
            self.commit(row);
        }
        self.commit(String::new());
        for problem in &overview.problems {
            self.commit(format!("{WARN}!{RESET} {problem}"));
        }
        if let Some(why) = &snapshots.disabled {
            self.commit(format!("{DIM}{why}{RESET}"));
        }
        if resumed {
            let prompts = agent.user_prompts();
            self.commit(format!("{DIM}Resumed a session with {} prompt(s); the last was “{}”.{RESET}", prompts.len(), prompts.last().map_or("", |s| s.lines().next().unwrap_or(""))));
        }
        self.commit(keys_hint());
        self.commit(String::new());
        self.last_kind = Kind::None;
        if let Some(since) = &mut self.since_banner {
            since.banner = self.pending.len();
        }
    }

    /// Draws the banner again with the model, effort and setup now in use, when its top row is
    /// still on screen; everything printed below it is printed again as it was.
    fn redraw_banner(&mut self, agent: &Agent, session: &Setup, snapshots: &Snapshots) {
        let Some(since) = self.since_banner.take() else { return };
        if since.rows + self.shown.len() > self.height {
            return;
        }
        let _ = queue!(self.out, MoveUp((self.cursor_row + since.rows) as u16), MoveToColumn(0), Clear(ClearType::FromCursorDown));
        self.cursor_row = 0;
        self.shown.clear();
        let waiting = std::mem::take(&mut self.pending);
        let (last_kind, last_blank) = (self.last_kind, self.last_blank);
        self.last_blank = false;
        self.banner(agent, session, snapshots, since.resumed);
        if since.lines.len() > since.banner || !waiting.is_empty() {
            self.pending.extend(since.lines.into_iter().skip(since.banner));
            self.pending.extend(waiting);
            (self.last_kind, self.last_blank) = (last_kind, last_blank);
        }
    }

    // ---------- input ----------

    async fn read_input(&mut self, keys: &mut EventStream) -> Result<Option<Input>> {
        if let Some(input) = self.next_input.take() {
            return Ok(Some(input));
        }
        loop {
            let Some(event) = keys.next().await else { return Ok(None) };
            let event = event?;
            let result = self.on_key_idle(event);
            self.sync_menu();
            self.draw()?;
            match result {
                Idle::Continue => {}
                Idle::Exit => return Ok(None),
                Idle::Input(input) => return Ok(Some(input)),
            }
        }
    }

    fn on_key_idle(&mut self, event: TermEvent) -> Idle {
        let key = match event {
            TermEvent::Paste(text) => {
                self.paste(&text);
                return Idle::Continue;
            }
            TermEvent::Resize(columns, rows) => {
                self.resize(columns, rows);
                return Idle::Continue;
            }
            TermEvent::Key(key) if key.kind != KeyEventKind::Release => key,
            _ => return Idle::Continue,
        };
        if self.picker.is_some() {
            return self.on_key_picker(key);
        }
        if let Some(menu) = self.suggestions()
            && let Some(idle) = self.on_key_menu(key, &menu)
        {
            return idle;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => {
                if !self.editor.is_empty() {
                    self.editor.clear();
                } else if self.exit_armed.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                    return Idle::Exit;
                } else {
                    self.exit_armed = Some(Instant::now());
                    self.notice("Press Ctrl+C again to exit.");
                }
            }
            KeyCode::Char('d') if ctrl && self.editor.is_empty() => return Idle::Exit,
            KeyCode::Enter if !newline_key(&key) => {
                if self.editor.text().ends_with('\\') {
                    self.editor.backspace();
                    self.editor.insert("\n");
                    return Idle::Continue;
                }
                return self.submit();
            }
            _ => self.edit_key(key),
        }
        Idle::Continue
    }

    /// Sends the prompt: a command when it starts with `/`, else a message to the model.
    fn submit(&mut self) -> Idle {
        let (text, images) = self.editor.take_with_images();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Idle::Continue;
        }
        if let Some(command) = trimmed.strip_prefix('/') {
            let (name, arg) = command.split_once(char::is_whitespace).unwrap_or((command, ""));
            return Idle::Input(Input::Command(name.to_string(), arg.trim().to_string()));
        }
        Idle::Input(Input::Prompt(text, images))
    }

    fn on_key_picker(&mut self, key: KeyEvent) -> Idle {
        let picker = self.picker.as_mut().expect("picker is open");
        match key.code {
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(picker.shown.len().saturating_sub(1)),
            KeyCode::Esc => self.picker = None,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.picker = None,
            KeyCode::Backspace => {
                let mut query = picker.query.clone();
                query.pop();
                picker.filter(query);
            }
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                let query = format!("{}{c}", picker.query);
                picker.filter(query);
            }
            KeyCode::Enter | KeyCode::Tab => {
                if let Some(choice) = picker.choice().cloned() {
                    self.picker = None;
                    return Idle::Input(match choice {
                        PickerChoice::Session(path) => Input::Resume(path),
                        PickerChoice::Model(alias) => Input::Model(alias),
                        PickerChoice::Effort(effort) => Input::Effort(effort),
                        PickerChoice::Mcp(name) => Input::Mcp(name, true),
                        PickerChoice::Prune(keep) => Input::Prune(keep),
                    });
                }
            }
            _ => {}
        }
        Idle::Continue
    }

    /// Keys for the slash menu: ↑↓ choose; Tab or Enter runs a command at the start of the
    /// prompt, completes a skill or a later command and fills in an argument; Esc closes it (the
    /// `/` is then plain text). None lets the key through.
    fn on_key_menu(&mut self, key: KeyEvent, menu: &Suggestions) -> Option<Idle> {
        let count = menu.items.len();
        let selected = self.menu_selected.min(count - 1);
        match key.code {
            KeyCode::Up => self.menu_selected = (selected + count - 1) % count,
            KeyCode::Down => self.menu_selected = (selected + 1) % count,
            KeyCode::Esc => self.menu_closed = Some(self.editor.text().to_string()),
            KeyCode::Tab | KeyCode::Enter if !newline_key(&key) => {
                self.choose(menu, selected);
                if runs_at_once(menu, selected) {
                    return Some(self.submit());
                }
            }
            _ => return None,
        }
        Some(Idle::Continue)
    }

    fn choose(&mut self, menu: &Suggestions, index: usize) {
        let value = &menu.items[index].value;
        let with = if menu.command { format!("/{value} ") } else { value.clone() };
        self.editor.replace_before(menu.start, &with);
    }

    /// The slash menu for the prompt as typed, while killi is idle.
    fn suggestions(&self) -> Option<Suggestions> {
        if self.busy.is_some() || self.picker.is_some() || self.asking.is_some() || self.editor.browsing() {
            return None;
        }
        if self.menu_closed.as_deref() == Some(self.editor.text()) {
            return None;
        }
        menu::suggest(self.editor.before_cursor(), &self.entries, |command| match command {
            "model" => self.model_choices.clone(),
            "effort" | "thinking" => self.effort_choices.clone(),
            _ => Vec::new(),
        })
    }

    /// Starts the menu's selection over when the text changes: on the first item, or on the
    /// current model or effort when it offers those.
    fn sync_menu(&mut self) {
        if self.editor.text() == self.menu_text {
            return;
        }
        self.menu_text = self.editor.text().to_string();
        let current = |item: &Item| item.value == self.status.model || item.value == self.status.thinking;
        self.menu_selected = self.suggestions().filter(|m| !m.command).and_then(|m| m.items.iter().position(current)).unwrap_or(0);
    }

    /// Shows the model and effort the agent runs on in the status line and the slash menu.
    fn show_model(&mut self, agent: &Agent, config: &Config) {
        self.sees_images = agent.sees_images();
        self.status.model = agent.model_alias().to_string();
        self.status.thinking = agent.thinking().label().to_string();
        self.status.window = agent.context_window();
        self.refresh_choices(config);
    }

    /// Remembers the model and effort just picked for the sessions started after this one.
    fn save_choice(&mut self, agent: &Agent) {
        let choice = Choice { model: Some(agent.model_alias().to_string()), effort: Some(agent.thinking().label().to_string()) };
        if let Err(e) = crate::session::save_choice(&choice) {
            self.error(&format!("{e:#}"));
        }
    }

    fn refresh_choices(&mut self, config: &Config) {
        let mark = |about: &str, current: bool| match (about.is_empty(), current) {
            (_, false) => about.to_string(),
            (true, true) => "current".to_string(),
            (false, true) => format!("{about} · current"),
        };
        self.model_choices = config
            .models
            .iter()
            .map(|(alias, model)| Item { value: alias.clone(), description: mark(&model.provider, *alias == self.status.model) })
            .collect();
        self.effort_choices = config
            .efforts(&self.status.model)
            .into_iter()
            .map(|effort| {
                let about = match effort.as_str() {
                    "off" => "no thinking",
                    "on" => "the provider's default effort",
                    _ => "",
                };
                let description = mark(about, effort == self.status.thinking);
                Item { value: effort, description }
            })
            .collect();
    }

    /// Keys while the agent works. Returns true to interrupt it.
    fn on_key_busy(&mut self, event: TermEvent) -> bool {
        let key = match event {
            TermEvent::Paste(text) => {
                self.paste(&text);
                return false;
            }
            TermEvent::Resize(columns, rows) => {
                self.resize(columns, rows);
                return false;
            }
            TermEvent::Key(key) if key.kind != KeyEventKind::Release => key,
            _ => return false,
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.asking.is_some() {
            return self.on_key_asking(key);
        }
        match key.code {
            KeyCode::Esc => return true,
            KeyCode::Char('c') if ctrl => return true,
            KeyCode::Enter if !newline_key(&key) => {
                if let Some(question) = aside_question(self.editor.text()) {
                    self.aside_asked = Some(question);
                    self.editor.take();
                    return false;
                }
                if self.editor.text().trim_start().starts_with('/') {
                    self.notice("Commands run when the agent is idle, except /btw: wait for this turn to finish, or press Esc.");
                    return false;
                }
                let (text, images) = self.editor.take_with_images();
                if !text.trim().is_empty() {
                    self.controls.steer.borrow_mut().push_back((text.clone(), images));
                    self.queued.push(text);
                }
            }
            _ => self.edit_key(key),
        }
        false
    }

    fn on_key_asking(&mut self, key: KeyEvent) -> bool {
        let asking = self.asking.as_mut().expect("a question is open");
        let count = asking.question.options.len();
        match key.code {
            KeyCode::Tab if self.editor.is_empty() => self.answer(None),
            KeyCode::Up if self.editor.is_empty() => asking.selected = asking.selected.saturating_sub(1),
            KeyCode::Down if self.editor.is_empty() => asking.selected = (asking.selected + 1).min(count.saturating_sub(1)),
            KeyCode::Char(c) if self.editor.is_empty() && c.is_ascii_digit() && (1..=count).contains(&(c as usize - '0' as usize)) => {
                asking.selected = c as usize - '1' as usize;
                self.answer(None);
            }
            KeyCode::Esc => {
                let asking = self.asking.take().expect("a question is open");
                let _ = asking.question.reply.send(String::new());
                self.commit(format!("{DIM}  (dismissed){RESET}"));
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Enter if !newline_key(&key) => {
                let typed = self.editor.take();
                self.answer(Some(typed).filter(|t| !t.trim().is_empty()));
            }
            _ => self.edit_key(key),
        }
        false
    }

    fn answer(&mut self, typed: Option<String>) {
        let asking = self.asking.take().expect("a question is open");
        let answer = typed.unwrap_or_else(|| asking.question.options.get(asking.selected).cloned().unwrap_or_default());
        self.commit(format!("  {FIN}↳{RESET} {answer}"));
        let _ = asking.question.reply.send(answer);
    }

    /// Keys that edit the prompt, the same whether or not the agent works.
    fn edit_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let e = &mut self.editor;
        match key.code {
            KeyCode::Enter => e.insert("\n"),
            KeyCode::Char('j') if ctrl => e.insert("\n"),
            KeyCode::Char('a') if ctrl => e.home(),
            KeyCode::Char('e') if ctrl => e.end(),
            KeyCode::Char('u') if ctrl => e.kill_to_start(),
            KeyCode::Char('k') if ctrl => e.kill_to_end(),
            KeyCode::Char('w') if ctrl => e.delete_word(),
            KeyCode::Char('b') if alt => e.word_left(),
            KeyCode::Char('f') if alt => e.word_right(),
            KeyCode::Char('o') if ctrl => self.show_detail(),
            KeyCode::Char('v') if ctrl => self.paste_clipboard_image(),
            KeyCode::Char('l') if ctrl => {
                let _ = execute!(self.out, Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0));
                self.cursor_row = 0;
                self.shown.clear();
                self.since_banner = None;
            }
            KeyCode::Char(c) if !ctrl => e.insert(c.encode_utf8(&mut [0; 4])),
            KeyCode::Backspace if alt => e.delete_word(),
            KeyCode::Backspace => e.backspace(),
            KeyCode::Delete => e.delete(),
            KeyCode::Left if alt || ctrl => e.word_left(),
            KeyCode::Right if alt || ctrl => e.word_right(),
            KeyCode::Left => e.left(),
            KeyCode::Right => e.right(),
            KeyCode::Home => e.home(),
            KeyCode::End => e.end(),
            KeyCode::Up => e.up(),
            KeyCode::Down => e.down(),
            KeyCode::BackTab => self.toggle_plan(),
            KeyCode::Tab => self.complete_command(),
            _ => {}
        }
    }

    /// A paste goes in as text, except an empty one, which Kil sends for Cmd+V when the
    /// clipboard holds only an image: that attaches the image.
    fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return self.paste_clipboard_image();
        }
        self.editor.insert(text);
    }

    /// Ctrl+V: attaches the clipboard's image to the prompt as an `[Image #N]` placeholder.
    fn paste_clipboard_image(&mut self) {
        if !self.sees_images {
            return self.notice(&format!("{} can't see images; /model switches to one that can.", self.status.model));
        }
        if self.asking.is_some() {
            return self.notice("An answer is text only; send the image with your next message.");
        }
        match paste::clipboard_image() {
            Ok(url) => self.editor.attach(url),
            Err(why) => self.notice(&why),
        }
    }

    fn complete_command(&mut self) {
        let text = self.editor.text().to_string();
        let Some(partial) = text.strip_prefix('/').filter(|p| !p.contains(char::is_whitespace)) else { return };
        if let Some(name) = COMMANDS.iter().map(|c| c.name).find(|name| name.starts_with(partial)) {
            self.editor.clear();
            self.editor.insert(&format!("/{name} "));
        }
    }

    fn toggle_plan(&mut self) {
        let on = !self.controls.plan.get();
        self.controls.plan.set(on);
        self.notice(if on {
            "Plan mode on: the agent researches and plans; writes are blocked until you approve its plan."
        } else {
            "Plan mode off."
        });
    }

    fn set_swarm(&mut self, on: bool) {
        self.controls.swarm.set(on);
        self.notice(if on {
            "Swarm mode on: the agent splits the work across many subagents with agent_swarm."
        } else {
            "Swarm mode off."
        });
    }

    // ---------- agent events ----------

    fn begin_turn(&mut self, prompt: &str) {
        self.commit_prompt(prompt);
        self.begin_busy();
    }

    /// Prints what the user typed as their message block.
    fn commit_prompt(&mut self, prompt: &str) {
        self.commit(String::new());
        let mut first = true;
        for line in prompt.lines() {
            for row in text::wrap_words(line, self.width.saturating_sub(3), 0) {
                let lead = if std::mem::take(&mut first) { format!("{FIN}❯{RESET}{TINT}{BOLD}") } else { format!(" {BOLD}") };
                let pad = " ".repeat(self.width.saturating_sub(3 + text::width(&row)));
                self.commit(format!("{TINT} {lead} {row}{pad}{RESET}"));
            }
        }
        self.commit(String::new());
        self.last_kind = Kind::None;
    }

    /// Prints a resumed session's conversation into the scrollback, as it read when it ran.
    fn replay_log(&mut self, log: &Path) {
        let loaded = match crate::session::load(log) {
            Ok(loaded) => loaded,
            Err(e) => return self.error(&format!("{e:#}")),
        };
        for shown in crate::session::replay(&loaded, &self.cwd, REPLAY_PROMPTS) {
            match shown {
                Replayed::Note(text) => self.notice(&text),
                Replayed::Prompt(prompt) => {
                    let shown: Vec<&str> = prompt.lines().take(REPLAY_PROMPT_ROWS).collect();
                    self.commit_prompt(&shown.join("\n"));
                    let more = prompt.lines().count().saturating_sub(shown.len());
                    if more > 0 {
                        self.notice(&format!("… {more} more line{}", if more == 1 { "" } else { "s" }));
                    }
                }
                Replayed::Text(text) => {
                    self.on_agent(Event::Text(text));
                    self.flush_partial();
                }
                Replayed::Tool { summary, output, is_error, brief } => {
                    self.on_agent(Event::ToolStart { id: "replay".into(), summary });
                    self.on_agent(Event::ToolEnd { id: "replay".into(), is_error, brief, display: None, output });
                }
            }
        }
        self.commit(String::new());
        self.last_kind = Kind::None;
    }

    fn begin_busy(&mut self) {
        self.busy = Some(Instant::now());
        self.spinner = 0;
        self.later_noted = false;
        self.last_end = None;
    }

    fn end_busy(&mut self) {
        self.flush_partial();
        self.end_thinking();
        self.running.clear();
        self.aside = None;
        self.busy = None;
    }

    fn finish_turn(&mut self, ended: Ended, before: Totals, after: Totals) {
        let elapsed = self.busy.map(|t| t.elapsed()).unwrap_or_default();
        self.end_busy();
        if let Some(asking) = self.asking.take() {
            let _ = asking.question.reply.send(String::new());
        }
        self.queued.clear();
        match ended {
            Ended::Interrupted => {
                self.last_end = Some(LastEnd::Interrupted);
                self.begin(Kind::Note);
                self.commit(format!("{WARN}Interrupted.{RESET} {DIM}Tell killi what to do instead.{RESET}"));
                self.controls.steer.borrow_mut().clear();
            }
            Ended::Done(Err(e)) => {
                self.last_end = Some(LastEnd::Interrupted);
                self.error(&gist(&format!("{e:#}")));
            }
            Ended::Done(Ok((text, stop))) => {
                self.last_end = Some(LastEnd::Done);
                match stop {
                    Stop::MaxTokens => self.notice("The reply hit the output token limit."),
                    Stop::RepeatBreaker => self.notice("Stopped: the agent kept repeating the same tool call. Tell it what to read or run instead, or pick another model with /model."),
                    Stop::Completed => {}
                }
                if !self.later_noted && leaves_something_for_later(&text) {
                    self.notice("The reply leaves something for later that wasn't noted; ask killi to note it with `later`, or see /later.");
                }
            }
        }
        let prompt = after.prompt - before.prompt;
        let cached = (after.cached - before.cached) * 100 / prompt.max(1);
        self.commit(String::new());
        let requests = after.requests - before.requests;
        let quick = matches!(self.last_end, Some(LastEnd::Done)) && elapsed < Duration::from_secs(3) && requests <= 1;
        let spent = prompt > 0 || after.completion > before.completion;
        if spent && !quick {
            self.commit(format!(
                "{DIM}{} · {requests} request{} · {} in, {cached}% cached · {} out{RESET}",
                clock(elapsed),
                if requests == 1 { "" } else { "s" },
                short(prompt),
                short(after.completion - before.completion)
            ));
        }
        self.last_kind = Kind::None;
        if elapsed >= self.notify_after {
            notify(match self.last_end {
                Some(LastEnd::Done) => "Done",
                _ => "Stopped",
            });
        }
    }

    fn on_agent(&mut self, event: Event) {
        match event {
            Event::Text(text) => {
                let text = if self.last_kind == Kind::Text { text.as_str() } else { text.trim_start() };
                if text.is_empty() {
                    return;
                }
                self.end_thinking();
                self.begin(Kind::Text);
                self.partial.push_str(text);
                while let Some(end) = self.partial.find('\n') {
                    let line: String = self.partial.drain(..=end).collect();
                    let line = line.trim_end_matches('\n');
                    if line.trim().is_empty() {
                        let table = self.markdown.finish();
                        self.commit_markdown(table);
                        self.held_blank = true;
                        self.release_asides(false);
                        continue;
                    }
                    if std::mem::take(&mut self.held_blank) {
                        self.commit(String::new());
                    }
                    let styled = self.markdown.line(line);
                    self.commit_markdown(styled);
                }
            }
            Event::Reasoning(text) => match &mut self.thinking {
                Some((_, buffer)) => buffer.push_str(&text),
                None => self.thinking = Some((Instant::now(), text)),
            },
            Event::Retry { attempt, wait, reason } => {
                let line = format!("{WARN}⚠ retry {attempt} in {}:{RESET}{DIM} {}{RESET}", clock(wait), gist(&reason));
                self.begin(Kind::Tool);
                self.commit(fit(&line, self.width));
            }
            Event::ToolStart { id, summary, .. } => {
                if summary == "later" || summary.starts_with("later ") {
                    self.later_noted = true;
                }
                self.flush_partial();
                self.end_thinking();
                self.begin(Kind::Tool);
                self.running.push(Running { id, summary, label: None, started: Instant::now() });
            }
            Event::ToolEnd { id, is_error, brief, display, output } => {
                let running = self.take_running(&id);
                let elapsed = running.as_ref().map(|r| r.started.elapsed());
                let summary = running.map(|r| r.summary).unwrap_or_default();
                self.tool_line(None, &summary, is_error, &brief, elapsed);
                if display.is_none() && summary.starts_with("bash ") {
                    self.output_preview(&output, is_error);
                }
                if let Some(display) = display {
                    let lines: Vec<&str> = display.lines().collect();
                    for line in lines.iter().take(DIFF_ROWS) {
                        self.commit(fit(&diff_line(line), self.width));
                    }
                    if lines.len() > DIFF_ROWS {
                        let more = lines.len() - DIFF_ROWS;
                        self.commit(format!("  {FAINT}│{RESET} {DIM}… {more} more line{} · Ctrl+O{RESET}", if more == 1 { "" } else { "s" }));
                    }
                    self.push_detail(summary, display);
                } else {
                    self.push_detail(summary, output);
                }
            }
            Event::Usage(usage) => self.add_usage(&usage, true),
            Event::Todos(todos) => self.todos = todos,
            Event::Ask(question) => {
                self.flush_partial();
                self.end_thinking();
                self.begin(Kind::Note);
                if question.plan {
                    self.commit(format!("{BOLD}{FIN}? Plan{RESET}"));
                    let mut markdown = Markdown::new(self.width.saturating_sub(2));
                    let mut rows: Vec<String> = question.text.lines().flat_map(|line| markdown.line(line)).collect();
                    rows.extend(markdown.finish());
                    for row in rows {
                        self.commit_wrapped(format!("  {row}"));
                    }
                    self.commit(String::new());
                } else {
                    for row in text::wrap_words(&format!("{BOLD}{FIN}?{RESET} {BOLD}{}{RESET}", question.text), self.width, 2) {
                        self.commit(row);
                    }
                }
                self.asking = Some(Asking { question, selected: 0 });
                notify("Waiting for your answer");
            }
            Event::Notice(text) => self.notice(&text),
            Event::Sub { label, event } => self.on_sub(label, *event),
        }
    }

    fn on_sub(&mut self, label: String, event: Event) {
        match event {
            Event::ToolStart { id, summary, .. } => {
                self.running.push(Running { id, summary, label: Some(label), started: Instant::now() });
            }
            Event::ToolEnd { id, is_error, brief, .. } => {
                let running = self.take_running(&id);
                let elapsed = running.as_ref().map(|r| r.started.elapsed());
                let summary = running.map(|r| r.summary).unwrap_or_default();
                self.tool_line(Some(&label), &summary, is_error, &brief, elapsed);
            }
            Event::Usage(usage) => self.add_usage(&usage, false),
            Event::Notice(text) => self.notice(&format!("{label}: {text}")),
            Event::Ask(question) => {
                let _ = question.reply.send(String::new());
            }
            _ => {}
        }
    }

    fn take_running(&mut self, id: &str) -> Option<Running> {
        let index = self.running.iter().position(|r| r.id == id)?;
        Some(self.running.remove(index))
    }

    fn tool_line(&mut self, label: Option<&str>, summary: &str, is_error: bool, brief: &str, elapsed: Option<Duration>) {
        let (glyph, note) = if is_error {
            (format!("{DANGER}✗{RESET}"), format!(" {DANGER}{brief}{RESET}"))
        } else if brief.is_empty() {
            (format!("{LAGOON}✓{RESET}"), String::new())
        } else {
            (format!("{LAGOON}✓{RESET}"), format!(" {DIM}· {brief}{RESET}"))
        };
        let took = elapsed.filter(|e| e.as_secs() >= 1).map(|e| format!(" {DIM}· {}{RESET}", precise_clock(e)));
        let note = format!("{note}{}", took.unwrap_or_default());
        let lead = label.map_or(0, |l| text::width(l) + 5);
        let summary = clip(summary, self.width.saturating_sub(lead + 2 + text::width(&note)).max(20));
        let line = match label {
            Some(label) => format!("  {DIM}↳ {label}:{RESET} {glyph} {DIM}{summary}{RESET}{note}"),
            None => format!("{glyph} {}{note}", tool_summary(&summary)),
        };
        self.begin(Kind::Tool);
        self.commit(fit(&line, self.width));
    }

    /// The first lines of a bash command's output under its line, or the last when it failed.
    fn output_preview(&mut self, output: &str, is_error: bool) {
        let lines: Vec<String> = output
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with("[Command "))
            .map(|l| l.chars().filter(|c| !c.is_control() || *c == '\t').collect::<String>().replace('\t', "    "))
            .collect();
        if lines.is_empty() || (lines.len() == 1 && lines[0].starts_with("(no output")) {
            return;
        }
        let skipped = lines.len().saturating_sub(PREVIEW_ROWS);
        let shown = if is_error { &lines[skipped..] } else { &lines[..lines.len() - skipped] };
        for line in shown {
            self.commit(fit(&format!("  {FAINT}│{RESET} {DIM}{line}{RESET}"), self.width));
        }
        if skipped > 0 {
            let which = if is_error { "earlier" } else { "more" };
            self.commit(format!("  {FAINT}│{RESET} {DIM}… {skipped} {which} line{} · Ctrl+O{RESET}", if skipped == 1 { "" } else { "s" }));
        }
    }

    fn push_detail(&mut self, title: String, body: String) {
        self.details.push((title, body));
        if self.details.len() > DETAIL_KEPT {
            self.details.remove(0);
        }
        self.detail_back = 0;
    }

    fn add_usage(&mut self, usage: &Usage, main: bool) {
        self.status.prompt += usage.prompt;
        self.status.cached += usage.cached;
        self.status.completion += usage.completion;
        if main {
            self.status.context = usage.prompt;
        }
    }

    fn reset_usage(&mut self) {
        self.status.prompt = 0;
        self.status.cached = 0;
        self.status.completion = 0;
        self.status.context = 0;
    }

    fn flush_partial(&mut self) {
        self.held_blank = false;
        if !self.partial.trim().is_empty() {
            let line = std::mem::take(&mut self.partial);
            let styled = self.markdown.line(&line);
            self.commit_markdown(styled);
        }
        self.partial.clear();
        let table = self.markdown.finish();
        self.commit_markdown(table);
        self.release_asides(true);
    }

    /// Ends a thought: one dim line when it took a while, with the full text behind Ctrl+O.
    fn end_thinking(&mut self) {
        if let Some((started, text)) = self.thinking.take() {
            let seconds = started.elapsed().as_secs_f32();
            let took = clock(started.elapsed());
            self.push_detail("Thinking".into(), text.clone());
            if seconds < 2.0 {
                return;
            }
            let first = first_sentence(&text);
            let gist = if first.is_empty() { String::new() } else { format!(": {first}") };
            let line = fit(&format!("{DIM}∴ Thought for {took}{gist}{RESET}"), self.width);
            self.begin(Kind::Tool);
            self.commit(line);
        }
    }

    fn show_detail(&mut self) {
        if self.details.is_empty() {
            self.notice("Nothing to expand yet.");
            return;
        }
        let back = self.detail_back % self.details.len();
        let (title, body) = self.details[self.details.len() - 1 - back].clone();
        self.detail_back = back + 1;
        let place = if self.details.len() > 1 { format!(" ({}/{})", back + 1, self.details.len()) } else { String::new() };
        self.begin(Kind::Note);
        self.commit(format!("{DIM}── {title}{place} ──{RESET}"));
        let lines: Vec<&str> = body.lines().collect();
        for line in lines.iter().take(400) {
            self.commit(format!("{DIM}{line}{RESET}"));
        }
        if lines.len() > 400 {
            self.commit(format!("{DIM}… {} more lines{RESET}", lines.len() - 400));
        }
        self.commit(String::new());
    }

    // ---------- commands ----------

    fn help(&mut self) {
        self.begin(Kind::Note);
        self.commit(format!("{BOLD}Commands{RESET} {DIM}type / to search them and your skills{RESET}"));
        let rows: Vec<(String, String, &str)> = COMMANDS
            .iter()
            .map(|c| {
                let aliases: String = c.aliases.iter().map(|a| format!(", /{a}")).collect();
                let args = if c.args.is_empty() { String::new() } else { format!(" {}", c.args) };
                (format!("/{}", c.name), format!("{aliases}{args}"), c.about)
            })
            .chain([("/<skill>".to_string(), " [request]".to_string(), "Run a skill")])
            .collect();
        let column = rows.iter().map(|(name, rest, _)| text::width(name) + text::width(rest)).max().unwrap_or(0);
        for (name, rest, about) in rows {
            let pad = " ".repeat(column - text::width(&name) - text::width(&rest));
            self.commit(format!("  {LAGOON}{name}{RESET}{DIM}{rest}{RESET}{pad}  {about}"));
        }
        self.commit(String::new());
        self.commit(format!("{BOLD}Keys{RESET}"));
        let column = KEYS.iter().map(|(key, _)| text::width(key)).max().unwrap_or(0);
        for (key, about) in KEYS {
            self.commit(format!("  {BOLD}{key:<column$}{RESET}  {DIM}{about}{RESET}"));
        }
        self.commit(String::new());
        self.last_kind = Kind::None;
    }

    /// A /btw answer, with the question it answers; it isn't part of the conversation. It is
    /// printed as one block, at the end of the model's paragraph if one is streaming.
    fn show_aside(&mut self, question: &str, answer: Result<(String, Usage)>) {
        self.aside = None;
        let (text, usage) = match answer {
            Ok(answer) => answer,
            Err(e) => return self.error(&format!("/btw: {e:#}")),
        };
        let cached = usage.cached * 100 / usage.prompt.max(1);
        let note = format!("Not kept in the conversation · {} in, {cached}% cached", short(usage.prompt));
        self.held_asides.push(aside_rows(question, &text, &note, self.width));
        self.release_asides(false);
    }

    /// Prints the answered /btw blocks, unless the model is in the middle of a paragraph and
    /// `force` is off: the block would cut it in two.
    fn release_asides(&mut self, force: bool) {
        if self.held_asides.is_empty() || (!force && self.last_kind == Kind::Text && !self.held_blank) {
            return;
        }
        // The block ends with its own blank row and the prose goes on as if it weren't there, so
        // the next chunk of text keeps its leading spaces.
        let before = self.last_kind;
        for rows in std::mem::take(&mut self.held_asides) {
            self.begin(Kind::Note);
            for row in rows {
                self.commit(row);
            }
            self.commit(String::new());
        }
        self.last_kind = before;
    }

    fn usage_report(&mut self, agent: &Agent) {
        let usage = &agent.stats.usage;
        let cached = usage.cached * 100 / usage.prompt.max(1);
        self.begin(Kind::Note);
        self.commit(format!(
            "{DIM}This session: {} request{}, {} tool call{}, {} input tokens ({cached}% cached), {} output tokens.{RESET}",
            agent.stats.requests,
            if agent.stats.requests == 1 { "" } else { "s" },
            agent.stats.tool_calls,
            if agent.stats.tool_calls == 1 { "" } else { "s" },
            short(usage.prompt),
            short(usage.completion)
        ));
    }

    /// Prints and copies what another agent needs to find this session: its id, the log's
    /// path, a summary and the prompts so far.
    fn share_session(&mut self, agent: &Agent) {
        let log = &agent.session.log_path;
        let id = log.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let later = crate::later::list(&self.cwd).len();
        let later = (later > 0).then(|| (crate::later::path(&self.cwd), later));
        let text = handoff(&id, log, &tilde(&self.cwd), agent.model_alias(), agent.thinking().label(), &agent.user_prompts(), &agent.stats, later);
        copy_to_clipboard(&text);
        self.begin(Kind::Note);
        for line in text.lines() {
            self.commit_wrapped(format!("{DIM}{line}{RESET}"));
        }
        self.notice("Copied to the clipboard. Paste it into another agent and it can read the whole session from the log.");
    }

    fn open_session_picker(&mut self, cwd: &Path) {
        let sessions = crate::session::list(cwd);
        if sessions.is_empty() {
            self.notice("No earlier sessions in this folder.");
            return;
        }
        let options = sessions
            .into_iter()
            .take(20)
            .map(|s| {
                let age = s.modified.elapsed().map(ago).unwrap_or_default();
                let prompt = s.first_prompt.split_whitespace().collect::<Vec<_>>().join(" ");
                (format!("{age:>8}  {}", crate::llm::truncate(&prompt, 70)), format!("{} prompts", s.prompts), PickerChoice::Session(s.path))
            })
            .collect();
        self.picker = Some(Picker::new("Resume a session".into(), options, 0));
    }

    /// Lists what was noted for later in this folder, numbered as `/later done N` counts them.
    fn show_later(&mut self, cwd: &Path) {
        let items = crate::later::list(cwd);
        self.begin(Kind::Note);
        if items.is_empty() {
            return self.notice("Nothing noted for later in this folder. The agent adds to the list with its `later` tool.");
        }
        self.commit(format!("{BOLD}Noted for later{RESET} {DIM}({}) · {}{RESET}", items.len(), tilde(&crate::later::path(cwd))));
        for (i, item) in items.iter().enumerate() {
            self.commit_wrapped(format!("  {DIM}{}.{RESET} {item}", i + 1));
        }
        self.notice("/later done N crosses one off.");
    }

    /// Offers how many of the newest sessions to keep, each choice saying how many it removes.
    fn open_prune_picker(&mut self, current: &Path) {
        let others = crate::session::list_all().iter().filter(|(path, _)| path != current).count();
        let options: Vec<_> = prune_choices(others)
            .into_iter()
            .map(|keep| (format!("keep the newest {keep}"), format!("removes {} of {others}", others - keep), PickerChoice::Prune(keep)))
            .collect();
        if options.is_empty() {
            return self.notice(&format!("Only {others} other session(s) on this machine; nothing worth pruning."));
        }
        self.picker = Some(Picker::new("Prune sessions · this one stays".into(), options, 0));
    }

    fn open_model_picker(&mut self, config: &Config) {
        let options: Vec<_> = config
            .models
            .iter()
            .map(|(alias, model)| {
                let current = if *alias == self.status.model { " · current" } else { "" };
                (alias.clone(), format!("{}{current}", model.provider), PickerChoice::Model(alias.clone()))
            })
            .collect();
        let selected = options.iter().position(|(_, _, c)| matches!(c, PickerChoice::Model(a) if *a == self.status.model)).unwrap_or(0);
        self.picker = Some(Picker::new("Switch model".into(), options, selected));
    }

    fn open_mcp_picker(&mut self, mcp: &Mcp, selected: Option<&str>) {
        if mcp.is_empty() {
            return self.notice("No MCP servers are configured: add them to ~/.killi/mcp.json or the project's .mcp.json.");
        }
        let servers = mcp.list();
        let at = servers.iter().position(|s| Some(s.name.as_str()) == selected).unwrap_or(0);
        let options = servers
            .into_iter()
            .map(|s| (s.name.clone(), format!("{} · {}", if s.on { "on" } else { "off" }, s.hint), PickerChoice::Mcp(s.name)))
            .collect();
        self.picker = Some(Picker::new("MCP servers · Enter turns one on or off".into(), options, at));
    }

    fn open_effort_picker(&mut self, config: &Config) {
        let options: Vec<_> = config
            .efforts(&self.status.model)
            .into_iter()
            .map(|effort| {
                let current = if effort == self.status.thinking { "current" } else { "" };
                (effort.clone(), current.to_string(), PickerChoice::Effort(effort))
            })
            .collect();
        let selected = options.iter().position(|(_, _, c)| matches!(c, PickerChoice::Effort(e) if *e == self.status.thinking)).unwrap_or(0);
        self.picker = Some(Picker::new(format!("Thinking effort for {}", self.status.model), options, selected));
    }

    fn refresh_branch(&mut self) {
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&self.cwd)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        };
        self.status.branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]).map(|branch| {
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
            if dirty { format!("{branch}*") } else { branch }
        });
    }

    // ---------- drawing ----------

    /// Commits a line of the model's text, wrapped between words, list items under their text.
    fn commit_wrapped(&mut self, line: String) {
        let indent = text::hanging_indent(&line);
        for row in text::wrap_words(&line, self.width, indent) {
            self.commit(row);
        }
    }

    fn commit_markdown(&mut self, rows: Vec<String>) {
        for row in rows {
            self.commit_wrapped(row);
        }
    }

    /// Queues a line for the scrollback, never two blank lines in a row.
    fn commit(&mut self, line: String) {
        let blank = line.is_empty();
        if blank && self.last_blank {
            return;
        }
        self.last_blank = blank;
        self.pending.push(line);
    }

    /// Clears the screen and its scrollback, dropping lines not yet shown.
    fn wipe_screen(&mut self) {
        self.pending.clear();
        self.last_blank = false;
        self.last_kind = Kind::None;
        let _ = execute!(self.out, Clear(ClearType::All), Clear(ClearType::Purge), crossterm::cursor::MoveTo(0, 0));
        self.cursor_row = 0;
        self.shown.clear();
        self.since_banner = None;
    }

    /// Starts a block of the conversation: a blank row first, unless it continues the block
    /// above (prose after prose, a tool call after a tool call, a note after a note).
    fn begin(&mut self, kind: Kind) {
        if self.last_kind != kind {
            self.commit(String::new());
        }
        self.last_kind = kind;
    }

    fn notice(&mut self, text: &str) {
        self.begin(Kind::Note);
        self.commit(format!("{DIM}{text}{RESET}"));
    }

    fn error(&mut self, text: &str) {
        self.begin(Kind::Note);
        self.commit(format!("{DANGER}✗ Error:{RESET} {text}"));
        if let Some(hint) = error_hint(text) {
            self.commit(format!("{DIM}{hint}{RESET}"));
        }
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        self.width = usize::from(columns).max(20);
        self.markdown.width = self.width;
        self.height = usize::from(rows).max(5);
        if self.cursor_row > 0 {
            let _ = queue!(self.out, MoveUp(self.cursor_row as u16));
        }
        let _ = queue!(self.out, MoveToColumn(0), Clear(ClearType::FromCursorDown));
        self.cursor_row = 0;
        self.shown.clear();
        self.since_banner = None;
    }

    /// The live area's rows and where the cursor goes in them.
    fn live(&self) -> (Vec<String>, usize, usize) {
        let width = self.width;
        let mut rows: Vec<String> = Vec::new();
        // A blank row, unless the row above is blank already: it keeps the parts of the live
        // area, and the live area and the conversation, from touching.
        let space = |rows: &mut Vec<String>| {
            if rows.last().map_or(!self.last_blank, |row| !row.is_empty()) {
                rows.push(String::new());
            }
        };
        if !self.partial.is_empty() {
            let mut markdown = self.markdown.clone();
            let mut styled = markdown.line(&self.partial);
            styled.extend(markdown.finish());
            let preview: Vec<String> = styled.iter().flat_map(|row| wrap(row, width)).collect();
            rows.extend(preview.into_iter().rev().take(6).rev());
        }
        if let Some(started) = self.busy {
            let frame = QUARTER[self.spinner % QUARTER.len()];
            // Work in flight continues the tool rows above it, but prose gets a blank row first.
            if !rows.is_empty() || matches!(self.last_kind, Kind::Text | Kind::Note) {
                space(&mut rows);
            }
            if let Some((since, text)) = &self.thinking {
                let first = first_sentence(text);
                rows.push(fit(&format!("{DIM}∴ thinking {} {first}{RESET}", clock(since.elapsed())), width));
            }
            let waiting_on_you = |running: &Running| self.asking.is_some() && running.summary.starts_with("ask_user_question");
            for running in self.running.iter().filter(|r| !waiting_on_you(r)).take(5) {
                let took = format!(" {DIM}· {}{RESET}", clock(running.started.elapsed()));
                let line = match &running.label {
                    Some(label) => format!("  {DIM}↳ {label}:{RESET} {LAGOON}{frame}{RESET} {DIM}{}{RESET}{took}", running.summary),
                    None => format!("{LAGOON}{frame}{RESET} {}{took}", tool_summary(&running.summary)),
                };
                rows.push(fit(&line, width));
            }
            if let Some((question, since)) = &self.aside {
                let question = clip(&question.split_whitespace().collect::<Vec<_>>().join(" "), width.saturating_sub(14));
                rows.push(fit(&format!("{LAGOON}{frame}{RESET} {BOLD}{FIN}btw{RESET} {question}{DIM} · {}{RESET}", clock(since.elapsed())), width));
            }
            if self.asking.is_none() {
                let spin = banner::working(self.spinner);
                rows.push(fit(&format!("{spin} {DIM}working {} · Esc to interrupt{RESET}", clock(started.elapsed())), width));
            }
            for queued in &self.queued {
                rows.push(fit(&format!("{FIN}❯{RESET} {DIM}{} (sent at the next step){RESET}", queued.replace('\n', " ")), width));
            }
        }
        if self.todos.iter().any(|t| t.status != "completed") {
            space(&mut rows);
            let done = self.todos.iter().filter(|t| t.status == "completed").count();
            rows.push(fit(&format!("  {DIM}tasks {done}/{}{RESET}", self.todos.len()), width));
            for todo in self.todos.iter().take(8) {
                let line = match todo.status.as_str() {
                    "completed" => format!("{LAGOON}✓{RESET} {DIM}{}{RESET}", todo.content),
                    "in_progress" => format!("{LAGOON}◐{RESET} {}", todo.content),
                    _ => format!("{FAINT}○{RESET} {}", todo.content),
                };
                rows.push(fit(&format!("  {line}"), width));
            }
            if self.todos.len() > 8 {
                rows.push(format!("  {DIM}… {} more{RESET}", self.todos.len() - 8));
            }
        }
        let mode = self.controls.plan.get().then_some("plan mode · Shift+Tab leaves it");
        space(&mut rows);
        rows.push(rule(mode, width));
        let known = |name: &str| self.entries.iter().any(|e| e.name == name || e.aliases.iter().any(|a| a == name));
        let (mut input, cursor_row, cursor_col) = self.editor.render(&format!("{LAGOON}❯{RESET} "), width, &known);
        if let Some(hint) = self.placeholder() {
            input[0] = fit(&format!("{LAGOON}❯{RESET} {FAINT}{hint}{RESET}"), width);
        }
        let first = cursor_row.saturating_sub(MAX_INPUT_ROWS - 1);
        let top = rows.len();
        rows.extend(input.into_iter().skip(first).take(MAX_INPUT_ROWS));
        let mut cursor = (top + cursor_row - first, cursor_col);
        rows.push(rule(mode.map(|_| ""), width));
        let modal = self.picker.is_some() || self.asking.is_some();
        let room = (self.height - 1).saturating_sub(rows.len() - top + 3 + usize::from(modal)).min(MENU_ROWS);
        if modal {
            rows.push(String::new());
        }
        if let Some(menu) = self.suggestions() {
            let visible = menu.items.len().min(room);
            if visible > 0 {
                rows.extend(self.menu_rows(&menu, visible));
            }
        }
        if let Some(picker) = &self.picker {
            let count = picker.shown.len();
            let visible = count.min(room.saturating_sub(1).max(1));
            rows.push(fit(&title_rule(&picker.title, width), width));
            let options: Vec<(&str, &str)> = picker.shown.iter().map(|&i| (picker.options[i].0.as_str(), picker.options[i].1.as_str())).collect();
            rows.extend(choice_rows(&options, picker.selected, visible, width, width * 3 / 5));
            let position = if count > visible { format!("{}/{count} · ", picker.selected + 1) } else { String::new() };
            let filter = match (picker.query.is_empty(), count) {
                (true, _) => "type to filter".to_string(),
                (false, 0) => format!("{RESET}{}{DIM} matches nothing", picker.query),
                (false, _) => format!("filter {RESET}{}{DIM}", picker.query),
            };
            rows.push(fit(&format!("  {DIM}{filter} · {position}↑↓ choose · Tab or Enter selects · Esc closes{RESET}"), width));
        }
        if let Some(asking) = &self.asking {
            let names: Vec<String> = asking.question.options.iter().enumerate().map(|(i, o)| format!("{}. {o}", i + 1)).collect();
            let options: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "")).collect();
            let visible = options.len().min(room.max(1));
            rows.extend(choice_rows(&options, asking.selected.min(options.len().saturating_sub(1)), visible, width, width));
            let hint = if asking.question.plan { "or type what to change" } else { "or type your own answer" };
            let choose = match options.len() {
                0 => String::new(),
                1 => "Tab or Enter to choose, ".into(),
                n => format!("1-{} or ↑↓ and Tab or Enter to choose, ", n.min(9)),
            };
            rows.push(fit(&format!("  {DIM}{choose}{hint} · Esc to dismiss{RESET}"), width));
        }
        rows.push(self.status_line(width));
        let excess = rows.len().saturating_sub(self.height - 1);
        if excess > 0 {
            let cut = excess.min(top);
            rows.drain(..cut);
            cursor.0 -= cut;
        }
        (rows, cursor.0, cursor.1)
    }

    fn placeholder(&self) -> Option<&'static str> {
        placeholder(self.editor.is_empty(), self.busy.is_some(), self.picker.is_some() || self.asking.is_some())
    }

    /// The slash menu, `visible` items at a time around the selection, and a line of keys.
    fn menu_rows(&self, menu: &Suggestions, visible: usize) -> Vec<String> {
        let count = menu.items.len();
        let selected = self.menu_selected.min(count - 1);
        let lead = if menu.command { "/" } else { "" };
        let names: Vec<String> = menu.items.iter().map(|i| format!("{lead}{}", i.value)).collect();
        let options: Vec<(&str, &str)> = names.iter().zip(&menu.items).map(|(name, item)| (name.as_str(), item.description.as_str())).collect();
        let mut rows = choice_rows(&options, selected, visible, self.width, 32);
        let position = if count > visible { format!("{}/{count} · ", selected + 1) } else { String::new() };
        let enter = match (menu.command, runs_at_once(menu, selected)) {
            (true, true) => "runs",
            (true, false) => "completes",
            (false, _) => "fills in",
        };
        rows.push(fit(&format!("  {DIM}{position}↑↓ choose · Tab or Enter {enter} · Esc closes{RESET}"), self.width));
        rows
    }

    /// The status line for `width` columns. Parts drop out from the least to the most useful as
    /// the terminal narrows: the cache and token detail, the context bar, the branch, and last
    /// the context figure, so the model and mode stay on screen at any width.
    fn status_line(&self, width: usize) -> String {
        let mut head = vec![format!("{RESET}{}{DIM}", self.status.model)];
        if let Some((word, value)) = thinking_parts(&self.status.thinking) {
            head.push(format!("{word} {}", tint(EFFORT, value)));
        }
        if self.controls.plan.get() {
            head.push(format!("{LAGOON}plan mode{RESET}{DIM}"));
        }
        if self.controls.swarm.get() {
            head.push(format!("{LAGOON}swarm mode{RESET}{DIM}"));
        }
        status_layout(&head.join(" · "), &self.status, width)
    }

    /// Paints the live area, in one write inside a synchronized update so no half-drawn frame
    /// is ever shown. When only rows changed, as on a spinner tick, just those are rewritten;
    /// a committed line scrolls the area, so then it is painted whole.
    fn draw(&mut self) -> Result<()> {
        let (rows, cursor_row, cursor_col) = self.live();
        let rows: Vec<String> = rows.iter().map(|row| text::plain(row).into_owned()).collect();
        let changed = self.pending.is_empty().then(|| changed_rows(&self.shown, &rows)).flatten();
        if changed.as_ref().is_some_and(|changed| changed.is_empty()) && cursor_row == self.cursor_row && cursor_col == self.shown_col {
            return Ok(());
        }
        let mut frame: Vec<u8> = Vec::new();
        queue!(frame, BeginSynchronizedUpdate, Hide)?;
        match changed {
            Some(changed) => {
                let mut at = self.cursor_row;
                for i in changed {
                    move_rows(&mut frame, at, i)?;
                    queue!(frame, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                    write!(frame, "{}{RESET}", rows[i])?;
                    at = i;
                }
                move_rows(&mut frame, at, cursor_row)?;
            }
            None => {
                if self.cursor_row > 0 {
                    queue!(frame, MoveUp(self.cursor_row as u16))?;
                }
                queue!(frame, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
                for line in self.pending.drain(..) {
                    write!(frame, "{}{RESET}\r\n", text::plain(&line))?;
                    if let Some(since) = &mut self.since_banner {
                        since.rows += screen_rows(&line, self.width);
                        since.lines.push(line);
                    }
                }
                write!(frame, "{}", rows.join("\r\n"))?;
                move_rows(&mut frame, rows.len().saturating_sub(1), cursor_row)?;
            }
        }
        if self.since_banner.as_ref().is_some_and(|since| since.rows + rows.len() > self.height) {
            self.since_banner = None;
        }
        queue!(frame, MoveToColumn(cursor_col as u16), Show, EndSynchronizedUpdate)?;
        self.out.write_all(&frame)?;
        self.out.flush()?;
        self.shown = rows;
        self.shown_col = cursor_col;
        self.cursor_row = cursor_row;
        Ok(())
    }

    /// Leaves the conversation on screen and puts the shell prompt below it.
    fn finish_screen(&mut self) -> Result<()> {
        if self.cursor_row > 0 {
            queue!(self.out, MoveUp(self.cursor_row as u16))?;
        }
        queue!(self.out, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
        for line in self.pending.drain(..) {
            write!(self.out, "{}{RESET}\r\n", text::plain(&line))?;
        }
        self.out.flush()?;
        Ok(())
    }
}

enum Idle {
    Continue,
    Exit,
    Input(Input),
}

/// Shift+Enter, Alt+Enter and Ctrl+Enter insert a line break instead of sending.
fn newline_key(key: &KeyEvent) -> bool {
    key.code == KeyCode::Enter && key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL)
}

/// Rows for a list of (name, note) choices, `visible` at a time around `selected`, the names
/// in a column at most `column` wide and the notes dimmed beside them.
fn choice_rows(options: &[(&str, &str)], selected: usize, visible: usize, width: usize, column: usize) -> Vec<String> {
    let start = selected.saturating_sub(visible / 2).min(options.len() - visible);
    let shown = &options[start..start + visible];
    let column = shown.iter().map(|(name, _)| text::width(name)).max().unwrap_or(0).clamp(12, column.max(12));
    shown
        .iter()
        .enumerate()
        .map(|(i, (name, note))| {
            let name = clip(name, column);
            let pad = " ".repeat(column.saturating_sub(text::width(&name)));
            let note = note.split_whitespace().collect::<Vec<_>>().join(" ");
            if start + i == selected {
                let line = fit(&format!("{TINT}{LAGOON}❯ {RESET}{TINT}{BOLD}{name}{RESET}{TINT}{pad}  {note}"), width);
                let fill = " ".repeat(width.saturating_sub(text::width(&line)));
                format!("{line}{TINT}{fill}{RESET}")
            } else {
                fit(&format!("  {name}{pad}  {DIM}{note}{RESET}"), width)
            }
        })
        .collect()
}

/// A /btw question and its answer as one block: the question, the answer hung off the output
/// gutter like a tool's output, and `note` last, so it reads as set aside from the conversation.
fn aside_rows(question: &str, answer: &str, note: &str, width: usize) -> Vec<String> {
    let question = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut rows = wrap_words(&format!("{BOLD}{FIN}btw{RESET} {BOLD}{question}{RESET}"), width, 4);
    let gutter = format!("  {FAINT}│{RESET}");
    let inner = width.saturating_sub(4);
    let mut markdown = Markdown::new(inner);
    let mut styled: Vec<String> = answer.lines().flat_map(|line| markdown.line(line)).collect();
    styled.extend(markdown.finish());
    while styled.last().is_some_and(|row| text::width(row) == 0) {
        styled.pop();
    }
    for row in styled {
        if text::width(&row) == 0 {
            rows.push(gutter.clone());
            continue;
        }
        for wrapped in wrap_words(&row, inner, text::hanging_indent(&row)) {
            rows.push(format!("{gutter} {wrapped}"));
        }
    }
    rows.push(gutter.clone());
    rows.push(format!("{gutter} {DIM}{note}{RESET}"));
    rows
}

/// What the empty prompt says it takes, in fg-faint: nothing once something is typed or while
/// a picker or a question `waits`, since those say it themselves.
fn placeholder(empty: bool, busy: bool, waits: bool) -> Option<&'static str> {
    if !empty || waits {
        return None;
    }
    Some(if busy { "a message for the next step · /btw asks on the side" } else { "ask, or / for commands and skills" })
}

/// A full-width line around the prompt, in lagoon with `label` on it in plan mode.
fn rule(label: Option<&str>, width: usize) -> String {
    match label {
        None => format!("{FAINT}{}{RESET}", "─".repeat(width)),
        Some("") => format!("{LAGOON}{}{RESET}", "─".repeat(width)),
        Some(label) => {
            let rest = width.saturating_sub(text::width(label) + 4);
            format!("{LAGOON}── {BOLD}{label}{RESET}{LAGOON} {}{RESET}", "─".repeat(rest))
        }
    }
}

/// A full-width line with `title` on it in lagoon, above a list of choices.
fn title_rule(title: &str, width: usize) -> String {
    let rest = width.saturating_sub(text::width(title) + 4);
    format!("{LAGOON}── {BOLD}{title}{RESET}{LAGOON} {}{RESET}", "─".repeat(rest))
}

/// The status line's parts fitted to `width`; see `App::status_line`. Each kind of figure has
/// its own color: the effort, the tokens and the branch; the meter is lagoon, and warn from 80%.
fn status_layout(head: &str, status: &Status, width: usize) -> String {
    let warn = status.window.is_some_and(|window| near_full(status.context, window));
    let figure = |text: String| if warn { tint(WARN, &text) } else { text };
    let context = |bar: bool| match (status.window, bar) {
        (Some(window), true) => format!("ctx {} {}", gauge(status.context, window), figure(format!("{}/{}", short(status.context), short(window)))),
        (Some(window), false) => format!("ctx {}", figure(format!("{}/{}", short(status.context), short(window)))),
        (None, _) => format!("ctx {}", short(status.context)),
    };
    let percent = match status.window {
        Some(window) => format!("ctx {}", figure(format!("{}%", status.context * 100 / window.max(1)))),
        None => format!("ctx {}", short(status.context)),
    };
    let tokens = |cache: bool| {
        if status.prompt == 0 {
            return None;
        }
        let count = |n: String| tint(TOKENS, &n);
        let cached = status.cached * 100 / status.prompt.max(1);
        Some(match cache {
            true => format!("{} in · {} cached · {} out", count(short(status.prompt)), count(format!("{cached}%")), count(short(status.completion))),
            false => format!("{} in · {} out", count(short(status.prompt)), count(short(status.completion))),
        })
    };
    let branch = |max: usize| {
        status.branch.as_ref().map(|branch| {
            let branch = clip(branch, max);
            match branch.strip_suffix('*') {
                Some(name) => format!("{}{}", tint(BRANCH, &format!("⎇ {name}")), tint(WARN, "*")),
                None => tint(BRANCH, &format!("⎇ {branch}")),
            }
        })
    };
    let layouts = [
        (context(true), tokens(true), branch(usize::MAX)),
        (context(true), tokens(false), branch(usize::MAX)),
        (context(true), None, branch(usize::MAX)),
        (context(false), None, branch(usize::MAX)),
        (percent.clone(), None, branch(16)),
        (percent.clone(), None, None),
    ];
    let divider = format!(" {} ", tint(FAINT, "│"));
    for (context, tokens, branch) in layouts {
        let groups: Vec<String> = [Some(head.to_string()), Some(context), tokens, branch].into_iter().flatten().collect();
        let line = groups.join(&divider);
        if text::width(&line) <= width {
            return format!("{DIM}{line}{RESET}");
        }
    }
    format!("{DIM}{}{RESET}", fit(head, width))
}

/// `text` in `color` inside a dim line: it resets, colors, and goes back to dim after.
fn tint(color: impl std::fmt::Display, text: &str) -> String {
    format!("{RESET}{color}{text}{RESET}{DIM}")
}

/// Whether the context has reached the share of the window where the meter turns warn.
fn near_full(used: u64, window: u64) -> bool {
    used * 100 / window.max(1) >= 80
}

/// How full the context window is, as a ten-cell lagoon meter that turns warn from 80%.
fn gauge(used: u64, window: u64) -> String {
    let filled = (used.saturating_mul(10).div_ceil(window.max(1)) as usize).min(10);
    let color = if near_full(used, window) { WARN } else { LAGOON };
    format!("{RESET}{color}{}{RESET}{FAINT}{}{RESET}{DIM}", "━".repeat(filled), "╌".repeat(10 - filled))
}

/// `text` cut to `max` columns with an ellipsis.
fn clip(text: &str, max: usize) -> String {
    if text::width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    for c in text.chars() {
        if text::width(&out) + 2 > max {
            break;
        }
        out.push(c);
    }
    out + "…"
}

/// The effort as a label and its value, as the banner and status line show them; nothing for
/// the provider's default.
fn thinking_parts(thinking: &str) -> Option<(&'static str, &str)> {
    match thinking {
        "on" => None,
        "off" => Some(("thinking", "off")),
        effort => Some(("effort", effort)),
    }
}

fn fit(line: &str, width: usize) -> String {
    wrap(line, width).into_iter().next().unwrap_or_default()
}

/// The indexes of the rows that differ from those on screen, or None when the area changed size
/// and has to be painted whole.
fn changed_rows(shown: &[String], rows: &[String]) -> Option<Vec<usize>> {
    (shown.len() == rows.len()).then(|| (0..rows.len()).filter(|&i| shown[i] != rows[i]).collect())
}

/// Moves the cursor from one row of the live area to another.
fn move_rows(out: &mut impl Write, from: usize, to: usize) -> std::io::Result<()> {
    match to.cmp(&from) {
        std::cmp::Ordering::Less => queue!(out, MoveUp((from - to) as u16)),
        std::cmp::Ordering::Greater => queue!(out, MoveDown((to - from) as u16)),
        std::cmp::Ordering::Equal => Ok(()),
    }
}

/// What to try for the failures people hit most, from the text of the error.
fn error_hint(error: &str) -> Option<&'static str> {
    let error = error.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| error.contains(w));
    if has(&["401", "403", "unauthorized", "invalid api key", "invalid_api_key", "forbidden"]) {
        Some("Check api_key or api_key_env for this model's provider in ~/.killi/config.toml.")
    } else if has(&["context length", "context_length", "too long", "maximum context", "prompt is too long"]) {
        Some("The conversation no longer fits the model: /compact summarizes it, /new starts over.")
    } else if has(&["usage limit", "quota", "reset at", "exceeded your current"]) {
        Some("This provider's usage limit is spent: pick another model with /model, or wait until the reset time above.")
    } else if has(&["429", "rate limit"]) {
        Some("The provider is limiting requests: wait a moment, or pick another model with /model.")
    } else if has(&["404", "model not found", "unknown model", "does not exist"]) {
        Some("The provider doesn't know this model or URL: check model and base_url in ~/.killi/config.toml.")
    } else if has(&["error sending request", "connection", "dns", "timed out", "timeout"]) {
        Some("Can't reach the provider: check your network, proxy and base_url.")
    } else {
        None
    }
}

/// An error reason with a JSON body cut down to its status and message: `HTTP 529: busy`. What
/// follows the body, such as `(after 3 attempts)`, stays.
fn gist(reason: &str) -> String {
    let reason = reason.replace('\n', " ");
    let Some(start) = reason.find('{') else { return reason };
    let mut bodies = serde_json::Deserializer::from_str(&reason[start..]).into_iter::<serde_json::Value>();
    let Some(Ok(body)) = bodies.next() else { return reason };
    let Some(message) = body.pointer("/error/message").or(body.get("message")).and_then(|m| m.as_str()) else { return reason };
    let after = reason[start + bodies.byte_offset()..].trim();
    let after = if after.is_empty() { String::new() } else { format!(" {after}") };
    let status: Vec<&str> = reason[..start].split_whitespace().take(2).collect();
    match status.as_slice() {
        ["HTTP", code] => format!("HTTP {}: {message}{after}", code.trim_end_matches(':')),
        _ => format!("{message}{after}"),
    }
}

/// A row of the edit tool's diff behind the output gutter: the line number, then the sign and
/// text in lagoon for an added line, danger for a removed one, muted for context.
fn diff_line(line: &str) -> String {
    let Some((head, text)) = line.split_once('\t').filter(|(head, _)| head.len() > 1 && head.is_char_boundary(1)) else {
        return format!("  {FAINT}│{RESET} {DIM}{line}{RESET}");
    };
    let (sign, number) = head.split_at(1);
    let color = match sign {
        "+" => LAGOON.to_string(),
        "-" => DANGER.to_string(),
        _ => DIM.to_string(),
    };
    format!("  {FAINT}│{RESET} {DIM}{number:>4}{RESET} {color}{sign} {text}{RESET}")
}

/// A tool call's summary with the tool's name in bold: `read src/lib.rs`.
fn tool_summary(summary: &str) -> String {
    match summary.split_once(' ') {
        Some((name, args)) => format!("{BOLD}{name}{RESET} {args}"),
        None => format!("{BOLD}{summary}{RESET}"),
    }
}

fn first_sentence(text: &str) -> String {
    let text = text.trim().replace('\n', " ");
    let end = text.find(['.', '。', '!', '?']).map_or(text.len(), |i| i + text[i..].chars().next().map_or(1, char::len_utf8));
    crate::llm::truncate(&text[..end], 100).to_string()
}

/// A duration in whole seconds that moves up a unit as it grows: `45s`, `1m05s`, `1h02m`.
fn clock(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f32().round() as u64;
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3_600, secs % 3_600 / 60),
    }
}

/// Like `clock`, with a tenth of a second below a minute: `2.4s`, `1m05s`.
fn precise_clock(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0..60 => format!("{:.1}s", elapsed.as_secs_f32()),
        _ => clock(elapsed),
    }
}

/// The question in `/btw <question>`, if that is what `text` is.
fn aside_question(text: &str) -> Option<String> {
    let rest = text.trim().strip_prefix("/btw")?;
    rest.starts_with(char::is_whitespace).then(|| rest.trim().to_string()).filter(|q| !q.is_empty())
}

fn short(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3).replace(".0k", "k"),
        _ => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
    }
}

fn ago(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    match s {
        0..60 => "just now".into(),
        60..3_600 => format!("{}m ago", s / 60),
        3_600..86_400 => format!("{}h ago", s / 3_600),
        _ => format!("{}d ago", s / 86_400),
    }
}

/// The counts `/prune` offers to keep, from fewest to most, each removing something.
fn prune_choices(others: usize) -> Vec<usize> {
    [5, 10, 20, 50, 100, 200].into_iter().filter(|&keep| keep < others).collect()
}

/// Bytes for the eye: `312 KB`, `4.2 MB`.
fn megabytes(bytes: u64) -> String {
    match bytes {
        0..1_000_000 => format!("{} KB", bytes / 1_000),
        _ => format!("{:.1} MB", bytes as f64 / 1e6),
    }
}

/// Whether a reply says it left work undone, by the words replies use for that.
fn leaves_something_for_later(text: &str) -> bool {
    let text = text.to_lowercase();
    ["for later", "follow-up", "follow up", "deferred", "left out", "not done", "out of scope", "didn't do", "后续", "待办", "留到", "之后再", "没有做", "未做"]
        .iter()
        .any(|phrase| text.contains(phrase))
}

/// A banner label, dimmed and padded so the values beside it line up.
fn label(name: &str) -> String {
    format!("{DIM}{name:<6}{RESET} ")
}

/// The banner's rows under the model and folder, in the same column: the instruction files and
/// what they cost, skills, MCP servers, a custom prompt and what is noted for later.
fn overview_rows(overview: &Overview) -> Vec<String> {
    let mut rows = Vec::new();
    if !overview.instructions.is_empty() {
        let names: Vec<String> = overview.instructions.iter().map(|path| tilde(path)).collect();
        rows.push(format!("{}{} {DIM}·{RESET} {TOKENS}~{}{RESET} {DIM}tokens{RESET}", label("rules"), names.join(", "), short(overview.instruction_tokens as u64)));
    }
    if overview.skills > 0 {
        rows.push(format!("{}{}", label("skills"), overview.skills));
    }
    if !overview.mcp.is_empty() {
        rows.push(format!("{}{}", label("mcp"), overview.mcp.join(", ")));
    }
    if overview.custom_prompt {
        rows.push(format!("{}custom {DIM}· [prompt] in config.toml{RESET}", label("prompt")));
    }
    if overview.later > 0 {
        rows.push(format!("{}{} noted {DIM}·{RESET} {LAGOON}/later{RESET}", label("later"), overview.later));
    }
    rows
}

/// The keys worth knowing, each in lagoon with what it does dimmed beside it.
fn keys_hint() -> String {
    let keys = [("/", "commands and skills"), ("Shift+Enter", "new line"), ("Shift+Tab", "plan mode"), ("Esc", "interrupts")];
    keys.iter().map(|(key, what)| format!("{LAGOON}{key}{RESET} {DIM}{what}{RESET}")).collect::<Vec<_>>().join(&format!(" {DIM}·{RESET} "))
}

/// The rows a printed line takes on a screen `width` columns wide, wrapping included.
fn screen_rows(line: &str, width: usize) -> usize {
    line.split('\n').map(|part| text::width(part).max(1).div_ceil(width.max(1))).sum()
}

/// `path` with the home folder as `~`.
fn tilde(path: &Path) -> String {
    match path.strip_prefix(crate::config::home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// The text `/session` copies: enough for another agent to find the log and know how to
/// read it, with the session's numbers and prompts so the reader knows what it is looking at.
#[allow(clippy::too_many_arguments)]
fn handoff(
    id: &str,
    log: &Path,
    cwd: &str,
    model: &str,
    effort: &str,
    prompts: &[String],
    stats: &crate::agent::Stats,
    later: Option<(PathBuf, usize)>,
) -> String {
    let usage = &stats.usage;
    let cached = usage.cached * 100 / usage.prompt.max(1);
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    let mut out = vec![
        format!("killi session {id} · {cwd}"),
        format!("log: {}", log.display()),
        format!(
            "{model} · effort {effort} · {} · {} · {} · {} in, {cached}% cached · {} out",
            plural(prompts.len(), "prompt"),
            plural(stats.requests as usize, "request"),
            plural(stats.tool_calls as usize, "tool call"),
            short(usage.prompt),
            short(usage.completion)
        ),
    ];
    if let Some((path, open)) = later {
        out.push(format!("later: {} ({open} open: what the agent noted to do later)", path.display()));
    }
    for (i, prompt) in prompts.iter().enumerate() {
        out.push(format!("{}. {}", i + 1, crate::llm::truncate(&prompt.split_whitespace().collect::<Vec<_>>().join(" "), 160)));
    }
    out.push(
        "The log is JSON lines, one record per `type`: `session` (cwd, model, effort, tools), every `message` exactly as sent \
         (user, assistant with tool_calls, tool results; a tool result's `tool` has is_error, brief and ms), `usage` after each \
         request (prompt, cached, completion and reasoning tokens, finish_reason, ms), `model` when the model or effort \
         changed, and `compaction`. Every record has a `ts` in ms. `scripts/sessions.py` in the killi repo summarizes logs and flags what to fix."
            .to_string(),
    );
    out.join("\n")
}

/// Puts `text` on the clipboard, through the terminal (OSC 52, which reaches the local
/// clipboard over SSH too) and through pbcopy.
fn copy_to_clipboard(text: &str) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut out = stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = out.flush();
    if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

/// Standard base64 with padding, as OSC 52 wants it.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A desktop notification: the terminal's own when it supports OSC 9, otherwise macOS's,
/// posted in the name of the app the terminal runs in, so clicking it brings that app to the
/// front rather than Script Editor. An app that can't be told falls back to a plain one.
fn notify(body: &str) {
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let kil = program == "kil" && kil_takes_osc9(&std::env::var("TERM_PROGRAM_VERSION").unwrap_or_default());
    if kil || ["iTerm.app", "WezTerm", "ghostty"].contains(&program.as_str()) || std::env::var_os("KITTY_WINDOW_ID").is_some() {
        let mut out = stdout();
        let _ = write!(out, "\x1b]9;killi: {body}\x07");
        let _ = out.flush();
        return;
    }
    let plain = format!("display notification \"{}\" with title \"killi\"", body.replace('"', "'"));
    let mut command = std::process::Command::new("/usr/bin/osascript");
    match host_app(&program) {
        Some(id) => command.args(["-e", "try", "-e", &format!("tell application id \"{id}\" to {plain}"), "-e", "on error", "-e", &plain, "-e", "end try"]),
        None => command.args(["-e", &plain]),
    };
    let _ = command.spawn();
}

/// Whether this Kil shows OSC 9 notifications itself and goes to the pane when one is
/// clicked: from 0.0.11. Older ones get the notification posted in Kil's name instead.
fn kil_takes_osc9(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let v = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    v >= (0, 0, 11)
}

/// The bundle id of the app the terminal runs in: what macOS puts in the environment of
/// anything an app starts, or the id that goes with `TERM_PROGRAM` for terminals that strip it.
fn host_app(program: &str) -> Option<String> {
    let known = match program {
        "Apple_Terminal" => Some("com.apple.Terminal"),
        "kil" => Some("dev.kil.terminal"),
        "vscode" => Some("com.microsoft.VSCode"),
        _ => None,
    };
    std::env::var("__CFBundleIdentifier")
        .ok()
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c)))
        .or_else(|| known.map(str::to_string))
}

struct Command {
    name: &'static str,
    /// Other names the menu finds it by. `/q`, `/h` and `/?` work too, unlisted.
    aliases: &'static [&'static str],
    args: &'static str,
    about: &'static str,
}

/// Whether `name` is a built-in command or one of its aliases, not a skill.
fn is_command(name: &str) -> bool {
    COMMANDS.iter().any(|c| c.name == name || c.aliases.contains(&name))
}

/// Whether choosing the menu's item at `index` sends the prompt: a built-in command at the start
/// does, a skill stays in the prompt so a request can follow it.
fn runs_at_once(menu: &Suggestions, index: usize) -> bool {
    menu.command && menu.start == 0 && is_command(&menu.items[index].value)
}

/// The built-in commands, in the order the menu lists them.
const COMMANDS: &[Command] = &[
    Command { name: "model", aliases: &[], args: "[alias]", about: "Switch model" },
    Command { name: "mcp", aliases: &[], args: "[server]", about: "Turn MCP servers on or off" },
    Command { name: "effort", aliases: &["thinking"], args: "[level]", about: "Switch thinking effort" },
    Command { name: "new", aliases: &["clear"], args: "", about: "Start a fresh session in this folder" },
    Command { name: "resume", aliases: &[], args: "", about: "Continue an earlier session in this folder" },
    Command { name: "btw", aliases: &[], args: "<question>", about: "Ask a side question, also while the agent works; not kept" },
    Command { name: "compact", aliases: &[], args: "[focus]", about: "Summarize the conversation to free context" },
    Command { name: "undo", aliases: &[], args: "", about: "Put files back as they were before the last turn" },
    Command { name: "plan", aliases: &[], args: "", about: "Toggle plan mode (also Shift+Tab)" },
    Command { name: "swarm", aliases: &[], args: "[task|on|off]", about: "Swarm mode: split work across many subagents" },
    Command { name: "usage", aliases: &["cost"], args: "", about: "Tokens and requests this session" },
    Command { name: "session", aliases: &["share"], args: "", about: "Copy this session's id, log path and summary, to paste into another agent" },
    Command { name: "later", aliases: &[], args: "[done N]", about: "What the agent noted for later in this folder; done N crosses one off" },
    Command { name: "prune", aliases: &[], args: "[keep]", about: "Delete every session on this machine but the newest few" },
    Command { name: "reload", aliases: &[], args: "", about: "Re-read [prompt], AGENTS.md files and skills" },
    Command { name: "help", aliases: &[], args: "", about: "Commands and keys" },
    Command { name: "exit", aliases: &["quit"], args: "", about: "Quit killi (also Ctrl+D)" },
];

const KEYS: &[(&str, &str)] = &[
    ("Enter", "send; while the agent works, sends your message at its next step"),
    ("Shift+Enter", "new line; also Ctrl+J, or \\ then Enter"),
    ("Esc", "interrupt the agent"),
    ("Shift+Tab", "plan mode"),
    ("Ctrl+O", "show the last tool output or thought in full"),
    ("Ctrl+V", "paste an image (Cmd+V too in Kil)"),
    ("↑ ↓", "history"),
    ("Ctrl+C", "clear the prompt; twice to quit"),
];

/// Raw mode and bracketed paste while the UI runs, restored when it ends.
struct TerminalGuard {
    enhanced: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnableBracketedPaste)?;
        let enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false);
        if enhanced {
            execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
        }
        Ok(Self { enhanced })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        if self.enhanced {
            let _ = execute!(out, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(out, DisableBracketedPaste, Show);
        let _ = terminal::disable_raw_mode();
        let _ = writeln!(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handoff_names_the_log_and_sums_the_session_up() {
        let stats = crate::agent::Stats {
            requests: 6,
            tool_calls: 7,
            usage: Usage { prompt: 22_400, cached: 18_600, completion: 1_200, reasoning: 0 },
            ..crate::agent::Stats::default()
        };
        let prompts = vec!["The slugify test fails.\nFix it.".to_string()];
        let log = Path::new("/h/.killi/sessions/x/1792673250-71085.jsonl");
        let text = handoff("1792673250-71085", log, "~/code/slug", "mimo/mimo-v2.6-pro", "high", &prompts, &stats, None);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "killi session 1792673250-71085 · ~/code/slug");
        assert_eq!(lines[1], "log: /h/.killi/sessions/x/1792673250-71085.jsonl");
        assert_eq!(lines[2], "mimo/mimo-v2.6-pro · effort high · 1 prompt · 6 requests · 7 tool calls · 22.4k in, 83% cached · 1.2k out");
        assert_eq!(lines[3], "1. The slugify test fails. Fix it.");
        assert!(lines[4].contains("`usage`") && lines[4].contains("sessions.py"));
        let later = Some((PathBuf::from("/h/.killi/sessions/x/later.md"), 2));
        let text = handoff("1", log, "~/code/slug", "m", "high", &prompts, &stats, later);
        assert_eq!(text.lines().nth(3).unwrap(), "later: /h/.killi/sessions/x/later.md (2 open: what the agent noted to do later)");
    }

    #[test]
    fn the_banner_lists_the_setup_in_its_column() {
        let overview = Overview {
            custom_prompt: true,
            instructions: vec![crate::config::home().join(".agents/AGENTS.md"), PathBuf::from("AGENTS.md")],
            instruction_tokens: 4551,
            skills: 204,
            later: 2,
            ..Overview::default()
        };
        let rows: Vec<String> = overview_rows(&overview).iter().map(|row| unstyled(row)).collect();
        assert_eq!(rows, [
            "rules  ~/.agents/AGENTS.md, AGENTS.md · ~4.6k tokens",
            "skills 204",
            "prompt custom · [prompt] in config.toml",
            "later  2 noted · /later",
        ]);
        assert!(overview_rows(&Overview::default()).is_empty());
        assert_eq!(unstyled(&keys_hint()), "/ commands and skills · Shift+Enter new line · Shift+Tab plan mode · Esc interrupts");
    }

    #[test]
    fn a_long_line_takes_the_rows_it_wraps_to() {
        assert_eq!(screen_rows("", 80), 1);
        assert_eq!(screen_rows(&format!("{DIM}{}{RESET}", "x".repeat(80)), 80), 1);
        assert_eq!(screen_rows(&"x".repeat(81), 80), 2);
        assert_eq!(screen_rows("a\nb", 80), 2);
    }

    #[test]
    fn prune_offers_only_counts_that_remove_something() {
        assert_eq!(prune_choices(3), Vec::<usize>::new());
        assert_eq!(prune_choices(12), [5, 10]);
        assert_eq!(prune_choices(500), [5, 10, 20, 50, 100, 200]);
        assert_eq!(megabytes(312_000), "312 KB");
        assert_eq!(megabytes(4_200_000), "4.2 MB");
    }

    #[test]
    fn a_reply_that_leaves_work_for_later_is_recognised() {
        assert!(leaves_something_for_later("Fixed the test. The flaky CI job is a follow-up."));
        assert!(leaves_something_for_later("修好了；缓存的问题留到之后再做。"));
        assert!(!leaves_something_for_later("Fixed the test; cargo test passes."));
    }

    #[test]
    fn base64_pads_like_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"killi session"), "a2lsbGkgc2Vzc2lvbg==");
    }

    #[test]
    fn kil_gets_osc9_notifications_from_the_version_that_shows_them() {
        assert!(!kil_takes_osc9("0.0.10"));
        assert!(!kil_takes_osc9(""));
        assert!(kil_takes_osc9("0.0.11"));
        assert!(kil_takes_osc9("0.1.0"));
        assert!(kil_takes_osc9("1.0"));
    }

    #[test]
    fn the_terminals_background_answer_is_read_at_any_depth() {
        assert_eq!(parse_background("\x1b]11;rgb:0b0b/0f0f/1414\x1b\\"), Some((11, 15, 20)));
        assert_eq!(parse_background("\x1b]11;rgb:f5/f7/f4\x07"), Some((245, 247, 244)));
        assert_eq!(parse_background("\x1b]11;rgb:f/f/f\x07"), Some((255, 255, 255)));
        assert_eq!(parse_background("\x1b]11;rgb:ffff/ffff\x07"), None);
        assert_eq!(parse_background(""), None);
    }

    #[test]
    fn resume_command_quotes_only_when_needed() {
        assert_eq!(resume_command(Path::new("/home/a@b.c/.killi/sessions/-src-app/1-2.jsonl")), "killi -r /home/a@b.c/.killi/sessions/-src-app/1-2.jsonl");
        assert_eq!(resume_command(Path::new("/s/-My Code-it's/1-2.jsonl")), r"killi -r '/s/-My Code-it'\''s/1-2.jsonl'");
    }

    #[test]
    fn choosing_a_skill_leaves_it_in_the_prompt() {
        let item = |value: &str| Item { value: value.into(), description: String::new() };
        let menu = Suggestions { items: vec![item("usage"), item("cost"), item("pr-resolve")], start: 0, command: true };
        assert!(runs_at_once(&menu, 0));
        assert!(runs_at_once(&menu, 1));
        assert!(!runs_at_once(&menu, 2));
        assert!(!runs_at_once(&Suggestions { start: 4, ..menu }, 0));
    }

    #[test]
    fn retry_reasons_keep_the_status_and_message() {
        let reason = r#"HTTP 529 <unknown status code>: {"type":"error","error":{"type":"overloaded_error","message":"busy (2064)"}}"#;
        assert_eq!(gist(reason), "HTTP 529: busy (2064)");
        assert_eq!(gist("connection reset"), "connection reset");
    }

    #[test]
    fn a_spent_quota_points_to_another_model_not_to_waiting_a_moment() {
        let spent = error_hint("HTTP 429 Too Many Requests: Usage limit reached for 5 hour. Your limit will reset at 16:06").unwrap();
        assert!(spent.contains("usage limit is spent"));
        let limited = error_hint("HTTP 429 Too Many Requests: slow down").unwrap();
        assert!(limited.contains("wait a moment"));
    }

    /// `line` without escape codes, whatever colors the theme picked.
    fn unstyled(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                chars.by_ref().find(char::is_ascii_alphabetic);
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn the_status_line_drops_detail_before_the_model_as_it_narrows() {
        let status = Status {
            window: Some(200_000),
            context: 38_000,
            prompt: 38_000,
            cached: 30_000,
            completion: 2_000,
            branch: Some("main".into()),
            ..Status::default()
        };
        let line = |width| unstyled(&status_layout("qwen3.7-plus", &status, width));
        let wide = line(120);
        assert!(wide.contains("cached") && wide.contains("⎇ main"));
        for width in 12..120 {
            let shown = line(width);
            assert!(text::width(&shown) <= width, "{width}: {shown}");
            assert!(shown.starts_with("qwen3.7-plus") || width < 12, "{width}: {shown:?}");
        }
        let narrow = line(40);
        assert!(narrow.contains("ctx") && !narrow.contains("cached"));
    }

    #[test]
    fn tool_rows_bold_the_name_and_hang_output_off_a_gutter() {
        assert_eq!(tool_summary("read src/lib.rs"), format!("{BOLD}read{RESET} src/lib.rs"));
        assert_eq!(tool_summary("todo"), format!("{BOLD}todo{RESET}"));
        assert_eq!(unstyled(&diff_line("+12\tlet x = 1;")), "  │   12 + let x = 1;");
        assert!(diff_line("-3\told").contains(&format!("{DANGER}- old")));
        assert_eq!(unstyled(&diff_line("@@ hunk")), "  │ @@ hunk");
    }

    #[test]
    fn the_empty_prompt_hints_at_commands_and_steering_unless_something_else_waits() {
        assert_eq!(placeholder(true, false, false), Some("ask, or / for commands and skills"));
        assert!(placeholder(true, true, false).unwrap().contains("/btw"));
        assert_eq!(placeholder(false, false, false), None);
        assert_eq!(placeholder(true, false, true), None);
    }

    #[test]
    fn the_context_meter_turns_warn_at_80_percent() {
        assert_eq!(unstyled(&gauge(30, 100)), "━━━╌╌╌╌╌╌╌");
        assert!(gauge(79, 100).contains(&LAGOON.to_string()));
        assert!(gauge(80, 100).contains(&WARN.to_string()));
    }

    #[test]
    fn durations_move_up_a_unit() {
        let secs = Duration::from_secs;
        assert_eq!(clock(secs(0)), "0s");
        assert_eq!(clock(secs(59)), "59s");
        assert_eq!(clock(secs(60)), "1m00s");
        assert_eq!(clock(secs(125)), "2m05s");
        assert_eq!(clock(secs(3_600)), "1h00m");
        assert_eq!(clock(secs(3_725)), "1h02m");
        assert_eq!(precise_clock(Duration::from_millis(2_400)), "2.4s");
        assert_eq!(precise_clock(secs(70)), "1m10s");
    }

    #[test]
    fn btw_takes_the_rest_of_the_line() {
        assert_eq!(aside_question("/btw what is this?").as_deref(), Some("what is this?"));
        assert_eq!(aside_question("  /btw  two\nlines ").as_deref(), Some("two\nlines"));
        assert_eq!(aside_question("/btw"), None);
        assert_eq!(aside_question("/btwx y"), None);
        assert_eq!(aside_question("ask /btw y"), None);
    }

    #[test]
    fn short_numbers() {
        assert_eq!(short(950), "950");
        assert_eq!(short(38_000), "38k");
        assert_eq!(short(1_250_000), "1.2M");
    }

    #[test]
    fn typing_in_a_picker_fuzzy_filters_its_options() {
        let models = ["minimax/MiniMax-M3.1-Flash", "qwen/qwen3.7-plus", "qwen/qwen3.8-max", "zai/glm-5.3"];
        let options = models.iter().map(|m| (m.to_string(), String::new(), PickerChoice::Model(m.to_string()))).collect();
        let mut picker = Picker::new("Switch model".into(), options, 3);
        assert!(matches!(picker.choice(), Some(PickerChoice::Model(m)) if m == "zai/glm-5.3"));
        picker.filter("q38".into());
        assert!(matches!(picker.choice(), Some(PickerChoice::Model(m)) if m == "qwen/qwen3.8-max"));
        picker.filter("glm".into());
        assert_eq!(picker.shown, [3]);
        picker.filter("xyz".into());
        assert!(picker.choice().is_none());
        picker.filter(String::new());
        assert_eq!(picker.shown.len(), 4);
    }

    #[test]
    fn a_failed_request_shows_its_status_and_message_and_what_follows() {
        let reason = r#"HTTP 401 Unauthorized: {"error":{"message":"Invalid API key"}} (after 1 attempt(s))"#;
        assert_eq!(gist(reason), "HTTP 401: Invalid API key (after 1 attempt(s))");
    }

    #[test]
    fn a_side_answer_is_one_block_hung_off_the_gutter() {
        let rows = aside_rows("what\nnow?", "It **reads** first.\n\n- one\n- two\n", "Not kept · 4k in", 40);
        let rows: Vec<String> = rows.iter().map(|row| unstyled(row)).collect();
        assert_eq!(rows, ["btw what now?", "  │ It reads first.", "  │", "  │ • one", "  │ • two", "  │", "  │ Not kept · 4k in"]);
        let long = aside_rows("q", &"word ".repeat(30), "n", 30);
        assert!(long.iter().all(|row| text::width(row) <= 30));
        assert!(long.iter().skip(1).all(|row| unstyled(row).starts_with("  │")));
    }

    #[test]
    fn each_kind_of_status_figure_has_its_own_color() {
        let status = Status {
            window: Some(200_000),
            context: 38_000,
            prompt: 38_000,
            cached: 30_000,
            completion: 2_000,
            branch: Some("main*".into()),
            ..Status::default()
        };
        let line = status_layout(&format!("model · effort {}", tint(EFFORT, "high")), &status, 140);
        assert!(line.contains(&format!("{EFFORT}high")));
        assert!(line.contains(&format!("{TOKENS}38k")) && line.contains(&format!("{TOKENS}78%")) && line.contains(&format!("{TOKENS}2k")));
        assert!(line.contains(&format!("{BRANCH}⎇ main")) && line.contains(&format!("{WARN}*")));
        assert!(line.contains(&format!("{FAINT}│")));
        assert!(!line.contains(&format!("{WARN}38k/200k")));
        let full = status_layout("model", &Status { context: 170_000, ..status }, 140);
        assert!(full.contains(&format!("{WARN}170k/200k")));
    }

    #[test]
    fn a_tick_repaints_only_the_rows_that_changed() {
        let rows = |texts: &[&str]| texts.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        let before = rows(&["working 1s", "rule", "❯", "rule", "status"]);
        assert_eq!(changed_rows(&before, &rows(&["working 2s", "rule", "❯", "rule", "status"])), Some(vec![0]));
        assert_eq!(changed_rows(&before, &before), Some(vec![]));
        assert_eq!(changed_rows(&before, &rows(&["rule", "❯", "rule", "status"])), None);
    }

    #[test]
    fn first_sentence_stops_at_the_end_of_a_sentence() {
        assert_eq!(first_sentence("The bug is in pop. Then fix it."), "The bug is in pop.");
        assert_eq!(first_sentence("先看代码。再修复。"), "先看代码。");
    }
}
