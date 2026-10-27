use super::*;

/// What the slash menu searches: the commands, then every skill not named like one.
pub(super) fn entries(session: &Setup) -> Vec<Entry> {
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

pub(super) struct Command {
    pub(super) name: &'static str,
    /// Other names the menu finds it by. `/q`, `/h` and `/?` work too, unlisted.
    pub(super) aliases: &'static [&'static str],
    pub(super) args: &'static str,
    pub(super) about: &'static str,
}

/// What a submitted prompt is: a command when it starts with `/`, else a message to the model.
/// The images pasted into it go along either way, since a skill's request can hold one.
pub(super) fn submission(text: String, images: Vec<Image>) -> Option<Input> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(command) = trimmed.strip_prefix('/') {
        let (name, arg) = command.split_once(char::is_whitespace).unwrap_or((command, ""));
        return Some(Input::Command(name.to_string(), arg.trim().to_string(), images));
    }
    Some(Input::Prompt(text, images))
}

/// Whether `name` is a built-in command or one of its aliases, not a skill.
pub(super) fn is_command(name: &str) -> bool {
    COMMANDS.iter().any(|c| c.name == name || c.aliases.contains(&name))
}

/// Whether choosing the menu's item at `index` sends the prompt: a built-in command at the start
/// does, unless it needs an argument (`/btw <question>`); a skill stays in the prompt so a request
/// can follow it.
pub(super) fn runs_at_once(menu: &Suggestions, index: usize) -> bool {
    let name = menu.items[index].value.as_str();
    let needs_argument = COMMANDS.iter().any(|c| (c.name == name || c.aliases.contains(&name)) && c.args.starts_with('<'));
    menu.fill == Fill::Command && menu.start == 0 && is_command(name) && !needs_argument
}

/// The commands that also work while the agent works: they need nothing the running turn holds,
/// except /effort, which waits for the turn to end (see `App::effort_after_turn`).
pub(super) const BUSY_COMMANDS: &[&str] = &["btw", "plan", "swarm", "later", "sessions", "help", "effort"];

/// The effort `arg` names among those `model` takes, or what to tell the user.
pub(super) fn effort_named(model: &str, efforts: &[&str], arg: &str) -> Result<String, String> {
    let effort = arg.trim().to_lowercase();
    if efforts.contains(&effort.as_str()) {
        Ok(effort)
    } else if effort.is_empty() {
        Err(format!("Say which effort: /effort {}.", efforts.join(" | ")))
    } else {
        Err(format!("{model} has no effort “{effort}”; choose one of {}.", efforts.join(", ")))
    }
}

/// The built-in commands, in the order the menu lists them.
pub(super) const COMMANDS: &[Command] = &[
    Command { name: "model", aliases: &[], args: "[alias]", about: "Switch model" },
    Command { name: "backup-model", aliases: &[], args: "[alias]", about: "Set the model this session falls back to on a rate limit" },
    Command { name: "mcp", aliases: &[], args: "[server]", about: "Turn MCP servers on or off" },
    Command { name: "web-search-config", aliases: &[], args: "", about: "Set what web_search asks: brave, tavily or your own searxng" },
    Command { name: "effort", aliases: &["thinking"], args: "[level]", about: "Switch thinking effort" },
    Command { name: "new", aliases: &["clear"], args: "", about: "Start a fresh session in this folder" },
    Command { name: "resume", aliases: &[], args: "", about: "Continue an earlier session in this folder" },
    Command { name: "btw", aliases: &[], args: "<question>", about: "Ask a side question, also while the agent works; not kept" },
    Command { name: "compact", aliases: &[], args: "[focus]", about: "Summarize the conversation to free context" },
    Command { name: "recap", aliases: &[], args: "", about: "Recap the session in 100 characters; also after each turn that used tools" },
    Command { name: "undo", aliases: &[], args: "", about: "Put files back as they were before the last turn" },
    Command { name: "plan", aliases: &[], args: "", about: "Toggle plan mode (also Shift+Tab)" },
    Command { name: "swarm", aliases: &[], args: "[task|on|off]", about: "Swarm mode: split work across many subagents" },
    Command { name: "usage", aliases: &["cost"], args: "", about: "Tokens and requests this session" },
    Command { name: "session", aliases: &["share"], args: "", about: "Copy this session's id, log path and summary, to paste into another agent" },
    Command { name: "sessions", aliases: &[], args: "", about: "The other killi sessions running on this machine, and what each is doing" },
    Command { name: "later", aliases: &[], args: "[done N]", about: "What the agent noted for later in this folder; done N crosses one off" },
    Command { name: "prune", aliases: &[], args: "[keep]", about: "Delete every session on this machine but the newest few" },
    Command { name: "reload", aliases: &[], args: "", about: "Re-read [prompt], AGENTS.md files and skills" },
    Command { name: "help", aliases: &[], args: "", about: "Commands and keys" },
    Command { name: "exit", aliases: &["quit"], args: "", about: "Quit killi (also Ctrl+D)" },
];

pub(super) const KEYS: &[(&str, &str)] = &[
    ("@", "name a file: the menu searches the workspace, and the file goes to the model with your message"),
    ("Enter", "send; while the agent works, sends your message at its next step"),
    ("Shift+Enter", "new line; also Ctrl+J, or \\ then Enter"),
    ("Esc", "interrupt the agent"),
    ("Ctrl+S", "while the agent works, interrupt it and send your message now"),
    ("Shift+Tab", "plan mode"),
    ("Ctrl+O", "show the last tool output or thought in full"),
    ("Ctrl+V", "paste an image (Cmd+V too in Kil)"),
    ("↑ ↓", "history"),
    ("PageUp PageDown", "scroll the conversation (fullscreen mode, with the wheel)"),
    ("Ctrl+C", "clear the prompt; on an empty one, interrupt the agent, or twice to quit"),
];

impl App {
    /// Runs a command that needs only the screen and this folder, so it also works while the
    /// agent does (see `BUSY_COMMANDS`). False when `name` with `arg` isn't one of those.
    pub(super) fn run_local(&mut self, name: &str, arg: &str) -> bool {
        match (name, arg) {
            ("help" | "h" | "?", _) => self.help(),
            ("btw", "") => self.error("Ask with /btw <question>."),
            ("plan", _) => self.toggle_plan(),
            ("swarm", "") => self.set_swarm(!self.controls.swarm.get()),
            ("swarm", "on") => self.set_swarm(true),
            ("swarm", "off") => self.set_swarm(false),
            ("later", "") => self.show_later(),
            ("sessions", _) => self.show_sessions(),
            ("later", arg) => match arg.strip_prefix("done").and_then(|rest| rest.trim().parse::<usize>().ok()) {
                Some(number) => match crate::later::done(&self.cwd, number) {
                    Ok(item) => self.notice(&format!("Crossed off: {item}")),
                    Err(e) => self.error_report(&e),
                },
                None => self.error("Use /later to see the list, or /later done 3 to cross one off."),
            },
            _ => return false,
        }
        true
    }

    fn help(&mut self) {
        self.begin(Kind::Note);
        let enhanced = ENHANCED.load(Ordering::Relaxed);
        self.emit(Line::block(move |width| help_rows(width, enhanced)));
        self.last_kind = Kind::None;
    }

    /// A /btw answer, with the question it answers; it isn't part of the conversation. It is
    /// printed as one block, at the end of the model's paragraph if one is streaming.
    pub(super) fn show_aside(&mut self, question: &str, answer: Result<(String, Usage)>) {
        self.aside = None;
        let (text, usage) = match answer {
            Ok(answer) => answer,
            Err(e) => return self.error_from("/btw", &e),
        };
        let cached = usage.cached * 100 / usage.prompt.max(1);
        let note = format!("Not kept in the conversation · {} in · {cached}% cached", short(usage.prompt));
        let question = question.to_string();
        let answer = text::sanitize(&text).into_owned();
        self.held_asides.push(Line::block(move |width| aside_rows("btw", &question, &answer, &note, width)));
        self.release_asides(false);
    }

    /// A recap, printed as a block of its own with what asking for it cost.
    pub(super) fn show_recap(&mut self, text: &str, usage: &Usage) {
        let cached = usage.cached * 100 / usage.prompt.max(1);
        let note = format!("{} in · {cached}% cached", short(usage.prompt));
        let text = text.to_string();
        self.held_asides.push(Line::block(move |width| recap_rows(&text, &note, width)));
        self.release_asides(true);
    }

    /// Prints the answered /btw blocks, unless the model is in the middle of a paragraph and
    /// `force` is off: the block would cut it in two.
    pub(super) fn release_asides(&mut self, force: bool) {
        if self.held_asides.is_empty() || (!force && self.last_kind == Kind::Text && !self.held_blank) {
            return;
        }
        // The block ends with its own blank row and the prose goes on as if it weren't there, so
        // the next chunk of text keeps its leading spaces.
        let before = self.last_kind;
        for aside in std::mem::take(&mut self.held_asides) {
            self.begin(Kind::Note);
            self.emit(aside);
            self.commit(String::new());
        }
        self.last_kind = before;
    }

    pub(super) fn usage_report(&mut self, agent: &Agent) {
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
    pub(super) fn share_session(&mut self, agent: &Agent) {
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

    /// Lists what was noted for later in this folder, numbered as `/later done N` counts them.
    fn show_later(&mut self) {
        let cwd = self.cwd.clone();
        let items = crate::later::list(&cwd);
        self.begin(Kind::Note);
        if items.is_empty() {
            return self.notice("Nothing noted for later in this folder. The agent adds to the list with its `later` tool.");
        }
        self.commit(format!("{BOLD}Noted for later{RESET} {DIM}({}) · {}{RESET}", items.len(), tilde(&crate::later::path(&cwd))));
        for (i, item) in items.iter().enumerate() {
            self.commit_wrapped(format!("  {DIM}{}.{RESET} {item}", i + 1));
        }
        self.notice("/later done N crosses one off.");
    }

    pub(super) fn refresh_branch(&mut self) {
        if self.branch_at.is_some_and(|at| at.elapsed() < BRANCH_TTL) {
            return;
        }
        self.branch_at = Some(Instant::now());
        self.status.branch = Self::branch_of(&self.cwd);
    }

    /// The branch for the status line, read where blocking is allowed: on this runtime two
    /// git spawns would freeze keys, ticks and peer answers, so the wait happens off it.
    pub(super) async fn refresh_branch_async(&mut self) {
        if self.branch_at.is_some_and(|at| at.elapsed() < BRANCH_TTL) {
            return;
        }
        self.branch_at = Some(Instant::now());
        let cwd = self.cwd.clone();
        if let Ok(branch) = tokio::task::spawn_blocking(move || Self::branch_of(&cwd)).await {
            self.status.branch = branch;
        }
    }

    /// The branch `cwd` is on, with a mark when it is dirty; None outside a repository.
    pub(super) fn branch_of(cwd: &Path) -> Option<String> {
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        };
        git(&["rev-parse", "--abbrev-ref", "HEAD"]).map(|branch| {
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
            if dirty { format!("{branch}*") } else { branch }
        })
    }
}
