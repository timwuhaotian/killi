use super::*;

impl App {
    pub(super) fn new(agent: &Agent, controls: Controls, cwd: &Path, skills: crate::skills::Skills) -> Self {
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
            preview_cache: None,
            stream: text::Sanitizer::default(),
            reasoning: text::Sanitizer::default(),
            held_blank: false,
            last_blank: false,
            last_kind: Kind::None,
            thinking: None,
            running: Vec::new(),
            swarm: None,
            todos: Vec::new(),
            asking: None,
            ask_past: Vec::new(),
            ask_view: None,
            ask_corrections: Vec::new(),
            picker: None,
            search: None,
            searxng: None,
            next_input: None,
            effort_later: None,
            entries: Vec::new(),
            skills,
            model_choices: Vec::new(),
            effort_choices: Vec::new(),
            menu_selected: 0,
            menu_text: String::new(),
            menu_closed: None,
            files: None,
            files_gen: 0,
            file_menu_cache: None,
            busy: None,
            details: Vec::new(),
            detail_back: 0,
            ctrl_o_taught: false,
            status: Status {
                model: agent.model_alias().to_string(),
                thinking: agent.thinking().label().to_string(),
                window: agent.context_window(),
                context: agent.stats.context,
                ..Status::default()
            },
            controls,
            spinner: 0,
            retry: None,
            snapshotting: false,
            snapshot_seq: 0,
            exit_armed: None,
            queued: Vec::new(),
            send_now: false,
            aside_asked: None,
            later_noted: false,
            aside: None,
            held_asides: Vec::new(),
            auto_recap: true,
            recap: None,
            last_end: None,
            cwd: cwd.to_path_buf(),
            notify_after: Duration::from_secs(10),
            sees_images: agent.sees_images(),
            since_banner: None,
            content_rows: usize::from(rows).max(5),
            fullscreen: false,
            history: History::new(usize::from(columns).max(20)),
            view_rows: usize::from(rows).max(5),
            selection: None,
            drag: None,
            pressed: None,
            banner_resumed: false,
            branch_at: None,
            live: None,
            peers: None,
            peer_asks: FuturesUnordered::new(),
            peer_asking: Vec::new(),
            peer_ask_id: 0,
            peer_tells: 0,
            undo_warned: false,
        };
        app.refresh_branch();
        app
    }

    /// Asks the terminal which row the cursor is on, to know how much of the screen is free.
    pub(super) fn locate(&mut self) {
        if let Ok((_, row)) = crossterm::cursor::position() {
            self.content_rows = usize::from(row).min(self.height);
        }
    }

    pub(super) fn banner(&mut self, agent: &Agent, session: &Setup, snapshots: &Snapshots, resumed: bool) {
        self.banner_resumed = resumed;
        let cwd = tilde(&self.cwd);
        let thinking = match thinking_parts(agent.thinking().label()) {
            Some((word, value)) => format!(" {DIM}· {word}{RESET} {EFFORT}{value}{RESET}"),
            None => String::new(),
        };
        let overview = session.overview();
        let model = format!("{}{BOLD}{}{RESET}{thinking}", label("model"), agent.model_alias());
        let setup = overview_rows(&overview);
        self.since_banner = self.pending.is_empty().then(|| SinceBanner { resumed, ..SinceBanner::default() });
        self.history.banner_starts();
        self.commit(String::new());
        let tall = self.height >= banner::COMPACT_ROWS;
        self.emit(Line::block(move |width| {
            let version = env!("CARGO_PKG_VERSION");
            if banner::compact(width, tall) {
                let cwd = shorten_path(&cwd, width.saturating_sub(7).max(20));
                let mut rows = vec![banner::wordmark(version), model.clone(), format!("{}{cwd}", label("folder"))];
                rows.extend(setup.iter().cloned());
                return rows.iter().map(|row| fit(row, width)).collect();
            }
            let room = width.saturating_sub(41).max(20);
            let cwd = shorten_path(&cwd, room);
            let mut side = vec![
                format!("{BOLD}killi{RESET} {DIM}{version}{RESET}"),
                format!("{DIM}a coding agent for the terminal{RESET}"),
                String::new(),
                model.clone(),
                format!("{}{cwd}", label("folder")),
            ];
            side.extend(setup.iter().cloned());
            banner::render(width, &side)
        }));
        self.commit(String::new());
        for problem in &overview.problems {
            self.commit(format!("{WARN}!{RESET} {problem}"));
        }
        if let Some(why) = &snapshots.disabled {
            self.commit(format!("{DIM}{why}{RESET}"));
        }
        if let Some(line) = self.peers_line() {
            self.emit(Line::block(move |width| wrap_words(&line, width, 0)));
        }
        if resumed {
            let prompts = agent.user_prompts();
            self.commit(format!("{DIM}Resumed a session with {} prompt{}; the last was “{}”.{RESET}", prompts.len(), if prompts.len() == 1 { "" } else { "s" }, prompts.last().map_or("", |s| s.lines().next().unwrap_or(""))));
        }
        self.emit(Line::block(|width| wrap_items(&key_hints(ENHANCED.load(Ordering::Relaxed)), width)));
        self.commit(String::new());
        self.last_kind = Kind::None;
        self.history.banner_ends();
        if let Some(since) = &mut self.since_banner {
            since.banner = self.pending.len();
        }
    }

    /// Draws the banner again with the model, effort and setup now in use, when its top row is
    /// still on screen; everything printed below it is printed again as it was.
    pub(super) fn redraw_banner(&mut self, agent: &Agent, session: &Setup, snapshots: &Snapshots) {
        if self.fullscreen {
            let Some(rest) = self.history.cut_banner() else { return };
            let (last_kind, last_blank) = (self.last_kind, self.last_blank);
            self.last_blank = false;
            self.banner(agent, session, snapshots, self.banner_resumed);
            self.history.restore(rest);
            (self.last_kind, self.last_blank) = (last_kind, last_blank);
            return;
        }
        let Some(since) = self.since_banner.take() else { return };
        if since.rows + self.shown.len() > self.height {
            return;
        }
        let _ = queue!(self.out, MoveUp((self.cursor_row + since.rows) as u16), MoveToColumn(0), Clear(ClearType::FromCursorDown));
        self.cursor_row = 0;
        self.shown.clear();
        self.content_rows = self.content_rows.saturating_sub(since.rows);
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

    /// Shows the model and effort the agent runs on in the status line and the slash menu.
    pub(super) fn show_model(&mut self, agent: &Agent, config: &Config) {
        self.sees_images = agent.sees_images();
        self.status.model = agent.model_alias().to_string();
        self.status.backup = agent.backup_alias().map(String::from);
        self.status.thinking = agent.thinking().label().to_string();
        self.status.window = agent.context_window();
        self.refresh_choices(config);
    }

    /// Remembers the model just picked for the sessions started after this one; its effort
    /// is the model's `default_effort` in the config, which `/effort` keeps up to date.
    pub(super) fn save_choice(&mut self, agent: &Agent) {
        let choice = Choice { model: Some(agent.model_alias().to_string()), effort: None };
        if let Err(e) = crate::session::save_choice(&choice) {
            self.error_report(&e);
        }
    }

    pub(super) fn refresh_choices(&mut self, config: &Config) {
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

    /// Clears the screen and its scrollback, dropping lines not yet shown.
    pub(super) fn wipe_screen(&mut self) {
        self.pending.clear();
        self.last_blank = false;
        self.last_kind = Kind::None;
        self.ctrl_o_taught = false;
        if self.fullscreen {
            self.history.clear();
            self.shown.clear();
            return;
        }
        let _ = execute!(self.out, Clear(ClearType::All), Clear(ClearType::Purge), MoveTo(0, 0));
        self.cursor_row = 0;
        self.shown.clear();
        self.since_banner = None;
        self.content_rows = 0;
    }

    pub(super) fn resize(&mut self, columns: u16, rows: u16) {
        self.width = usize::from(columns).max(20);
        self.markdown.width = self.width;
        self.height = usize::from(rows).max(5);
        if self.fullscreen {
            self.history.set_width(self.width);
            self.shown.clear();
            return;
        }
        if self.cursor_row > 0 {
            let _ = queue!(self.out, MoveUp(self.cursor_row as u16));
        }
        let _ = queue!(self.out, MoveToColumn(0), Clear(ClearType::FromCursorDown));
        self.cursor_row = 0;
        self.shown.clear();
        self.since_banner = None;
        self.content_rows = self.height;
    }

    /// Scrolls the conversation by `rows`, up when negative; fullscreen mode only.
    pub(super) fn scroll(&mut self, rows: isize) {
        if rows < 0 {
            self.history.scroll_up(rows.unsigned_abs(), self.view_rows);
        } else {
            self.history.scroll_down(rows.unsigned_abs());
        }
    }

    /// Leaves the conversation on screen and puts the shell prompt below it.
    pub(super) fn finish_screen(&mut self) -> Result<()> {
        if self.fullscreen {
            leave_fullscreen(&mut self.out);
            for row in self.history.all_rows() {
                write!(self.out, "{}{RESET}\r\n", text::plain(row))?;
            }
            self.out.flush()?;
            return Ok(());
        }
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
