//! The interactive terminal UI. The conversation is printed into the terminal's own
//! scrollback, so scrolling, selecting and copying work as usual; only a live area at the
//! bottom is redrawn: what is happening now, the task list, the prompt and a status line.

mod banner;
mod editor;
mod menu;
mod text;

use std::io::{Stdout, Write, stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::cursor::{MoveToColumn, MoveUp};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::agent::{Agent, Controls, Event, Question, Sink, Stop, Todo};
use crate::config::{Config, Thinking};
use crate::setup::Setup;
use crate::llm::{Client, Usage};
use crate::snapshot::Snapshots;
use editor::Editor;
use menu::{Entry, Item, Suggestions};
use text::{ACCENT, BOLD, DIM, GREEN, ITALIC, MAGENTA, Markdown, RED, RESET, YELLOW, wrap};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PROMPT: &str = "\x1b[38;5;75m›\x1b[0m ";
const MAX_INPUT_ROWS: usize = 10;
const MENU_ROWS: usize = 8;
const DIFF_ROWS: usize = 16;

pub struct Start {
    pub model: Option<String>,
    /// A session log to continue.
    pub resume: Option<PathBuf>,
    /// Open the session picker first.
    pub pick_session: bool,
}

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
    let session = Setup::new(config, &cwd, start.model.as_deref()).await?;
    let mut agent = match &start.resume {
        Some(log) => session.resume(log, controls.clone(), sink.clone())?,
        None => session.start(controls.clone(), sink.clone())?,
    };
    let mut snapshots = Snapshots::new(&cwd);
    let mut app = App::new(&agent, controls.clone(), &cwd);
    app.entries = entries(&session);
    app.refresh_choices(session.config());
    app.notify_after = Duration::from_secs(session.config().agent.notify_after.unwrap_or(10));
    let _terminal = TerminalGuard::enter()?;
    app.banner(&agent, &session, &snapshots, start.resume.is_some());
    if start.pick_session {
        app.open_session_picker(&cwd);
    }
    app.draw()?;

    let mut keys = EventStream::new();
    loop {
        let Some(input) = app.read_input(&mut keys).await? else { break };
        match input {
            Input::Prompt(prompt) => {
                snapshots.take();
                app.begin_turn(&prompt);
                let before = Totals::of(&agent);
                let ended = run_turn(&mut app, &mut keys, &mut rx, agent.run(&prompt)).await?;
                app.finish_turn(ended, before, Totals::of(&agent));
                if matches!(app.last_end, Some(LastEnd::Interrupted)) {
                    agent.recover()?;
                }
            }
            Input::Command(command, arg) => {
                if !handle_command(&mut app, &mut agent, &session, &mut snapshots, &mut keys, &mut rx, &cwd, &command, &arg).await? {
                    break;
                }
            }
            Input::Resume(path) => match session.resume(&path, controls.clone(), sink.clone()) {
                Ok(resumed) => {
                    agent = resumed;
                    app.notice(&format!("Resumed the session started with “{}”.", agent.user_prompts().first().map_or("", |s| s.as_str())));
                    app.editor = Editor::with_history(agent.user_prompts());
                }
                Err(e) => app.error(&format!("{e:#}")),
            },
            Input::Model(alias) => match session.config().resolve(Some(&alias)).and_then(Client::new) {
                Ok(client) => {
                    app.status.model = alias.clone();
                    app.status.window = client.model.context_window;
                    app.status.thinking = client.model.thinking.label().to_string();
                    agent.switch_model(client);
                    app.refresh_choices(session.config());
                    app.notice(&format!("Switched to {alias}. The cache starts over with the new model."));
                }
                Err(e) => app.error(&format!("{e:#}")),
            },
            Input::Effort(effort) => {
                let efforts = session.config().efforts(agent.model_alias());
                if efforts.contains(&effort) {
                    agent.set_thinking(Thinking::parse(&effort));
                    app.status.thinking = effort.clone();
                    app.refresh_choices(session.config());
                    app.notice(&format!("Thinking effort {effort} for this session. To keep it, set [thinking] effort in ~/.killi/config.toml."));
                } else {
                    app.error(&format!("{} has no effort “{effort}”; choose one of {}.", agent.model_alias(), efforts.join(", ")));
                }
            }
        }
        app.refresh_branch();
        app.draw()?;
    }
    app.finish_screen()?;
    Ok(ExitCode::SUCCESS)
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
    let ended = loop {
        tokio::select! {
            biased;
            Some(event) = rx.recv() => app.on_agent(event),
            Some(Ok(event)) = keys.next() => {
                if app.on_key_busy(event) {
                    break Ended::Interrupted;
                }
            }
            result = &mut future => break Ended::Done(result),
            _ = tick.tick() => app.spinner += 1,
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
    session: &Setup,
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
        "clear" | "new" => match session.start(agent.controls.clone(), agent_sink(agent)) {
            Ok(fresh) => {
                *agent = fresh;
                app.todos.clear();
                app.reset_usage();
                app.notice("Started a new session.");
            }
            Err(e) => app.error(&format!("{e:#}")),
        },
        "resume" => app.open_session_picker(cwd),
        "model" if arg.is_empty() => app.open_model_picker(session.config()),
        "model" => app.next_input = Some(Input::Model(arg.to_string())),
        "effort" | "thinking" if arg.is_empty() => app.open_effort_picker(session.config()),
        "effort" | "thinking" => app.next_input = Some(Input::Effort(arg.to_lowercase())),
        "reload" => {
            agent.reload_system(&session.system_prompt())?;
            app.notice("Reloaded the system prompt ([prompt] in config.toml), AGENTS.md files and skills. The next request starts the cache over.");
        }
        "usage" | "cost" => app.usage_report(agent),
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
    snapshots.take();
    app.begin_turn(shown);
    let before = Totals::of(agent);
    let ended = run_turn(app, keys, rx, agent.run(prompt)).await?;
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
    let taken = |name: &str| COMMANDS.iter().any(|c| c.name == name || c.aliases.contains(&name));
    let commands = COMMANDS.iter().map(|c| Entry {
        name: c.name.to_string(),
        aliases: c.aliases.iter().map(|a| a.to_string()).collect(),
        description: if c.args.is_empty() { c.about.to_string() } else { format!("{} — {}", c.args, c.about) },
    });
    let skills = session.skills().iter().filter(|s| !taken(&s.name)).map(|s| Entry {
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
    Prompt(String),
    Command(String, String),
    Resume(PathBuf),
    Model(String),
    Effort(String),
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
    selected: usize,
}

#[derive(Clone)]
enum PickerChoice {
    Session(PathBuf),
    Model(String),
    Effort(String),
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
    /// The last tool output or thought, for Ctrl+O.
    detail: Option<(String, String)>,
    status: Status,
    controls: Controls,
    spinner: usize,
    exit_armed: Option<Instant>,
    queued: Vec<String>,
    last_end: Option<LastEnd>,
    cwd: PathBuf,
    notify_after: Duration,
}

#[derive(PartialEq, Clone, Copy)]
enum Kind {
    None,
    Text,
    Tool,
}

impl App {
    fn new(agent: &Agent, controls: Controls, cwd: &Path) -> Self {
        let (columns, rows) = terminal::size().unwrap_or((100, 40));
        let mut app = Self {
            out: stdout(),
            width: usize::from(columns).max(20),
            height: usize::from(rows).max(5),
            cursor_row: 0,
            pending: Vec::new(),
            editor: Editor::with_history(agent.user_prompts()),
            markdown: Markdown::default(),
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
            detail: None,
            status: Status {
                model: agent.model_alias().to_string(),
                thinking: agent.thinking().label().to_string(),
                window: agent.context_window(),
                ..Status::default()
            },
            controls,
            spinner: 0,
            exit_armed: None,
            queued: Vec::new(),
            last_end: None,
            cwd: cwd.to_path_buf(),
            notify_after: Duration::from_secs(10),
        };
        app.refresh_branch();
        app
    }

    fn banner(&mut self, agent: &Agent, session: &Setup, snapshots: &Snapshots, resumed: bool) {
        let home = crate::config::home();
        let cwd = match self.cwd.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => self.cwd.display().to_string(),
        };
        let room = self.width.saturating_sub(41).max(20);
        let cwd = if text::width(&cwd) > room {
            let tail: String = cwd.chars().rev().take(room - 1).collect::<Vec<_>>().into_iter().rev().collect();
            format!("…{tail}")
        } else {
            cwd
        };
        let thinking = thinking_note(agent.thinking().label());
        let thinking = if thinking.is_empty() { String::new() } else { format!(" {DIM}· {thinking}{RESET}") };
        let side = [
            format!("{BOLD}killi{RESET} {DIM}{}{RESET}", env!("CARGO_PKG_VERSION")),
            format!("{DIM}a coding agent for the terminal{RESET}"),
            String::new(),
            format!("{DIM}model {RESET} {}{thinking}", agent.model_alias()),
            format!("{DIM}folder{RESET} {cwd}"),
        ];
        self.commit(String::new());
        for row in banner::render(self.width, &side) {
            self.commit(row);
        }
        self.commit(String::new());
        let info = session.describe();
        if !info.is_empty() {
            self.commit(format!("{DIM}{info}{RESET}"));
        }
        if let Some(why) = &snapshots.disabled {
            self.commit(format!("{DIM}{why}{RESET}"));
        }
        if resumed {
            let prompts = agent.user_prompts();
            self.commit(format!("{DIM}Resumed a session with {} prompt(s); the last was “{}”.{RESET}", prompts.len(), prompts.last().map_or("", |s| s.lines().next().unwrap_or(""))));
        }
        self.commit(format!("{DIM}/ commands and skills · Shift+Enter new line · Shift+Tab plan mode · Esc interrupts{RESET}"));
        self.commit(String::new());
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
                self.editor.insert(&text);
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
                let text = self.editor.take();
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return Idle::Continue;
                }
                if let Some(command) = trimmed.strip_prefix('/') {
                    let (name, arg) = command.split_once(char::is_whitespace).unwrap_or((command, ""));
                    return Idle::Input(Input::Command(name.to_string(), arg.trim().to_string()));
                }
                return Idle::Input(Input::Prompt(text));
            }
            _ => self.edit_key(key),
        }
        Idle::Continue
    }

    fn on_key_picker(&mut self, key: KeyEvent) -> Idle {
        let picker = self.picker.as_mut().expect("picker is open");
        match key.code {
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => picker.selected = (picker.selected + 1).min(picker.options.len().saturating_sub(1)),
            KeyCode::Esc => self.picker = None,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.picker = None,
            KeyCode::Enter => {
                let picker = self.picker.take().expect("picker is open");
                if let Some((_, _, choice)) = picker.options.get(picker.selected) {
                    return Idle::Input(match choice.clone() {
                        PickerChoice::Session(path) => Input::Resume(path),
                        PickerChoice::Model(alias) => Input::Model(alias),
                        PickerChoice::Effort(effort) => Input::Effort(effort),
                    });
                }
            }
            _ => {}
        }
        Idle::Continue
    }

    /// Keys for the slash menu: ↑↓ choose, Tab completes, Enter runs a command or fills in an
    /// argument, Esc closes it. None lets the key through, as Enter is after a command.
    fn on_key_menu(&mut self, key: KeyEvent, menu: &Suggestions) -> Option<Idle> {
        let count = menu.items.len();
        let selected = self.menu_selected.min(count - 1);
        match key.code {
            KeyCode::Up => self.menu_selected = (selected + count - 1) % count,
            KeyCode::Down => self.menu_selected = (selected + 1) % count,
            KeyCode::Esc => self.menu_closed = Some(self.editor.text().to_string()),
            KeyCode::Tab => self.choose(menu, selected),
            KeyCode::Enter if !newline_key(&key) => {
                self.choose(menu, selected);
                return if menu.command { None } else { Some(Idle::Continue) };
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
                self.editor.insert(&text);
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
                if self.editor.text().trim_start().starts_with('/') {
                    self.notice("Commands run when the agent is idle: wait for this turn to finish, or press Esc.");
                    return false;
                }
                let text = self.editor.take();
                if !text.trim().is_empty() {
                    self.controls.steer.borrow_mut().push_back(text.clone());
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
        self.commit(format!("  {GREEN}↳{RESET} {answer}"));
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
            KeyCode::Char('l') if ctrl => {
                let _ = execute!(self.out, Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0));
                self.cursor_row = 0;
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
        self.commit(String::new());
        for (i, line) in prompt.lines().enumerate() {
            let lead = if i == 0 { format!("{ACCENT}›{RESET} ") } else { "  ".into() };
            self.commit(format!("{lead}{BOLD}{line}{RESET}"));
        }
        self.commit(String::new());
        self.last_kind = Kind::None;
        self.begin_busy();
    }

    fn begin_busy(&mut self) {
        self.busy = Some(Instant::now());
        self.last_end = None;
    }

    fn end_busy(&mut self) {
        self.flush_partial();
        self.end_thinking();
        self.running.clear();
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
                self.commit(format!("{YELLOW}Interrupted.{RESET} {DIM}Tell killi what to do instead.{RESET}"));
                self.controls.steer.borrow_mut().clear();
            }
            Ended::Done(Err(e)) => {
                self.last_end = Some(LastEnd::Interrupted);
                self.error(&format!("{e:#}"));
            }
            Ended::Done(Ok((_, stop))) => {
                self.last_end = Some(LastEnd::Done);
                match stop {
                    Stop::MaxTokens => self.notice("The reply hit the output token limit."),
                    Stop::RepeatBreaker => self.notice("Stopped: the agent kept repeating the same tool call."),
                    Stop::Completed => {}
                }
            }
        }
        let prompt = after.prompt - before.prompt;
        let cached = (after.cached - before.cached) * 100 / prompt.max(1);
        self.commit(String::new());
        let requests = after.requests - before.requests;
        self.commit(format!(
            "{DIM}{:.0}s · {requests} request{} · {} in, {cached}% cached · {} out{RESET}",
            elapsed.as_secs_f32(),
            if requests == 1 { "" } else { "s" },
            short(prompt),
            short(after.completion - before.completion)
        ));
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
                if self.last_kind == Kind::Tool {
                    self.commit(String::new());
                }
                self.last_kind = Kind::Text;
                self.partial.push_str(text);
                while let Some(end) = self.partial.find('\n') {
                    let line: String = self.partial.drain(..=end).collect();
                    let line = line.trim_end_matches('\n');
                    if line.trim().is_empty() {
                        self.held_blank = true;
                        continue;
                    }
                    if std::mem::take(&mut self.held_blank) {
                        self.commit(String::new());
                    }
                    let styled = self.markdown.line(line);
                    self.commit_wrapped(styled);
                }
            }
            Event::Reasoning(text) => match &mut self.thinking {
                Some((_, buffer)) => buffer.push_str(&text),
                None => self.thinking = Some((Instant::now(), text)),
            },
            Event::Retry { attempt, wait, reason } => {
                let line = format!("{DIM}{YELLOW}retry {attempt} in {:.0}s:{RESET}{DIM} {}{RESET}", wait.as_secs_f32(), gist(&reason));
                self.commit(fit(&line, self.width));
            }
            Event::ToolStart { id, summary, .. } => {
                self.flush_partial();
                self.end_thinking();
                if self.last_kind == Kind::Text {
                    self.commit(String::new());
                }
                self.last_kind = Kind::Tool;
                self.running.push(Running { id, summary, label: None, started: Instant::now() });
            }
            Event::ToolEnd { id, is_error, brief, display, output } => {
                let summary = self.take_running(&id).map(|r| r.summary).unwrap_or_default();
                self.tool_line(None, &summary, is_error, &brief);
                if let Some(display) = display {
                    let lines: Vec<&str> = display.lines().collect();
                    for line in lines.iter().take(DIFF_ROWS) {
                        self.commit(fit(&diff_line(line), self.width));
                    }
                    if lines.len() > DIFF_ROWS {
                        let more = lines.len() - DIFF_ROWS;
                        self.commit(format!("        {DIM}… {more} more line{}{RESET}", if more == 1 { "" } else { "s" }));
                    }
                }
                self.detail = Some((summary, output));
            }
            Event::Usage(usage) => self.add_usage(&usage, true),
            Event::Todos(todos) => self.todos = todos,
            Event::Ask(question) => {
                self.flush_partial();
                self.end_thinking();
                if question.plan {
                    self.commit(format!("{BOLD}{MAGENTA}Plan{RESET}"));
                    let mut markdown = Markdown::default();
                    for line in question.text.lines() {
                        self.commit_wrapped(format!("  {}", markdown.line(line)));
                    }
                    self.commit(String::new());
                } else {
                    self.commit(format!("{BOLD}{MAGENTA}?{RESET} {BOLD}{}{RESET}", question.text));
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
                let summary = self.take_running(&id).map(|r| r.summary).unwrap_or_default();
                self.tool_line(Some(&label), &summary, is_error, &brief);
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

    fn tool_line(&mut self, label: Option<&str>, summary: &str, is_error: bool, brief: &str) {
        let (bullet, note) = if is_error {
            (format!("{RED}●{RESET}"), format!(" {RED}{brief}{RESET}"))
        } else if brief.is_empty() {
            (format!("{GREEN}●{RESET}"), String::new())
        } else {
            (format!("{GREEN}●{RESET}"), format!(" {DIM}· {brief}{RESET}"))
        };
        let lead = label.map_or(0, |l| text::width(l) + 5);
        let summary = clip(summary, self.width.saturating_sub(lead + 2 + text::width(&note)).max(20));
        let line = match label {
            Some(label) => format!("  {DIM}↳ {label}:{RESET} {bullet} {DIM}{summary}{RESET}{note}"),
            None => format!("{bullet} {summary}{note}"),
        };
        self.commit(fit(&line, self.width));
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
            self.commit_wrapped(styled);
        }
        self.partial.clear();
    }

    /// Ends a thought: one dim line when it took a while, with the full text behind Ctrl+O.
    fn end_thinking(&mut self) {
        if let Some((started, text)) = self.thinking.take() {
            let seconds = started.elapsed().as_secs_f32();
            self.detail = Some(("Thinking".into(), text.clone()));
            if seconds < 2.0 {
                return;
            }
            let first = first_sentence(&text);
            let gist = if first.is_empty() { String::new() } else { format!(": {first}") };
            let line = fit(&format!("{DIM}{ITALIC}∴ Thought for {seconds:.0}s{gist}{RESET}"), self.width);
            self.commit(line);
            self.detail = Some(("Thinking".into(), text));
        }
    }

    fn show_detail(&mut self) {
        let Some((title, body)) = self.detail.clone() else {
            self.notice("Nothing to expand yet.");
            return;
        };
        self.commit(format!("{DIM}── {title} ──{RESET}"));
        let lines: Vec<&str> = body.lines().collect();
        for line in lines.iter().take(400) {
            self.commit(format!("{DIM}{line}{RESET}"));
        }
        if lines.len() > 400 {
            self.commit(format!("{DIM}… {} more lines{RESET}", lines.len() - 400));
        }
    }

    // ---------- commands ----------

    fn help(&mut self) {
        self.commit(format!("{BOLD}Commands{RESET} {DIM}type / to search them and your skills{RESET}"));
        for c in COMMANDS {
            let aliases: String = c.aliases.iter().map(|a| format!(", /{a}")).collect();
            let args = if c.args.is_empty() { String::new() } else { format!(" {}", c.args) };
            self.commit(format!("  {ACCENT}/{}{RESET}{DIM}{aliases}{args}{RESET}  {}", c.name, c.about));
        }
        self.commit(format!("  {ACCENT}/<skill>{RESET}{DIM} [request]{RESET}  Run a skill"));
        self.commit(format!("{BOLD}Keys{RESET}"));
        for (key, about) in KEYS {
            self.commit(format!("  {ACCENT}{key}{RESET} {DIM}{about}{RESET}"));
        }
    }

    fn usage_report(&mut self, agent: &Agent) {
        let usage = &agent.stats.usage;
        let cached = usage.cached * 100 / usage.prompt.max(1);
        self.commit(format!(
            "{DIM}This session: {} requests, {} tool calls, {} input tokens ({cached}% cached), {} output tokens.{RESET}",
            agent.stats.requests,
            agent.stats.tool_calls,
            short(usage.prompt),
            short(usage.completion)
        ));
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
        self.picker = Some(Picker { title: "Resume a session".into(), options, selected: 0 });
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
        self.picker = Some(Picker { title: "Switch model".into(), options, selected });
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
        self.picker = Some(Picker { title: format!("Thinking effort for {}", self.status.model), options, selected });
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

    /// Queues a line for the scrollback, never two blank lines in a row.
    fn commit(&mut self, line: String) {
        let blank = line.is_empty();
        if blank && self.last_blank {
            return;
        }
        self.last_blank = blank;
        self.pending.push(line);
    }

    fn notice(&mut self, text: &str) {
        self.commit(format!("{DIM}{text}{RESET}"));
    }

    fn error(&mut self, text: &str) {
        self.commit(format!("{RED}Error:{RESET} {text}"));
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        self.width = usize::from(columns).max(20);
        self.height = usize::from(rows).max(5);
        if self.cursor_row > 0 {
            let _ = queue!(self.out, MoveUp(self.cursor_row as u16));
        }
        let _ = queue!(self.out, MoveToColumn(0), Clear(ClearType::FromCursorDown));
        self.cursor_row = 0;
    }

    /// The live area's rows and where the cursor goes in them.
    fn live(&self) -> (Vec<String>, usize, usize) {
        let width = self.width;
        let mut rows: Vec<String> = Vec::new();
        if !self.partial.is_empty() {
            let mut markdown = Markdown::default();
            let preview = wrap(&markdown.line(&self.partial), width);
            rows.extend(preview.into_iter().rev().take(6).rev());
        }
        if let Some(started) = self.busy {
            let spin = SPINNER[self.spinner % SPINNER.len()];
            if let Some((since, text)) = &self.thinking {
                let first = first_sentence(text);
                rows.push(fit(&format!("{MAGENTA}{spin}{RESET} {DIM}thinking {}s {first}{RESET}", since.elapsed().as_secs()), width));
            }
            for running in self.running.iter().take(5) {
                let label = running.label.as_ref().map(|l| format!("{DIM}↳ {l}:{RESET} ")).unwrap_or_default();
                rows.push(fit(&format!("{ACCENT}{spin}{RESET} {label}{} {DIM}{}s{RESET}", running.summary, running.started.elapsed().as_secs()), width));
            }
            if self.thinking.is_none() && self.running.is_empty() && self.asking.is_none() {
                rows.push(fit(&format!("{ACCENT}{spin}{RESET} {DIM}working {}s · Esc to interrupt{RESET}", started.elapsed().as_secs()), width));
            }
            for queued in &self.queued {
                rows.push(fit(&format!("{DIM}› {} (sent at the next step){RESET}", queued.replace('\n', " ")), width));
            }
        }
        if self.todos.iter().any(|t| t.status != "completed") {
            for todo in self.todos.iter().take(8) {
                let (mark, style) = match todo.status.as_str() {
                    "completed" => ("☑", DIM),
                    "in_progress" => ("◐", YELLOW),
                    _ => ("☐", ""),
                };
                rows.push(fit(&format!("  {style}{mark} {}{RESET}", todo.content), width));
            }
        }
        let mode = self.controls.plan.get().then_some("plan mode · Shift+Tab leaves it");
        rows.push(rule(mode, width));
        let (input, cursor_row, cursor_col) = self.editor.render(PROMPT, width);
        let first = cursor_row.saturating_sub(MAX_INPUT_ROWS - 1);
        let top = rows.len();
        rows.extend(input.into_iter().skip(first).take(MAX_INPUT_ROWS));
        let mut cursor = (top + cursor_row - first, cursor_col);
        rows.push(rule(mode.map(|_| ""), width));
        let room = (self.height - 1).saturating_sub(rows.len() - top + 3).min(MENU_ROWS);
        if let Some(menu) = self.suggestions() {
            let visible = menu.items.len().min(room);
            if visible > 0 {
                rows.extend(self.menu_rows(&menu, visible));
            }
        }
        if let Some(picker) = &self.picker {
            let count = picker.options.len();
            let visible = count.min(room.max(1));
            let options: Vec<(&str, &str)> = picker.options.iter().map(|(name, note, _)| (name.as_str(), note.as_str())).collect();
            rows.extend(choice_rows(&options, picker.selected, visible, width, width * 3 / 5));
            let position = if count > visible { format!("{}/{count} · ", picker.selected + 1) } else { String::new() };
            rows.push(fit(&format!("  {DIM}{}{RESET} {DIM}· {position}↑↓ choose · Enter selects · Esc closes{RESET}", picker.title), width));
        }
        if let Some(asking) = &self.asking {
            let names: Vec<String> = asking.question.options.iter().enumerate().map(|(i, o)| format!("{}. {o}", i + 1)).collect();
            let options: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "")).collect();
            let visible = options.len().min(room.max(1));
            rows.extend(choice_rows(&options, asking.selected.min(options.len().saturating_sub(1)), visible, width, width));
            let hint = if asking.question.plan { "or type what to change" } else { "or type your own answer" };
            let choose = if options.is_empty() { String::new() } else { "↑↓ and Enter to choose, ".into() };
            rows.push(fit(&format!("  {DIM}{choose}{hint} · Esc to dismiss{RESET}"), width));
        }
        rows.push(fit(&self.status_line(), width));
        let excess = rows.len().saturating_sub(self.height - 1);
        if excess > 0 {
            let cut = excess.min(top);
            rows.drain(..cut);
            cursor.0 -= cut;
        }
        (rows, cursor.0, cursor.1)
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
        let enter = if menu.command { "Enter runs" } else { "Enter fills in" };
        rows.push(fit(&format!("  {DIM}{position}↑↓ choose · Tab completes · {enter} · Esc closes{RESET}"), self.width));
        rows
    }

    fn status_line(&self) -> String {
        let mut parts = vec![format!("{RESET}{}{DIM}", self.status.model)];
        let thinking = thinking_note(&self.status.thinking);
        if !thinking.is_empty() {
            parts.push(thinking);
        }
        if self.controls.plan.get() {
            parts.push(format!("{YELLOW}plan mode{RESET}{DIM}"));
        }
        if self.controls.swarm.get() {
            parts.push(format!("{MAGENTA}swarm mode{RESET}{DIM}"));
        }
        parts.push(match self.status.window {
            Some(window) => format!("ctx {} {}/{}", gauge(self.status.context, window), short(self.status.context), short(window)),
            None => format!("ctx {}", short(self.status.context)),
        });
        if self.status.prompt > 0 {
            let cached = self.status.cached * 100 / self.status.prompt.max(1);
            parts.push(format!("{} in · {cached}% cached · {} out", short(self.status.prompt), short(self.status.completion)));
        }
        if let Some(branch) = &self.status.branch {
            parts.push(format!("⎇ {branch}"));
        }
        format!("{DIM}{}{RESET}", parts.join(" · "))
    }

    fn draw(&mut self) -> Result<()> {
        let (rows, cursor_row, cursor_col) = self.live();
        if self.cursor_row > 0 {
            queue!(self.out, MoveUp(self.cursor_row as u16))?;
        }
        queue!(self.out, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
        for line in self.pending.drain(..) {
            write!(self.out, "{line}{RESET}\r\n")?;
        }
        write!(self.out, "{}", rows.join("\r\n"))?;
        let last = rows.len().saturating_sub(1);
        if last > cursor_row {
            queue!(self.out, MoveUp((last - cursor_row) as u16))?;
        }
        queue!(self.out, MoveToColumn(cursor_col as u16))?;
        self.cursor_row = cursor_row;
        self.out.flush()?;
        Ok(())
    }

    /// Leaves the conversation on screen and puts the shell prompt below it.
    fn finish_screen(&mut self) -> Result<()> {
        if self.cursor_row > 0 {
            queue!(self.out, MoveUp(self.cursor_row as u16))?;
        }
        queue!(self.out, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
        for line in self.pending.drain(..) {
            write!(self.out, "{line}{RESET}\r\n")?;
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
            let line = if start + i == selected {
                format!("{ACCENT}❯ {BOLD}{name}{RESET}{pad}  {note}")
            } else {
                format!("  {name}{pad}  {DIM}{note}{RESET}")
            };
            fit(&line, width)
        })
        .collect()
}

/// A full-width line around the prompt, in yellow with `label` on it in plan mode.
fn rule(label: Option<&str>, width: usize) -> String {
    match label {
        None => format!("{DIM}{}{RESET}", "─".repeat(width)),
        Some("") => format!("{YELLOW}{}{RESET}", "─".repeat(width)),
        Some(label) => {
            let rest = width.saturating_sub(text::width(label) + 4);
            format!("{YELLOW}── {BOLD}{label}{RESET}{YELLOW} {}{RESET}", "─".repeat(rest))
        }
    }
}

/// How full the context window is, as a ten-cell bar that turns yellow, then red.
fn gauge(used: u64, window: u64) -> String {
    let filled = (used.saturating_mul(10).div_ceil(window.max(1)) as usize).min(10);
    let color = match used * 100 / window.max(1) {
        0..50 => GREEN,
        50..80 => YELLOW,
        _ => RED,
    };
    format!("{RESET}{color}{}{RESET}{DIM}{}", "━".repeat(filled), "─".repeat(10 - filled))
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

/// The effort as the banner and status line show it; nothing for the provider's default.
fn thinking_note(thinking: &str) -> String {
    match thinking {
        "on" => String::new(),
        "off" => "thinking off".into(),
        effort => format!("effort {effort}"),
    }
}

fn fit(line: &str, width: usize) -> String {
    wrap(line, width).into_iter().next().unwrap_or_default()
}

/// An error reason with a JSON body cut down to its status and message: `HTTP 529: busy`.
fn gist(reason: &str) -> String {
    let reason = reason.replace('\n', " ");
    let Some(start) = reason.find('{') else { return reason };
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&reason[start..]) else { return reason };
    let Some(message) = body.pointer("/error/message").or(body.get("message")).and_then(|m| m.as_str()) else { return reason };
    let status: Vec<&str> = reason[..start].split_whitespace().take(2).collect();
    match status.as_slice() {
        ["HTTP", code] => format!("HTTP {}: {message}", code.trim_end_matches(':')),
        _ => message.to_string(),
    }
}

/// A row of the edit tool's diff: the sign, line number and text, colored by the sign.
fn diff_line(line: &str) -> String {
    let Some((head, text)) = line.split_once('\t').filter(|(head, _)| head.len() > 1 && head.is_char_boundary(1)) else {
        return format!("        {DIM}{line}{RESET}");
    };
    let (sign, number) = head.split_at(1);
    let color = match sign {
        "+" => GREEN,
        "-" => RED,
        _ => DIM,
    };
    format!("  {DIM}{number:>5}{RESET} {color}{sign} {text}{RESET}")
}

fn first_sentence(text: &str) -> String {
    let text = text.trim().replace('\n', " ");
    let end = text.find(['.', '。', '!', '?']).map_or(text.len(), |i| i + text[i..].chars().next().map_or(1, char::len_utf8));
    crate::llm::truncate(&text[..end], 100).to_string()
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

/// A desktop notification: the terminal's own when it supports OSC 9, otherwise macOS's.
fn notify(body: &str) {
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    if ["iTerm.app", "WezTerm", "ghostty"].contains(&program.as_str()) || std::env::var_os("KITTY_WINDOW_ID").is_some() {
        let mut out = stdout();
        let _ = write!(out, "\x1b]9;killi: {body}\x07");
        let _ = out.flush();
    } else {
        let script = format!("display notification \"{}\" with title \"killi\"", body.replace('"', "'"));
        let _ = std::process::Command::new("/usr/bin/osascript").args(["-e", &script]).spawn();
    }
}

struct Command {
    name: &'static str,
    /// Other names the menu finds it by. `/q`, `/h` and `/?` work too, unlisted.
    aliases: &'static [&'static str],
    args: &'static str,
    about: &'static str,
}

/// The built-in commands, in the order the menu lists them.
const COMMANDS: &[Command] = &[
    Command { name: "model", aliases: &[], args: "[alias]", about: "Switch model" },
    Command { name: "effort", aliases: &["thinking"], args: "[level]", about: "Switch thinking effort" },
    Command { name: "new", aliases: &["clear"], args: "", about: "Start a fresh session in this folder" },
    Command { name: "resume", aliases: &[], args: "", about: "Continue an earlier session in this folder" },
    Command { name: "compact", aliases: &[], args: "[focus]", about: "Summarize the conversation to free context" },
    Command { name: "undo", aliases: &[], args: "", about: "Put files back as they were before the last turn" },
    Command { name: "plan", aliases: &[], args: "", about: "Toggle plan mode (also Shift+Tab)" },
    Command { name: "swarm", aliases: &[], args: "[task|on|off]", about: "Swarm mode: split work across many subagents" },
    Command { name: "usage", aliases: &["cost"], args: "", about: "Tokens and requests this session" },
    Command { name: "reload", aliases: &[], args: "", about: "Re-read [prompt], AGENTS.md files and skills" },
    Command { name: "help", aliases: &[], args: "", about: "Commands and keys" },
    Command { name: "exit", aliases: &["quit"], args: "", about: "Quit killi (also Ctrl+D)" },
];

const KEYS: &[(&str, &str)] = &[
    ("Enter", "send; while the agent works, sends your message at its next step"),
    ("Shift+Enter, Ctrl+J, \\ then Enter", "new line"),
    ("Esc", "interrupt the agent"),
    ("Shift+Tab", "plan mode"),
    ("Ctrl+O", "show the last tool output or thought in full"),
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
        let _ = execute!(out, DisableBracketedPaste);
        let _ = terminal::disable_raw_mode();
        let _ = writeln!(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_reasons_keep_the_status_and_message() {
        let reason = r#"HTTP 529 <unknown status code>: {"type":"error","error":{"type":"overloaded_error","message":"busy (2064)"}}"#;
        assert_eq!(gist(reason), "HTTP 529: busy (2064)");
        assert_eq!(gist("connection reset"), "connection reset");
    }

    #[test]
    fn short_numbers() {
        assert_eq!(short(950), "950");
        assert_eq!(short(38_000), "38k");
        assert_eq!(short(1_250_000), "1.2M");
    }

    #[test]
    fn first_sentence_stops_at_the_end_of_a_sentence() {
        assert_eq!(first_sentence("The bug is in pop. Then fix it."), "The bug is in pop.");
        assert_eq!(first_sentence("先看代码。再修复。"), "先看代码。");
    }
}
