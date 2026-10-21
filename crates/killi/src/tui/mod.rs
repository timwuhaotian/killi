//! The interactive terminal UI. The conversation is printed into the terminal's own
//! scrollback, so scrolling, selecting and copying work as usual; only a live area at the
//! bottom is redrawn: what is happening now, the task list, the prompt and a status line.

mod editor;
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
use crate::config::Config;
use crate::setup::Setup;
use crate::llm::{Client, Usage};
use crate::snapshot::Snapshots;
use editor::Editor;
use text::{BLUE, BOLD, DIM, GREEN, ITALIC, MAGENTA, Markdown, RED, RESET, YELLOW, wrap};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PROMPT: &str = "\x1b[34m›\x1b[0m ";
const MAX_INPUT_ROWS: usize = 10;

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
                    agent.switch_model(client);
                    app.notice(&format!("Switched to {alias}. The cache starts over with the new model."));
                }
                Err(e) => app.error(&format!("{e:#}")),
            },
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
        "exit" | "quit" => return Ok(false),
        "help" => app.help(),
        "plan" => app.toggle_plan(),
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
        "model" => app.pending_model = Some(arg.to_string()),
        "reload" => {
            agent.reload_system(&session.system_prompt())?;
            app.notice("Reloaded the system prompt, AGENTS.md files and skills. The next request starts the cache over.");
        }
        "usage" | "cost" => app.usage_report(agent),
        other => match session.skill_prompt(other, arg) {
            Some(prompt) => {
                snapshots.take();
                app.begin_turn(format!("/{other} {arg}").trim_end());
                let before = Totals::of(agent);
                let ended = run_turn(app, keys, rx, agent.run(&prompt)).await?;
                app.finish_turn(ended, before, Totals::of(agent));
                if matches!(app.last_end, Some(LastEnd::Interrupted)) {
                    agent.recover()?;
                }
            }
            None => app.error(&format!("Unknown command /{other}. Type /help for the list.")),
        },
    }
    Ok(true)
}

fn agent_sink(agent: &Agent) -> Sink {
    agent.sink()
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
    options: Vec<(String, PickerChoice)>,
    selected: usize,
}

#[derive(Clone)]
enum PickerChoice {
    Session(PathBuf),
    Model(String),
}

#[derive(Default)]
struct Status {
    model: String,
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
    /// Rows from the top of the live area to the cursor, to find the area again.
    cursor_row: usize,
    /// Lines waiting to be printed above the live area.
    pending: Vec<String>,
    editor: Editor,
    markdown: Markdown,
    /// Streamed text after the last line break.
    partial: String,
    last_kind: Kind,
    thinking: Option<(Instant, String)>,
    running: Vec<Running>,
    todos: Vec<Todo>,
    asking: Option<Asking>,
    picker: Option<Picker>,
    pending_model: Option<String>,
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
        let (columns, _) = terminal::size().unwrap_or((100, 40));
        let mut app = Self {
            out: stdout(),
            width: usize::from(columns).max(20),
            cursor_row: 0,
            pending: Vec::new(),
            editor: Editor::with_history(agent.user_prompts()),
            markdown: Markdown::default(),
            partial: String::new(),
            last_kind: Kind::None,
            thinking: None,
            running: Vec::new(),
            todos: Vec::new(),
            asking: None,
            picker: None,
            pending_model: None,
            busy: None,
            detail: None,
            status: Status { model: agent.model_alias().to_string(), window: agent.context_window(), ..Status::default() },
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
        self.commit(format!("{BOLD}killi{RESET} {DIM}{} · {} · {cwd}{RESET}", env!("CARGO_PKG_VERSION"), agent.model_alias()));
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
        self.commit(format!("{DIM}Enter sends · Shift+Enter or Ctrl+J for a new line · Shift+Tab plan mode · Esc interrupts · /help{RESET}"));
        self.commit(String::new());
    }

    // ---------- input ----------

    async fn read_input(&mut self, keys: &mut EventStream) -> Result<Option<Input>> {
        if let Some(alias) = self.pending_model.take() {
            return Ok(Some(Input::Model(alias)));
        }
        loop {
            let Some(event) = keys.next().await else { return Ok(None) };
            let event = event?;
            let result = self.on_key_idle(event);
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
            TermEvent::Resize(columns, _) => {
                self.resize(columns);
                return Idle::Continue;
            }
            TermEvent::Key(key) if key.kind != KeyEventKind::Release => key,
            _ => return Idle::Continue,
        };
        if self.picker.is_some() {
            return self.on_key_picker(key);
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
                if let Some((_, choice)) = picker.options.get(picker.selected) {
                    return Idle::Input(match choice.clone() {
                        PickerChoice::Session(path) => Input::Resume(path),
                        PickerChoice::Model(alias) => Input::Model(alias),
                    });
                }
            }
            _ => {}
        }
        Idle::Continue
    }

    /// Keys while the agent works. Returns true to interrupt it.
    fn on_key_busy(&mut self, event: TermEvent) -> bool {
        let key = match event {
            TermEvent::Paste(text) => {
                self.editor.insert(&text);
                return false;
            }
            TermEvent::Resize(columns, _) => {
                self.resize(columns);
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
        if let Some(name) = COMMANDS.iter().map(|(name, _)| *name).find(|name| name.starts_with(partial)) {
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

    // ---------- agent events ----------

    fn begin_turn(&mut self, prompt: &str) {
        self.commit(String::new());
        for (i, line) in prompt.lines().enumerate() {
            let lead = if i == 0 { format!("{BLUE}›{RESET} ") } else { "  ".into() };
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
        self.commit(format!(
            "{DIM}{:.0}s · {} request(s) · {} in, {cached}% cached · {} out{RESET}",
            elapsed.as_secs_f32(),
            after.requests - before.requests,
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
                self.end_thinking();
                if self.last_kind == Kind::Tool {
                    self.commit(String::new());
                }
                self.last_kind = Kind::Text;
                self.partial.push_str(&text);
                while let Some(end) = self.partial.find('\n') {
                    let line: String = self.partial.drain(..=end).collect();
                    let styled = self.markdown.line(line.trim_end_matches('\n'));
                    self.commit(styled);
                }
            }
            Event::Reasoning(text) => match &mut self.thinking {
                Some((_, buffer)) => buffer.push_str(&text),
                None => self.thinking = Some((Instant::now(), text)),
            },
            Event::Retry { attempt, wait, reason } => {
                self.commit(format!("{DIM}{YELLOW}retry {attempt} in {:.0}s:{RESET}{DIM} {}{RESET}", wait.as_secs_f32(), crate::llm::truncate(&reason, 200)));
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
                    for line in display.lines().take(16) {
                        self.commit(diff_line(line));
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
                        self.commit(format!("  {}", markdown.line(line)));
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
        let summary = summary.replace(&format!("{}/", self.cwd.display()), "").replace(&self.cwd.display().to_string(), ".");
        let (bullet, note) = if is_error {
            (format!("{RED}●{RESET}"), format!(" {RED}{brief}{RESET}"))
        } else if brief.is_empty() {
            (format!("{GREEN}●{RESET}"), String::new())
        } else {
            (format!("{GREEN}●{RESET}"), format!(" {DIM}· {brief}{RESET}"))
        };
        match label {
            Some(label) => self.commit(format!("  {DIM}↳ {label}:{RESET} {bullet} {DIM}{summary}{RESET}{note}")),
            None => self.commit(format!("{bullet} {summary}{note}")),
        }
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
        if !self.partial.is_empty() {
            let line = std::mem::take(&mut self.partial);
            let styled = self.markdown.line(&line);
            self.commit(styled);
        }
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
            self.commit(format!("{DIM}{ITALIC}∴ Thought for {seconds:.0}s{gist}{RESET}"));
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
        self.commit(format!("{BOLD}Commands{RESET}"));
        for (name, about) in COMMANDS {
            self.commit(format!("  {BLUE}/{name}{RESET} {DIM}{about}{RESET}"));
        }
        self.commit(format!("{BOLD}Keys{RESET}"));
        for (key, about) in KEYS {
            self.commit(format!("  {BLUE}{key}{RESET} {DIM}{about}{RESET}"));
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
                (format!("{age:>8}  {} {DIM}({} prompts){RESET}", crate::llm::truncate(&s.first_prompt, 70), s.prompts), PickerChoice::Session(s.path))
            })
            .collect();
        self.picker = Some(Picker { title: "Resume a session".into(), options, selected: 0 });
    }

    fn open_model_picker(&mut self, config: &Config) {
        let options: Vec<_> = config
            .models
            .iter()
            .map(|(alias, model)| {
                let current = if *alias == self.status.model { " (current)" } else { "" };
                (format!("{alias} {DIM}{}{current}{RESET}", model.provider), PickerChoice::Model(alias.clone()))
            })
            .collect();
        let selected = options.iter().position(|(_, c)| matches!(c, PickerChoice::Model(a) if *a == self.status.model)).unwrap_or(0);
        self.picker = Some(Picker { title: "Switch model".into(), options, selected });
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

    fn commit(&mut self, line: String) {
        self.pending.push(line);
    }

    fn notice(&mut self, text: &str) {
        self.commit(format!("{DIM}{text}{RESET}"));
    }

    fn error(&mut self, text: &str) {
        self.commit(format!("{RED}Error:{RESET} {text}"));
    }

    fn resize(&mut self, columns: u16) {
        self.width = usize::from(columns).max(20);
        let _ = queue!(self.out, MoveUp(self.cursor_row as u16), MoveToColumn(0), Clear(ClearType::FromCursorDown));
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
                rows.push(fit(&format!("{BLUE}{spin}{RESET} {label}{} {DIM}{}s{RESET}", running.summary, running.started.elapsed().as_secs()), width));
            }
            if self.thinking.is_none() && self.running.is_empty() && self.asking.is_none() {
                rows.push(fit(&format!("{BLUE}{spin}{RESET} {DIM}working {}s · Esc to interrupt{RESET}", started.elapsed().as_secs()), width));
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
        if let Some(asking) = &self.asking {
            for (i, option) in asking.question.options.iter().enumerate() {
                let line = if i == asking.selected {
                    format!("  {BLUE}❯ {}. {option}{RESET}", i + 1)
                } else {
                    format!("    {}. {option}", i + 1)
                };
                rows.push(fit(&line, width));
            }
            let hint = if asking.question.plan { "or type what to change" } else { "or type your own answer" };
            rows.push(fit(&format!("  {DIM}↑↓ and Enter to choose, {hint} · Esc to dismiss{RESET}"), width));
        }
        if let Some(picker) = &self.picker {
            rows.push(fit(&format!("{BOLD}{}{RESET} {DIM}↑↓ Enter · Esc{RESET}", picker.title), width));
            let start = picker.selected.saturating_sub(7);
            for (i, (label, _)) in picker.options.iter().enumerate().skip(start).take(8) {
                let line = if i == picker.selected { format!("{BLUE}❯ {label}{RESET}") } else { format!("  {label}") };
                rows.push(fit(&line, width));
            }
        }
        rows.push(format!("{DIM}{}{RESET}", "─".repeat(width)));
        let (input, cursor_row, cursor_col) = self.editor.render(PROMPT, width);
        let first = cursor_row.saturating_sub(MAX_INPUT_ROWS - 1);
        let top = rows.len();
        rows.extend(input.into_iter().skip(first).take(MAX_INPUT_ROWS));
        let cursor = (top + cursor_row - first, cursor_col);
        rows.push(fit(&self.status_line(), width));
        (rows, cursor.0, cursor.1)
    }

    fn status_line(&self) -> String {
        let mut parts = vec![self.status.model.clone()];
        if self.controls.plan.get() {
            parts.push(format!("{YELLOW}plan mode{RESET}{DIM}"));
        }
        let context = match self.status.window {
            Some(window) => format!("ctx {}/{}", short(self.status.context), short(window)),
            None => format!("ctx {}", short(self.status.context)),
        };
        parts.push(context);
        if let Some(branch) = &self.status.branch {
            parts.push(format!("⎇ {branch}"));
        }
        if self.status.prompt > 0 {
            let cached = self.status.cached * 100 / self.status.prompt.max(1);
            parts.push(format!("{} in ({cached}% cached) · {} out", short(self.status.prompt), short(self.status.completion)));
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

fn fit(line: &str, width: usize) -> String {
    wrap(line, width).into_iter().next().unwrap_or_default()
}

fn diff_line(line: &str) -> String {
    match line.chars().next() {
        Some('+') => format!("    {GREEN}{line}{RESET}"),
        Some('-') => format!("    {RED}{line}{RESET}"),
        _ => format!("    {DIM}{line}{RESET}"),
    }
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

const COMMANDS: &[(&str, &str)] = &[
    ("help", "this list"),
    ("plan", "toggle plan mode (also Shift+Tab)"),
    ("undo", "put files back as they were before the last turn"),
    ("compact", "summarize the conversation to free context; add a focus if you like"),
    ("clear", "start a new session"),
    ("resume", "continue an earlier session in this folder"),
    ("model", "switch model; /model <alias> or pick from a list"),
    ("reload", "re-read AGENTS.md files and skills into the system prompt"),
    ("usage", "tokens and requests this session"),
    ("exit", "quit (also Ctrl+D)"),
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
