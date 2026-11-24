//! What a one-shot run (`killi -p`) and the commands that print and exit write, through
//! writes that never panic: a reader that went away (`killi -p … | head`) ends the output,
//! not the process.

use std::collections::HashMap;
use std::fmt::Display;
use std::io::{self, Write};
use std::rc::Rc;

use tokio::sync::Notify;

use crate::agent::Event;
use crate::llm;

/// stdout can't be written any more, most often because its reader exited. A command that
/// ends on it exits 141, as one killed by SIGPIPE would, and says nothing more.
#[derive(Debug)]
pub struct Closed;

impl Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stdout was closed")
    }
}

impl std::error::Error for Closed {}

/// The exit code of a command whose stdout was closed under it.
pub const CLOSED_EXIT: u8 = 141;

/// Writes `text` to stdout and flushes it.
pub fn out(text: &str) -> Result<(), Closed> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|_| Closed)
}

/// Writes `text` and a newline to stdout, for a command to end with `?` once its reader is
/// gone.
pub fn say(text: impl Display) -> anyhow::Result<()> {
    Ok(out(&format!("{text}\n"))?)
}

/// Writes `text` to stderr; a stderr that is gone is ignored, since nothing could report it.
pub fn err(text: &str) {
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(text.as_bytes());
    let _ = stderr.flush();
}

#[derive(Clone, Copy, PartialEq)]
enum Last {
    Nothing,
    Text,
    Tool,
}

/// Where the text stood when an attempt's own began, for a discarded attempt to put back.
#[derive(Clone, Copy)]
struct Mark {
    last: Last,
    line_open: bool,
    held_newlines: usize,
}

/// Prints a one-shot run as compact lines: the model's text on stdout, one line per tool call
/// on stderr, a blank line between the two. Colors only when stderr is a terminal.
pub struct Printer {
    cwd: String,
    last: Last,
    /// Where the text stood when the current attempt's began; none while the attempt has
    /// streamed no text, or once what it streamed is settled.
    mark: Option<Mark>,
    /// The current attempt's text, held back so a retried or failed attempt never reaches
    /// stdout: only the final reply is printed, when the attempt ends or the agent records it.
    attempt: String,
    /// The model's text is mid-line in the attempt.
    line_open: bool,
    /// Newlines at the end of the text so far, printed only if more text follows.
    held_newlines: usize,
    /// The call whose line on stderr waits for its end mark.
    tool_open: Option<String>,
    /// What each call in flight is, for an end that comes after its line was closed.
    tools: HashMap<String, String>,
    stdout: Box<dyn Write>,
    stderr: Box<dyn Write>,
    /// stdout failed once; nothing more is written to it.
    closed: bool,
    /// Told when stdout closes, so the run can end.
    halt: Option<Rc<Notify>>,
    pub dim: &'static str,
    accent: &'static str,
    pub red: &'static str,
    pub reset: &'static str,
}

impl Printer {
    /// Writes to the process's stdout and stderr, and tells `halt` if stdout closes.
    pub fn new(cwd: &std::path::Path, halt: Rc<Notify>) -> Self {
        use std::io::IsTerminal;
        let color = io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        let mut printer = Self::to(cwd, Box::new(io::stdout()), Box::new(io::stderr()), color);
        printer.halt_on(halt);
        printer
    }

    /// Tells `halt` when stdout closes.
    pub fn halt_on(&mut self, halt: Rc<Notify>) {
        self.halt = Some(halt);
    }

    /// Writes to the given streams.
    pub fn to(
        cwd: &std::path::Path,
        stdout: Box<dyn Write>,
        stderr: Box<dyn Write>,
        color: bool,
    ) -> Self {
        let pick = |code: &'static str| if color { code } else { "" };
        Self {
            cwd: cwd.display().to_string(),
            last: Last::Nothing,
            mark: None,
            attempt: String::new(),
            line_open: false,
            held_newlines: 0,
            tool_open: None,
            tools: HashMap::new(),
            stdout,
            stderr,
            closed: false,
            halt: None,
            dim: pick("\x1b[2m"),
            accent: pick("\x1b[34m"),
            red: pick("\x1b[31m"),
            reset: pick("\x1b[0m"),
        }
    }

    /// Whether stdout failed: its reader is gone.
    pub fn closed(&self) -> bool {
        self.closed
    }

    pub fn event(&mut self, event: Event) {
        match event {
            Event::Text(text) => {
                let text = if self.last == Last::Text {
                    text.as_str()
                } else {
                    text.trim_start()
                };
                let body = text.trim_end_matches('\n');
                if body.is_empty() && self.last != Last::Text {
                    return;
                }
                self.mark.get_or_insert(Mark {
                    last: self.last,
                    line_open: self.line_open,
                    held_newlines: self.held_newlines,
                });
                if body.is_empty() {
                    self.held_newlines += text.len();
                    return;
                }
                if self.last == Last::Tool {
                    self.attempt.push('\n');
                }
                self.attempt.push_str(&"\n".repeat(self.held_newlines));
                self.attempt.push_str(body);
                self.held_newlines = text.len() - body.len();
                self.line_open = true;
                self.last = Last::Text;
            }
            Event::Retry {
                attempt,
                wait,
                reason,
            } => {
                self.close_tool_line();
                let dropped = self.drop_attempt();
                self.note(&format!(
                    "{}retry {attempt} in {:.1}s: {}{}",
                    self.dim,
                    wait.as_secs_f32(),
                    llm::truncate(&reason, 300),
                    self.reset
                ));
                if dropped {
                    self.note(&format!(
                        "{}the partial reply was discarded; retrying …{}",
                        self.dim, self.reset
                    ));
                }
            }
            Event::Discard { reason } => {
                self.close_tool_line();
                if self.drop_attempt() {
                    self.note(&format!(
                        "{}the partial reply was discarded; {reason}{}",
                        self.dim, self.reset
                    ));
                }
            }
            // The recorded part is the reply's for good: a continuation's retry keeps it.
            Event::Settled => self.flush_attempt(),
            Event::ToolStart { id, summary } => {
                if self.last == Last::Text {
                    self.end_text();
                    self.out("\n");
                }
                self.close_tool_line();
                let summary = summary
                    .replace(&format!("{}/", self.cwd), "")
                    .replace(&self.cwd, ".");
                let line = format!("{}●{} {summary}", self.accent, self.reset);
                self.write_err(&line);
                self.tools.insert(id.clone(), summary);
                self.tool_open = Some(id);
                self.last = Last::Tool;
            }
            Event::ToolEnd { id, is_error, .. } => {
                let summary = self.tools.remove(&id).unwrap_or_default();
                if self.tool_open.as_ref() == Some(&id) {
                    self.tool_open = None;
                    let mark = if is_error {
                        format!(" {}✗{}", self.red, self.reset)
                    } else {
                        String::new()
                    };
                    self.write_err(&format!("{mark}\n"));
                } else if is_error {
                    // Another call's line came after this one's: the mark gets a line of its own.
                    self.close_tool_line();
                    self.note(&format!("{}✗{} {summary}", self.red, self.reset));
                }
            }
            Event::Notice(text) => {
                self.close_tool_line();
                self.end_text();
                self.note(&format!("{}{text}{}", self.dim, self.reset));
            }
            Event::PairTurn {
                role,
                model,
                effort,
                what,
            } => {
                self.close_tool_line();
                self.end_text();
                self.note(&format!("→ {} · {what} — {model} ({effort})", role.label()));
            }
            Event::Sub { label, event, .. } => {
                if matches!(
                    *event,
                    Event::ToolStart { .. }
                        | Event::ToolEnd { is_error: true, .. }
                        | Event::Retry { .. }
                        | Event::Finished { .. }
                ) {
                    self.close_tool_line();
                }
                let line = match *event {
                    Event::ToolStart { summary, .. } => {
                        format!("  {}↳ {label}: {summary}{}", self.dim, self.reset)
                    }
                    Event::ToolEnd {
                        is_error: true,
                        brief,
                        ..
                    } => format!("  {}↳ {label}: ✗ {brief}{}", self.red, self.reset),
                    Event::Retry {
                        attempt,
                        wait,
                        reason,
                    } => format!(
                        "  {}↳ {label}: retry {attempt} in {:.1}s: {}{}",
                        self.dim,
                        wait.as_secs_f32(),
                        llm::truncate(&reason, 300),
                        self.reset
                    ),
                    Event::Finished { is_error } => format!(
                        "  {}↳ {label}: {}{}",
                        self.dim,
                        if is_error { "failed" } else { "done" },
                        self.reset
                    ),
                    _ => return,
                };
                self.note(&line);
            }
            Event::Reasoning(_)
            | Event::ToolDelta(_)
            | Event::Usage(_)
            | Event::Todos(_)
            | Event::Ask(_)
            | Event::Swarm { .. }
            | Event::Started
            | Event::Finished { .. }
            | Event::Detached => {}
        }
    }

    /// Drops the attempt's held text, putting back what came before it; whether there was any.
    fn drop_attempt(&mut self) -> bool {
        let Some(mark) = self.mark.take() else {
            return false;
        };
        self.attempt.clear();
        self.last = mark.last;
        self.line_open = mark.line_open;
        self.held_newlines = mark.held_newlines;
        true
    }

    /// Ends an open tool line, whose end mark won't follow on it.
    fn close_tool_line(&mut self) {
        if self.tool_open.take().is_some() {
            self.write_err("\n");
        }
    }

    /// Prints the attempt's text so far; a retry no longer takes it back.
    fn flush_attempt(&mut self) {
        self.mark = None;
        if !self.attempt.is_empty() {
            let text = std::mem::take(&mut self.attempt);
            self.out(&text);
        }
    }

    /// Ends the model's text with exactly one newline, dropping any it held back.
    fn end_text(&mut self) {
        self.flush_attempt();
        if self.line_open {
            self.out("\n");
            self.line_open = false;
        }
        self.held_newlines = 0;
    }

    /// Ends what is open: the reply's text and a tool line. At the end of the run, also one
    /// cut short.
    pub fn finish(&mut self) {
        self.close_tool_line();
        self.end_text();
    }

    /// A line on stderr.
    pub fn note(&mut self, line: &str) {
        self.write_err(&format!("{line}\n"));
    }

    fn out(&mut self, text: &str) {
        if self.closed {
            return;
        }
        let written = self
            .stdout
            .write_all(text.as_bytes())
            .and_then(|()| self.stdout.flush());
        if written.is_err() {
            self.closed = true;
            if let Some(halt) = &self.halt {
                halt.notify_one();
            }
        }
    }

    fn write_err(&mut self, text: &str) {
        let _ = self.stderr.write_all(text.as_bytes());
        let _ = self.stderr.flush();
    }
}
