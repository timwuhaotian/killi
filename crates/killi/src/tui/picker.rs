use super::*;

/// The /web-search-config list: each service behind `web_search`, what it needs, and a mark
/// on the one in use.
pub(super) fn search_options(current: &crate::config::SearchConfig) -> Vec<(String, String, PickerChoice)> {
    let in_use = current.provider.as_deref().unwrap_or("brave");
    let option = |name: &str, what: &str| {
        let mark = if name == in_use { " · in use" } else { "" };
        (name.to_string(), format!("{what}{mark}"), PickerChoice::Search(name.to_string()))
    };
    vec![
        option("brave", "an API key, or the public results page"),
        option("tavily", "an API key"),
        option("searxng", "your own instance: a URL with the json format"),
    ]
}

pub(super) struct Picker {
    pub(super) title: String,
    /// Each option's name, a note shown beside it, and what choosing it does.
    pub(super) options: Vec<(String, String, PickerChoice)>,
    /// What has been typed to narrow the options, matched against their names.
    pub(super) query: String,
    /// Indexes into `options` of those matching `query`, best first.
    pub(super) shown: Vec<usize>,
    /// Position in `shown`.
    pub(super) selected: usize,
}

/// The /web-search-config flow under way: the service picked (none while its list is open),
/// whether killi starts a local SearXNG with Docker or the user points at their own, the
/// instance URL once answered, and the `[search]` the flow started from, for prefilling.
pub(super) struct SearchFlow {
    pub(super) provider: Option<String>,
    /// `None` while the Docker offer waits; `Some(true)` while its start runs.
    pub(super) docker: Option<bool>,
    pub(super) url: Option<String>,
    pub(super) current: crate::config::SearchConfig,
}
impl Picker {
    pub(super) fn new(title: String, options: Vec<(String, String, PickerChoice)>, selected: usize) -> Picker {
        let shown = (0..options.len()).collect();
        Picker { title, options, query: String::new(), shown, selected }
    }

    /// Narrows the options to those fuzzy-matching `query` and selects the best.
    pub(super) fn filter(&mut self, query: String) {
        let all: Vec<usize> = (0..self.options.len()).collect();
        self.shown = menu::fuzzy_filter(&all, &query, |&i| &self.options[i].0);
        self.query = query;
        self.selected = 0;
    }

    pub(super) fn choice(&self) -> Option<&PickerChoice> {
        self.shown.get(self.selected).map(|&i| &self.options[i].2)
    }
}

#[derive(Clone)]
pub(super) enum PickerChoice {
    Session(PathBuf),
    Model(String),
    BackupModel(String),
    Effort(String),
    Mcp(String),
    Prune(usize),
    Search(String),
}

/// The counts `/prune` offers to keep, from fewest to most, each removing something.
pub(super) fn prune_choices(others: usize) -> Vec<usize> {
    [5, 10, 20, 50, 100, 200].into_iter().filter(|&keep| keep < others).collect()
}

impl App {
    pub(super) fn on_key_picker(&mut self, key: KeyEvent) -> Idle {
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
                    if let PickerChoice::Session(path) = &choice
                        && crate::session::is_locked(path)
                    {
                        self.notice("that session is still running; continue it there, or start /new");
                        return Idle::Continue;
                    }
                    self.picker = None;
                    return match choice {
                        PickerChoice::Session(path) => Idle::Input(Input::Resume(path)),
                        PickerChoice::Model(alias) => Idle::Input(Input::Model(alias)),
                        PickerChoice::BackupModel(alias) => Idle::Input(Input::BackupModel(alias)),
                        PickerChoice::Effort(effort) => Idle::Input(Input::Effort(effort)),
                        PickerChoice::Mcp(name) => Idle::Input(Input::Mcp(name, true)),
                        PickerChoice::Prune(keep) => Idle::Input(Input::Prune(keep)),
                        PickerChoice::Search(provider) => {
                            self.search_pick(&provider);
                            Idle::Continue
                        }
                    };
                }
            }
            _ => {}
        }
        Idle::Continue
    }

    /// Keys for the slash and `@` menus: ↑↓ choose; Tab or Enter runs a command at the start of
    /// the prompt, completes a skill, a later command or a path and fills in an argument; Esc
    /// closes it (the `/` or `@` is then plain text). None lets the key through.
    pub(super) fn on_key_menu(&mut self, key: KeyEvent, menu: &Suggestions) -> Option<Idle> {
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
        let with = match menu.fill {
            Fill::Command => format!("/{value} "),
            Fill::Argument => value.clone(),
            Fill::File if value.ends_with('/') => format!("@{}", mention::escape(value)),
            Fill::File => format!("@{} ", mention::escape(value)),
        };
        self.editor.replace_before(menu.start, &with);
    }

    /// The slash or `@` menu for the prompt as typed. While the agent works the slash menu
    /// offers only the commands that run then plus every skill (its prompt queues for the
    /// agent's next step), typed at the start of the prompt, and /effort's choices; files are
    /// offered as ever.
    pub(super) fn suggestions(&mut self) -> Option<Suggestions> {
        if self.picker.is_some() || self.asking.is_some() || self.editor.browsing() {
            return None;
        }
        if self.menu_closed.as_deref() == Some(self.editor.text()) {
            return None;
        }
        if self.files.is_some() {
            let before = self.editor.before_cursor().to_string();
            if let Some((cached, listed, menu)) = &self.file_menu_cache
                && *cached == before
                && *listed == self.files_gen
            {
                return menu.clone();
            }
            let menu = self.files.as_ref().and_then(|files| menu::files(&before, files));
            self.file_menu_cache = Some((before, self.files_gen, menu.clone()));
            if let Some(menu) = menu {
                return Some(menu);
            }
        }
        if self.busy.is_some() {
            let entries: Vec<Entry> = self.entries.iter().filter(|e| BUSY_COMMANDS.contains(&e.name.as_str()) || !is_command(&e.name)).cloned().collect();
            return menu::suggest(self.editor.before_cursor(), &entries, |command| match command {
                "effort" | "thinking" => self.effort_choices.clone(),
                _ => Vec::new(),
            })
            .filter(|menu| menu.fill != Fill::Command || menu.start == 0);
        }
        menu::suggest(self.editor.before_cursor(), &self.entries, |command| match command {
            "model" => self.model_choices.clone(),
            "effort" | "thinking" => self.effort_choices.clone(),
            _ => Vec::new(),
        })
    }

    /// Starts the menu's selection over when the text changes: on the first item, or on the
    /// current model or effort when it offers those.
    pub(super) fn sync_menu(&mut self) {
        if self.editor.text() == self.menu_text {
            return;
        }
        self.menu_text = self.editor.text().to_string();
        let (model, thinking) = (self.status.model.clone(), self.status.thinking.clone());
        let current = |item: &Item| item.value == model || item.value == thinking;
        self.menu_selected = self.suggestions().filter(|m| m.fill == Fill::Argument).and_then(|m| m.items.iter().position(current)).unwrap_or(0);
    }

    /// Reads the workspace's paths when the cursor is in an `@` word and forgets them when it
    /// isn't, so the list is as fresh as the `@` and costs nothing between them.
    pub(super) fn sync_files(&mut self) {
        let before = self.editor.before_cursor();
        let word = before.rsplit(char::is_whitespace).next().unwrap_or(before);
        match (word.starts_with('@'), self.files.is_some()) {
            (true, false) => {
                self.files = Some(mention::list(&self.cwd));
                self.files_gen += 1;
            }
            (false, true) => {
                self.files = None;
                self.files_gen += 1;
            }
            _ => {}
        }
    }

    pub(super) fn open_session_picker(&mut self, cwd: &Path) {
        let sessions = crate::session::list(cwd);
        if sessions.is_empty() {
            self.notice("No earlier sessions in this folder.");
            return;
        }
        let running: std::collections::HashMap<PathBuf, u32> = crate::live::list().into_iter().map(|e| (e.log, e.pid)).collect();
        let options = sessions
            .into_iter()
            .take(20)
            .map(|s| {
                let age = s.modified.elapsed().map(ago).unwrap_or_default();
                let prompt = s.first_prompt.split_whitespace().collect::<Vec<_>>().join(" ");
                let counts = format!("{} prompt{}", s.prompts, if s.prompts == 1 { "" } else { "s" });
                if crate::session::is_locked(&s.path) {
                    let state = match running.get(&s.path) {
                        Some(pid) => format!("running (pid {pid})"),
                        None => "running".to_string(),
                    };
                    (format!("{age:>8}  {}", crate::llm::truncate(&prompt, 70)), format!("{state} · {counts}"), PickerChoice::Session(s.path))
                } else {
                    (format!("{age:>8}  {}", crate::llm::truncate(&prompt, 70)), counts, PickerChoice::Session(s.path))
                }
            })
            .collect();
        self.picker = Some(Picker::new("Resume a session".into(), options, 0));
    }

    /// Offers how many of the newest sessions to keep, each choice saying how many it removes.
    pub(super) fn open_prune_picker(&mut self, current: &Path) {
        let others = crate::session::list_all().iter().filter(|(path, _)| path != current).count();
        let options: Vec<_> = prune_choices(others)
            .into_iter()
            .map(|keep| (format!("keep the newest {keep}"), format!("removes {} of {others}", others - keep), PickerChoice::Prune(keep)))
            .collect();
        if options.is_empty() {
            return self.notice(&format!("Only {} on this machine; nothing worth pruning.", crate::tools::count(others, "other session")));
        }
        self.picker = Some(Picker::new("Prune sessions · this one stays".into(), options, 0));
    }

    pub(super) fn open_model_picker(&mut self, config: &Config, backup: bool) {
        let chosen = if backup { self.status.backup.clone().unwrap_or_default() } else { self.status.model.clone() };
        let options: Vec<_> = config
            .models
            .iter()
            .map(|(alias, model)| {
                let current = if *alias == chosen { " · current" } else { "" };
                let pick = if backup {
                    PickerChoice::BackupModel(alias.clone())
                } else {
                    PickerChoice::Model(alias.clone())
                };
                (alias.clone(), format!("{}{current}", model.provider), pick)
            })
            .collect();
        let selected = options.iter().position(|(_, _, c)| matches!(c, PickerChoice::Model(a) | PickerChoice::BackupModel(a) if *a == chosen)).unwrap_or(0);
        let title = if backup { "Backup model, for every session" } else { "Switch model" }.to_string();
        self.picker = Some(Picker::new(title, options, selected));
    }

    pub(super) fn open_mcp_picker(&mut self, mcp: &Mcp, selected: Option<&str>) {
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

    /// /web-search-config: pick what `web_search` asks, then answer for what that service
    /// needs; the pick is written to the config and applies at once.
    pub(super) fn open_search_picker(&mut self, config: &Config) {
        let in_use = config.search.provider.as_deref().unwrap_or("brave");
        let selected = ["brave", "tavily", "searxng"].iter().position(|name| *name == in_use).unwrap_or(0);
        self.search = Some(SearchFlow { provider: None, docker: None, url: None, current: config.search.clone() });
        self.picker = Some(Picker::new("Web search service".into(), search_options(&config.search), selected));
    }

    /// A service picked from that list or named by `/web-search-config <service>`: ask for
    /// what it needs, prefilled with what the config already has for it.
    pub(super) fn search_pick(&mut self, provider: &str) {
        let Some(mut flow) = self.search.take() else { return };
        flow.provider = Some(provider.to_string());
        let current = flow.current.clone();
        let same = current.provider.as_deref().unwrap_or("brave") == provider;
        let key = same.then(|| current.api_key.clone()).flatten();
        let fresh = provider == "searxng" && !same;
        if provider == "searxng" && !fresh {
            // An instance is already configured: edit its URL and key, not the offer.
            flow.docker = Some(false);
        }
        self.search = Some(flow);
        match provider {
            "searxng" => {
                self.search_note();
                if fresh {
                    self.search_question(
                        "SearXNG is self-hosted. Start a local one with Docker, or use an instance you run?",
                        vec!["start one with Docker".into(), "use my own instance".into()],
                    );
                } else {
                    let url = current.url.clone().unwrap_or_else(|| "http://localhost:8888".into());
                    self.search_ask("SearXNG instance URL:", Some(&url), false);
                }
            }
            "tavily" => self.search_ask("Tavily API key:", key.as_deref(), true),
            _ => self.search_ask("Brave API key (Enter for none: killi then reads Brave's public page):", key.as_deref(), true),
        }
    }

    /// What picking SearXNG asks of the user: it is self-hosted, so say what killi calls,
    /// where its settings live, and that Docker is one answer.
    fn search_note(&mut self) {
        self.begin(Kind::Note);
        self.emit(Line::Words(
            format!("{DIM}killi asks {{url}}/search?q=…&format=json. With Docker it starts an instance on localhost:8888 with the json format on; without it, run one yourself and add json to search.formats in its settings.yml.{RESET}"),
            2,
        ));
    }

    /// One question of the flow, shown as the ask tool's questions are, with the answer typed
    /// into the prompt. Nobody waits on the reply: `answer` routes it back into the flow.
    pub(super) fn search_ask(&mut self, text: &str, prefill: Option<&str>, secret: bool) {
        self.search_question(text, Vec::new());
        if secret {
            self.asking.as_mut().expect("a question is open").secret = true;
        }
        if let Some(fill) = prefill.filter(|fill| !fill.is_empty()) {
            self.editor.insert(fill);
        }
    }

    /// One choice of the flow: a question whose options are picked, not typed.
    fn search_question(&mut self, text: &str, options: Vec<String>) {
        self.begin(Kind::Note);
        self.emit(Line::Words(format!("{BOLD}{FIN}?{RESET} {BOLD}{text}{RESET}"), 2));
        let (reply, _) = tokio::sync::oneshot::channel::<String>();
        self.asking = Some(Asking { question: Question { text: text.to_string(), options, plan: false, reply }, selected: 0, internal: true, secret: false });
    }

    /// One answer of the flow: the Docker offer, the instance URL (validated), or the key;
    /// once all are in, the pick goes to the config file and this session as `Input::Search`.
    pub(super) fn search_answer(&mut self, answer: &str, secret: bool) {
        let Some(mut flow) = self.search.take() else { return };
        let answer = answer.trim();
        let provider = flow.provider.clone().unwrap_or_default();
        if provider == "searxng" && flow.url.is_none() && flow.docker.is_none() {
            let echo = format!("  {FIN}↳{RESET} {answer}");
            if answer.contains("Docker") {
                flow.docker = Some(true);
                self.search = Some(flow);
                self.commit(echo);
                self.begin(Kind::Note);
                self.commit(format!("{DIM}Starting SearXNG with Docker; the first run pulls the image, which can take a few minutes.{RESET}"));
                self.searxng = Some(Box::pin(crate::tools::start_searxng()));
                return;
            }
            flow.docker = Some(false);
            self.commit(echo);
            self.search = Some(flow);
            return self.search_ask("SearXNG instance URL:", Some("http://localhost:8888"), false);
        }
        if provider == "searxng" && flow.url.is_none() {
            if !(answer.starts_with("http://") || answer.starts_with("https://")) {
                self.error("The instance is its URL, such as http://localhost:8888.");
                self.search = Some(flow);
                return self.search_ask("SearXNG instance URL:", Some(answer), false);
            }
            self.commit(format!("  {FIN}↳{RESET} {answer}"));
            flow.url = Some(answer.to_string());
            let key = (flow.current.provider.as_deref().unwrap_or("brave") == "searxng").then(|| flow.current.api_key.clone()).flatten();
            self.search = Some(flow);
            return self.search_ask("SearXNG API key, if the instance asks for one (Enter for none):", key.as_deref(), true);
        }
        let echo = if answer.is_empty() { "(none)" } else if secret { "(set)" } else { answer };
        self.commit(format!("  {FIN}↳{RESET} {}", text::sanitize(echo)));
        if answer.is_empty() && provider == "tavily" {
            return self.error("web_search with Tavily needs an API key; nothing was changed.");
        }
        self.next_input = Some(Input::Search(provider, flow.url, (!answer.is_empty()).then(|| answer.to_string())));
    }

    pub(super) fn open_effort_picker(&mut self, config: &Config) {
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
}
