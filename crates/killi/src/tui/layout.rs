use super::*;

/// The status line's parts fitted to `width`; see `App::status_line`. The model and mode stand
/// at the left and the figures at the right edge, in groups three spaces apart. Each kind of
/// figure has its own color: the effort, the tokens and the branch; the meter is lagoon, and
/// warn from 80%. None when even the shortest layout runs past the window.
pub(super) fn status_layout(head: &str, status: &Status, width: usize) -> Option<String> {
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
            true => format!("{} in{dot}{} cached{dot}{} out", count(short(status.prompt)), count(format!("{cached}%")), count(short(status.completion)), dot = dot()),
            false => format!("{} in{dot}{} out", count(short(status.prompt)), count(short(status.completion)), dot = dot()),
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
    for (context, tokens, branch) in layouts {
        let figures: Vec<String> = [Some(context), tokens, branch].into_iter().flatten().collect();
        let right = figures.join("   ");
        let used = text::width(head) + text::width(&right);
        if used + 3 <= width {
            return Some(format!("{DIM}{head}{}{right}{RESET}", " ".repeat(width - used)));
        }
    }
    None
}

impl App {
    /// The live area's rows and where the cursor goes in them, padded above with blank rows so
    /// it ends on the bottom row of a screen the conversation doesn't fill yet.
    pub(super) fn live(&mut self) -> (Vec<String>, usize, usize) {
        let width = self.width;
        let mut rows: Vec<String> = Vec::new();
        // Rows that go first when the live area runs past the window: the task list, then
        // the queue, then the streaming preview, the /btw block and the peer rows. The
        // running calls, the spinner and the retry countdown never go.
        let mut shrink_tasks: Vec<usize> = Vec::new();
        let mut shrink_queue: Vec<usize> = Vec::new();
        let mut shrink_head: Vec<usize> = Vec::new();
        // A blank row, unless the row above is blank already: it keeps the parts of the live
        // area, and the live area and the conversation, from touching.
        let space = |rows: &mut Vec<String>| {
            if rows.last().map_or(!self.last_blank, |row| !row.is_empty()) {
                rows.push(String::new());
            }
        };
        if !self.partial.is_empty() {
            // Preview touches the scrollback without this: `draw` prints pending rows
            // then the live rows with no gap of its own.
            space(&mut rows);
            let cached = match &self.preview_cache {
                Some((tail, cached_width, cached_gen, preview))
                    if *tail == self.partial && *cached_width == width && *cached_gen == self.markdown.generation() =>
                {
                    Some(preview.clone())
                }
                _ => None,
            };
            let preview = match cached {
                Some(preview) => preview,
                None => {
                    let mut markdown = self.markdown.clone();
                    let mut styled = markdown.line(&self.partial);
                    styled.extend(markdown.finish());
                    let preview: Vec<String> = styled.iter().flat_map(|row| wrap(row, width)).collect();
                    self.preview_cache =
                        Some((self.partial.clone(), width, self.markdown.generation(), preview.clone()));
                    preview
                }
            };
            let head = rows.len();
            rows.extend(preview.into_iter().rev().take(6).rev());
            shrink_head.extend(head..rows.len());
        } else {
            self.preview_cache = None;
        }
        if let Some(started) = self.busy {
            let frame = QUARTER[self.spinner % QUARTER.len()];
            // Work in flight continues the tool rows above it, but prose gets a blank row first.
            if !rows.is_empty() || matches!(self.last_kind, Kind::Text | Kind::Note) {
                space(&mut rows);
            }
            let waiting_on_you = |running: &Running| {
                self.asking.is_some()
                    && (running.summary.starts_with("ask_user_question") || running.summary.starts_with("exit_plan_mode"))
            };
            for running in self.running.iter().filter(|r| !waiting_on_you(r)).take(5) {
                if let Some(swarm) = self.swarm.as_ref().filter(|s| s.id == running.id) {
                    // A panel, not one more tool row: the lines its finished subagents left
                    // above look like its own rows.
                    space(&mut rows);
                    rows.extend(swarm.rows(width, self.spinner, (self.height / 3).clamp(6, 14)));
                    continue;
                }
                let took = format!(" {DIM}· {}{RESET}", clock(running.started.elapsed()));
                let line = match &running.label {
                    Some(label) => format!("  {DIM}↳ {label}:{RESET} {LAGOON}{frame}{RESET} {DIM}{}{RESET}{took}", tool_text(&running.summary)),
                    None => format!("{LAGOON}{frame}{RESET} {}{took}", tool_summary(&tool_text(&running.summary))),
                };
                rows.push(fit(&line, width));
            }
            if let Some((question, since)) = &self.aside {
                let question = clip(&question.split_whitespace().collect::<Vec<_>>().join(" "), width.saturating_sub(14));
                shrink_head.push(rows.len());
                rows.push(fit(&format!("{LAGOON}{frame}{RESET} {BOLD}{FIN}btw{RESET} {question}{DIM} · {}{RESET}", clock(since.elapsed())), width));
            }
            let peers = rows.len();
            rows.extend(self.peer_rows(frame, width));
            shrink_head.extend(peers..rows.len());
            if self.asking.is_none() {
                // The action word never touches the tool rows or the conversation; the
                // elapsed time and the interrupt hint sit on their own row under it.
                space(&mut rows);
                let retry = self.retry.map(|(attempt, until)| (attempt, until.saturating_duration_since(Instant::now())));
                if self.snapshotting && retry.is_none_or(|(_, left)| left.is_zero()) {
                    rows.extend(snapshot_rows(self.snapshot_seq, self.spinner, started.elapsed()).into_iter().map(|row| fit(&row, width)));
                } else {
                    let (action, detail) = if let Some((attempt, left)) = retry.filter(|(_, left)| !left.is_zero()) {
                        ("retrying".to_string(), format!("in {} · attempt {attempt}", clock(left)))
                    } else if let Some((_, text)) = &self.thinking {
                        ("thinking".to_string(), first_sentence(text))
                    } else if !self.partial.trim().is_empty() {
                        ("writing".to_string(), String::new())
                    } else {
                        ("working".to_string(), String::new())
                    };
                    rows.extend(working_rows(self.spinner, started.elapsed(), &action, &detail).into_iter().map(|row| fit(&row, width)));
                }
            }
            let queue = rows.len();
            for queued in &self.queued {
                rows.push(fit(&queued_row(queued), width));
            }
            shrink_queue.extend(queue..rows.len());
        } else if !self.peer_asking.is_empty() {
            space(&mut rows);
            rows.extend(self.peer_rows(QUARTER[self.spinner % QUARTER.len()], width));
        }
        if self.todos.iter().any(|t| t.status != "completed") {
            let tasks = rows.len();
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
            shrink_tasks.extend((tasks..rows.len()).rev());
        }
        let mode = self.controls.plan.get().then_some("plan mode · Shift+Tab leaves it");
        space(&mut rows);
        rows.push(rule(mode, width));
        let known = |name: &str| self.entries.iter().any(|e| e.name == name || e.aliases.iter().any(|a| a == name));
        let (mut input, cursor_row, cursor_col) = self.editor.render(&format!("{FIN}❯{RESET} "), width, &known);
        if let Some(hint) = self.placeholder() {
            input[0] = fit(&format!("{FIN}❯{RESET} {FAINT}{hint}{RESET}"), width);
        }
        let first = cursor_row.saturating_sub(MAX_INPUT_ROWS - 1);
        let top = rows.len();
        rows.extend(input.into_iter().skip(first).take(MAX_INPUT_ROWS));
        let mut cursor = (top + cursor_row - first, cursor_col);
        rows.push(rule(mode.map(|_| ""), width));
        let modal = self.picker.is_some() || self.asking.is_some();
        let room = (self.height - 1).saturating_sub(rows.len() - top + 4 + usize::from(modal)).min(MENU_ROWS);
        let listed = rows.len();
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
            let position = if count > visible { format!("{}/{count}", picker.selected + 1) } else { String::new() };
            let filter = match (picker.query.is_empty(), count) {
                (true, _) => "type to filter".to_string(),
                (false, 0) => format!("{RESET}{}{DIM} matches nothing", picker.query),
                (false, _) => format!("filter {RESET}{}{DIM}", picker.query),
            };
            let hints = [(2, filter), (1, position), (4, "↑↓ choose".into()), (5, "Tab or Enter selects".into()), (3, "Esc closes".into())];
            rows.push(hint_row(&hints, width));
        }
        if let Some(asking) = &self.asking {
            // While an answered question is revisited, its text and given answer stand
            // above its options; the waiting question's text is in the scrollback already.
            let past = self.ask_view.and_then(|view| self.ask_past.get(view)).filter(|_| !asking.internal && !asking.question.plan);
            let (options, selected) = match past {
                Some(past) => (past.options.as_slice(), past.selected),
                None => (asking.question.options.as_slice(), asking.selected),
            };
            let names: Vec<String> = options.iter().enumerate().map(|(i, o)| format!("{}. {}", i + 1, text::sanitize_row(o))).collect();
            if let (Some(past), Some(view)) = (past, self.ask_view) {
                let head = format!("{BOLD}{FIN}?{RESET} {DIM}question {}/{}{RESET} {BOLD}{}{RESET}", view + 1, self.ask_past.len() + 1, first_line(&past.text, 80));
                rows.push(fit(&head, width));
            }
            let visible = names.len().min(room.max(1));
            rows.extend(ask_rows(&names, selected.min(names.len().saturating_sub(1)), visible, width));
            if let Some(past) = past {
                rows.push(fit(&format!("  {FIN}↳{RESET} {DIM}answered: {}", text::sanitize_row(&past.answer)), width));
            }
            let mut hints: Vec<(u8, String)> = match names.len() {
                0 => Vec::new(),
                1 => vec![(5, "Tab or Enter chooses".into())],
                n => vec![(4, format!("1-{} or ↑↓ choose", n.min(9))), (5, "Tab or Enter confirms".into())],
            };
            match past {
                Some(_) => {
                    if let Some(confirm) = hints.get_mut(1) {
                        confirm.1 = "Tab or Enter confirms change".into();
                    }
                    hints.push((6, format!("←→ questions {}/{}", self.ask_view.expect("viewing an answered question") + 1, self.ask_past.len() + 1)));
                    hints.extend([(3, "or type a new answer".to_string()), (2, "Esc back".to_string())]);
                }
                None => {
                    let own = if asking.question.plan { "or type a change" } else { "or type an answer" };
                    if !asking.internal && !asking.question.plan && !self.ask_past.is_empty() {
                        hints.push((6, "← previous question".to_string()));
                    }
                    hints.extend([(3, own.to_string()), (2, "Esc dismisses".to_string())]);
                }
            }
            rows.push(hint_row(&hints, width));
        }
        if rows.len() > listed + usize::from(modal) {
            rows.push(String::new());
        }
        if self.exit_armed.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            rows.push(fit(&format!("{DIM}Press Ctrl+C again to exit.{RESET}"), width));
        }
        rows.push(self.status_line(width));
        let mut gone = 0;
        let mut excess = rows.len().saturating_sub(self.height - 1);
        if excess > 0 {
            let mut drop: Vec<usize> =
                shrink_tasks.into_iter().chain(shrink_queue).chain(shrink_head).take(excess).collect();
            drop.sort_unstable_by(|a, b| b.cmp(a));
            gone = drop.iter().filter(|&&i| i < cursor.0).count();
            for i in drop {
                rows.remove(i);
            }
            cursor.0 -= gone;
            excess = rows.len().saturating_sub(self.height - 1);
        }
        if excess > 0 {
            let cut = excess.min(top.saturating_sub(gone));
            rows.drain(..cut);
            cursor.0 -= cut;
        }
        let pad = if self.fullscreen { 0 } else { padding(self.height, self.content_rows + self.pending_rows(), rows.len()) };
        rows.splice(0..0, std::iter::repeat_n(String::new(), pad));
        (rows, cursor.0 + pad, cursor.1)
    }

    fn placeholder(&self) -> Option<&'static str> {
        placeholder(self.editor.is_empty(), self.busy.is_some(), self.picker.is_some() || self.asking.is_some())
    }

    /// The slash or `@` menu, `visible` items at a time around the selection, and a line of keys.
    fn menu_rows(&self, menu: &Suggestions, visible: usize) -> Vec<String> {
        let count = menu.items.len();
        let selected = self.menu_selected.min(count - 1);
        let (lead, column) = match menu.fill {
            Fill::Command => ("/", 32),
            Fill::Argument => ("", 32),
            Fill::File => ("@", self.width),
        };
        let names: Vec<String> = menu.items.iter().map(|i| format!("{lead}{}", i.value)).collect();
        let options: Vec<(&str, &str)> = names.iter().zip(&menu.items).map(|(name, item)| (name.as_str(), item.description.as_str())).collect();
        let mut rows = choice_rows(&options, selected, visible, self.width, column);
        let position = if count > visible { format!("{}/{count}", selected + 1) } else { String::new() };
        let enter = match (menu.fill, runs_at_once(menu, selected)) {
            (Fill::Command, true) => "runs",
            (Fill::Command, false) => "completes",
            (Fill::Argument, _) => "fills in",
            (Fill::File, _) => "inserts",
        };
        let hints = [(1, position), (4, "↑↓ choose".into()), (5, format!("Tab or Enter {enter}")), (3, "Esc closes".into())];
        rows.push(hint_row(&hints, self.width));
        rows
    }

    /// The status line for `width` columns. Parts drop out from the least to the most useful as
    /// the terminal narrows: the cache and token detail, the context bar, the branch, and last
    /// the context figure, so the model and mode stay on screen at any width. `swarm mode`
    /// yields before the context figure does.
    pub(super) fn status_line(&self, width: usize) -> String {
        let mut head = vec![self.status.model.clone()];
        if let Some((word, value)) = thinking_parts(&self.status.thinking) {
            head.push(format!("{word} {}", tint(EFFORT, value)));
        }
        if let Some(backup) = self.status.backup.as_deref().filter(|backup| *backup != self.status.model.as_str()) {
            head.push(format!("{} {}", label("backup"), backup));
        }
        if self.controls.plan.get() {
            head.push(format!("{LAGOON}plan mode{RESET}{DIM}"));
        }
        let swarm = self.controls.swarm.get().then(|| format!("{LAGOON}swarm mode{RESET}{DIM}"));
        if let Some(swarm) = &swarm {
            head.push(swarm.clone());
        }
        let full = head.join(&dot());
        if let Some(line) = status_layout(&full, &self.status, width) {
            return line;
        }
        if swarm.is_some() {
            head.pop();
            if let Some(line) = status_layout(&head.join(&dot()), &self.status, width) {
                return line;
            }
        }
        format!("{DIM}{}{RESET}", fit(&full, width))
    }
}
