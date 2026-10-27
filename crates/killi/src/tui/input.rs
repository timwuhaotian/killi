use super::*;

/// Shift+Enter, Alt+Enter and Ctrl+Enter insert a line break instead of sending.
pub(super) fn newline_key(key: &KeyEvent) -> bool {
    key.code == KeyCode::Enter && key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL)
}

impl App {
    pub(super) async fn read_input(&mut self, keys: &mut EventStream) -> Result<Option<Input>> {
        if let Some(effort) = self.effort_later.take() {
            return Ok(Some(Input::Effort(effort)));
        }
        if let Some(input) = self.next_input.take() {
            self.recap = None;
            return Ok(Some(input));
        }
        if std::mem::take(&mut self.send_now)
            && let Some((text, images)) = self.send_now_prompt()
        {
            self.recap = None;
            return Ok(Some(Input::Prompt(text, images)));
        }
        loop {
            let event = tokio::select! {
                event = keys.next() => event,
                () = resumed() => {
                    self.draw()?;
                    continue;
                }
                Some(incoming) = next_incoming(&mut self.peers) => {
                    self.on_peer(incoming);
                    self.draw()?;
                    continue;
                }
                Some(answered) = self.peer_asks.next(), if !self.peer_asks.is_empty() => {
                    self.on_peer_answer(answered);
                    self.draw()?;
                    continue;
                }
                // Keeps the clock on a question from another session moving while idle.
                () = tokio::time::sleep(Duration::from_secs(1)), if !self.peer_asking.is_empty() => {
                    self.spinner += 1;
                    self.draw()?;
                    continue;
                }
                done = async { self.recap.as_mut().expect("a recap is pending").as_mut().await }, if self.recap.is_some() => {
                    self.recap = None;
                    if let Ok((text, usage)) = done {
                        self.show_recap(&text, &usage);
                    }
                    self.draw()?;
                    continue;
                }
            };
            let Some(event) = event else { return Ok(None) };
            let event = event?;
            let result = self.on_key_idle(event);
            self.sync_menu();
            self.draw()?;
            match result {
                Idle::Continue => {}
                Idle::Exit => return Ok(None),
                Idle::Input(input) => {
                    self.recap = None;
                    return Ok(Some(input));
                }
            }
        }
    }

    pub(super) fn on_key_idle(&mut self, event: TermEvent) -> Idle {
        let key = match event {
            TermEvent::Paste(text) => {
                self.paste(&text);
                return Idle::Continue;
            }
            TermEvent::Resize(columns, rows) => {
                self.resize(columns, rows);
                return Idle::Continue;
            }
            TermEvent::Mouse(mouse) => {
                self.on_wheel(mouse.kind);
                return Idle::Continue;
            }
            TermEvent::Key(key) if key.kind != KeyEventKind::Release => {
                if !self.page_key(&key) {
                    self.history.follow();
                }
                key
            }
            _ => return Idle::Continue,
        };
        if self.picker.is_some() {
            return self.on_key_picker(key);
        }
        if self.asking.is_some() {
            self.on_key_asking(key);
            return Idle::Continue;
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
                    // Armed, not printed: the live area shows it until it lapses, so the
                    // transcript keeps only what was done.
                    self.exit_armed = Some(Instant::now());
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

    /// The wheel scrolls the conversation, three rows a notch; fullscreen mode only.
    pub(super) fn on_wheel(&mut self, kind: MouseEventKind) {
        match kind {
            MouseEventKind::ScrollUp => self.scroll(-3),
            MouseEventKind::ScrollDown => self.scroll(3),
            _ => {}
        }
    }

    /// PageUp and PageDown scroll the conversation by a page, fullscreen mode only. False for
    /// every other key, which goes back to following the conversation.
    pub(super) fn page_key(&mut self, key: &KeyEvent) -> bool {
        let page = self.view_rows.saturating_sub(2).max(1) as isize;
        match key.code {
            KeyCode::PageUp => self.scroll(-page),
            KeyCode::PageDown => self.scroll(page),
            _ => return false,
        }
        true
    }

    /// Sends the prompt: a command when it starts with `/`, else a message to the model.
    pub(super) fn submit(&mut self) -> Idle {
        let (text, images) = self.editor.take_with_images();
        match submission(text, images) {
            Some(input) => Idle::Input(input),
            None => Idle::Continue,
        }
    }

    pub(super) fn on_key_asking(&mut self, key: KeyEvent) -> bool {
        // ←/→ walk this turn's questions while one waits: back over the answered ones,
        // forward again to the waiting one. Typing keeps the arrows for the text.
        let navigable = self.asking.as_ref().is_some_and(|a| !a.internal && !a.question.plan) && !self.ask_past.is_empty();
        if navigable && self.editor.is_empty() {
            match key.code {
                KeyCode::Left => {
                    self.ask_view = Some(match self.ask_view {
                        None => self.ask_past.len() - 1,
                        Some(view) => view.saturating_sub(1),
                    });
                    return false;
                }
                KeyCode::Right if self.ask_view.is_some() => {
                    let view = self.ask_view.expect("viewing an answered question");
                    self.ask_view = if view + 1 >= self.ask_past.len() { None } else { Some(view + 1) };
                    return false;
                }
                _ => {}
            }
        }
        if self.ask_view.is_some() {
            return self.on_key_past(key);
        }
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
                if asking.internal {
                    self.search = None;
                }
                // A correction with no answer to ride on goes on its own, or it is lost.
                let note = std::mem::take(&mut self.ask_corrections);
                let payload = if note.is_empty() { String::new() } else { format!("{}（本问未作答）", note.join("\n")) };
                for line in &note {
                    self.commit(format!("  {FIN}↳{RESET} {line}"));
                }
                self.ask_past.clear();
                self.ask_view = None;
                let _ = asking.question.reply.send(payload);
                self.commit(format!("{DIM}  (dismissed){RESET}"));
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) && !self.editor.is_empty() => self.editor.clear(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Enter if !newline_key(&key) => {
                let secret = self.asking.as_ref().is_some_and(|asking| asking.internal && asking.secret);
                let typed = if secret { self.editor.take_quiet() } else { self.editor.take() };
                self.answer(Some(typed).filter(|t| !t.trim().is_empty()));
            }
            _ => self.edit_key(key),
        }
        false
    }

    /// Keys while an answered question is on screen instead of the waiting one: choosing
    /// confirms the change and returns to the waiting question; Esc just returns.
    fn on_key_past(&mut self, key: KeyEvent) -> bool {
        let view = self.ask_view.expect("viewing an answered question");
        let count = self.ask_past[view].options.len();
        match key.code {
            KeyCode::Up if self.editor.is_empty() => self.ask_past[view].selected = self.ask_past[view].selected.saturating_sub(1),
            KeyCode::Down if self.editor.is_empty() => {
                self.ask_past[view].selected = (self.ask_past[view].selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Char(c) if self.editor.is_empty() && c.is_ascii_digit() && (1..=count).contains(&(c as usize - '0' as usize)) => {
                self.ask_past[view].selected = c as usize - '1' as usize;
                self.confirm_past(None);
            }
            KeyCode::Tab if self.editor.is_empty() => self.confirm_past(None),
            KeyCode::Esc => self.ask_view = None,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) && !self.editor.is_empty() => self.editor.clear(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Enter if !newline_key(&key) => {
                let typed = self.editor.take();
                self.confirm_past(Some(typed).filter(|t| !t.trim().is_empty()));
            }
            _ => self.edit_key(key),
        }
        false
    }

    /// Records a revisited answer that changed as a correction for the model, and goes
    /// back to the waiting question. An unchanged answer records nothing.
    fn confirm_past(&mut self, typed: Option<String>) {
        let view = self.ask_view.take().expect("viewing an answered question");
        let past = &mut self.ask_past[view];
        let answer = typed.unwrap_or_else(|| past.options.get(past.selected).cloned().unwrap_or_default());
        if let Some(at) = past.options.iter().position(|option| *option == answer) {
            past.selected = at;
        }
        if answer != past.answer {
            let old = std::mem::replace(&mut past.answer, answer.clone());
            self.ask_corrections.push(format!("更正之前的问题「{}」：答案改为「{answer}」（原为「{old}」）", first_line(&past.text, 60)));
        }
    }

    fn answer(&mut self, typed: Option<String>) {
        let asking = self.asking.take().expect("a question is open");
        let answer = typed.unwrap_or_else(|| asking.question.options.get(asking.selected).cloned().unwrap_or_default());
        if asking.internal {
            return self.search_answer(&answer, asking.secret);
        }
        if !asking.question.plan {
            let selected = asking.question.options.iter().position(|option| *option == answer).unwrap_or(asking.selected);
            self.ask_past.push(PastAsk { text: asking.question.text.clone(), options: asking.question.options.clone(), selected, answer: answer.clone() });
        }
        let note = std::mem::take(&mut self.ask_corrections);
        for line in &note {
            self.commit(format!("  {FIN}↳{RESET} {line}"));
        }
        self.commit(format!("  {FIN}↳{RESET} {}", text::sanitize(&answer)));
        let payload = if note.is_empty() { answer } else { format!("{}\n本问答案：{answer}", note.join("\n")) };
        let _ = asking.question.reply.send(payload);
    }

    /// Keys that edit the prompt, the same whether or not the agent works.
    pub(super) fn edit_key(&mut self, key: KeyEvent) {
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
            KeyCode::Char('l') if ctrl && self.fullscreen => {
                self.history.follow();
                self.shown.clear();
            }
            KeyCode::Char('l') if ctrl => {
                let _ = execute!(self.out, Clear(ClearType::All), MoveTo(0, 0));
                self.cursor_row = 0;
                self.shown.clear();
                self.since_banner = None;
                self.content_rows = 0;
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
    /// clipboard holds only an image: that attaches the image. A paste of file paths is a
    /// drop on the terminal: an image is attached like a pasted one, any other file is
    /// named with `@` so its contents go with the message.
    pub(super) fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return self.paste_clipboard_image();
        }
        let Some(files) = paste::dropped_files(text) else { return self.editor.insert(text) };
        for file in files {
            match image_type(&file) {
                Some(media_type) => {
                    let name = file.file_name().map_or_else(|| file.display().to_string(), |n| n.to_string_lossy().into_owned());
                    self.attach_image(|| image_url(&name, &file, media_type).map(|(url, _)| url));
                }
                None => {
                    let shown = file.strip_prefix(&self.cwd).unwrap_or(&file);
                    self.editor.insert_word(&format!("@{}", mention::escape(&shown.display().to_string())));
                }
            }
        }
    }

    /// Ctrl+V: attaches the clipboard's image to the prompt as an `[Image #N]` placeholder.
    fn paste_clipboard_image(&mut self) {
        self.attach_image(paste::clipboard_image);
    }

    /// Attaches the image `read` gives to the prompt as an `[Image #N]` placeholder, when the
    /// model can see it and the prompt is not an answer to a question.
    fn attach_image(&mut self, read: impl FnOnce() -> Result<String, String>) {
        if !self.sees_images {
            return self.notice(&format!("{} can't see images; /model switches to one that can.", self.status.model));
        }
        if self.asking.is_some() {
            return self.notice("An answer is text only; send the image with your next message.");
        }
        match read() {
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

    pub(super) fn toggle_plan(&mut self) {
        let on = !self.controls.plan.get();
        self.controls.plan.set(on);
        self.notice(if on {
            "Plan mode on: the agent researches and plans; writes are blocked until you approve its plan."
        } else {
            "Plan mode off."
        });
    }

    pub(super) fn set_swarm(&mut self, on: bool) {
        self.controls.swarm.set(on);
        self.notice(if on {
            "Swarm mode on: the agent splits the work across many subagents with agent_swarm."
        } else {
            "Swarm mode off."
        });
    }
}
