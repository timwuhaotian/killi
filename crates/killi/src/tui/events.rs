use super::*;

/// How the rows of subagents outside a swarm panel are found again: their label and the
/// call's id, since two subagents' providers can hand out the same id.
fn sub_key(label: &str, id: &str) -> String {
    format!("{label}\0{id}")
}

impl App {
    pub(super) fn on_agent(&mut self, event: Event) {
        if !matches!(event, Event::Retry { .. } | Event::Sub { .. }) {
            self.retry = None;
        }
        match event {
            Event::Text(text) => {
                let text = if self.last_kind == Kind::Text { text.as_str() } else { text.trim_start() };
                if text.is_empty() {
                    return;
                }
                self.end_thinking();
                self.begin(Kind::Text);
                self.partial.push_str(&self.stream.push(text));
                while let Some(end) = self.partial.find('\n') {
                    let line: String = self.partial.drain(..=end).collect();
                    let line = line.trim_end_matches('\n');
                    if line.trim().is_empty() {
                        let table = self.markdown.finish_parts();
                        self.commit_parts(table);
                        self.held_blank = true;
                        self.release_asides(false);
                        continue;
                    }
                    if std::mem::take(&mut self.held_blank) {
                        self.commit(String::new());
                    }
                    let styled = self.markdown.parts(line);
                    self.commit_parts(styled);
                }
            }
            Event::Reasoning(delta) => {
                let clean = self.reasoning.push(&delta);
                match &mut self.thinking {
                    Some((_, buffer)) => buffer.push_str(&clean),
                    None => self.thinking = Some((Instant::now(), clean)),
                }
            }
            Event::Retry { attempt, wait, reason } => {
                self.retry = Some((attempt, Instant::now() + wait));
                let discarded = self.flush_partial();
                self.thinking = None;
                let _ = self.reasoning.finish();
                if discarded {
                    self.notice("the partial reply above was discarded; retrying …");
                }
                let line = format!("{WARN}⚠ retry {attempt} in {}:{RESET}{DIM} {}{RESET}", clock(wait), text::sanitize_row(&gist(&reason)));
                self.begin(Kind::Tool);
                self.emit(Line::Fit(line));
            }
            Event::ToolStart { id, summary, .. } => {
                let summary = text::sanitize_row(&summary).into_owned();
                if summary == "later" || summary.starts_with("later ") {
                    self.later_noted = true;
                }
                self.flush_partial();
                self.end_thinking();
                self.begin(Kind::Tool);
                self.running.push(Running { id, summary, label: None, started: Instant::now() });
            }
            Event::ToolEnd { id, is_error, brief, display, output } => {
                if self.swarm.as_ref().is_some_and(|s| s.id == id) {
                    self.swarm = None;
                }
                let running = self.take_running(&id);
                let elapsed = running.as_ref().map(|r| r.started.elapsed());
                let summary = running.map(|r| r.summary).unwrap_or_default();
                if is_bookkeeping(&summary, is_error) {
                    return;
                }
                let brief = text::sanitize_row(&brief);
                let output = text::sanitize(&output);
                self.tool_line(None, &summary, is_error, &brief, elapsed);
                if display.is_none() && summary.starts_with("bash ") {
                    self.output_preview(&output, is_error);
                }
                if let Some(display) = display {
                    let shown: Vec<String> = display.lines().take(DIFF_ROWS).map(str::to_string).collect();
                    self.emit(Line::block(move |width| diff_rows(&shown, width)));
                    self.hidden_note(display.lines().count().saturating_sub(DIFF_ROWS), "more");
                    let detail = text::sanitize(&display).into_owned();
                    self.push_detail(summary, detail);
                } else {
                    self.push_detail(summary, output.into_owned());
                }
            }
            Event::Usage(usage) => self.add_usage(&usage, true),
            Event::Todos(mut todos) => {
                todos.iter_mut().for_each(|todo| todo.content = text::sanitize_row(&todo.content).into_owned());
                self.todos = todos;
            }
            Event::Ask(question) => {
                self.flush_partial();
                self.end_thinking();
                self.begin(Kind::Note);
                if question.plan {
                    self.commit(format!("{BOLD}{FIN}? Plan{RESET}"));
                    let plan = text::sanitize(&question.text).into_owned();
                    self.emit(Line::block(move |width| {
                        let mut markdown = Markdown::new(width.saturating_sub(2));
                        let mut rows: Vec<String> = plan.lines().flat_map(|line| markdown.line(line)).collect();
                        rows.extend(markdown.finish());
                        rows.into_iter()
                            .flat_map(|row| {
                                let row = format!("  {row}");
                                wrap_words(&row, width, text::hanging_indent(&row))
                            })
                            .collect()
                    }));
                    self.commit(String::new());
                } else {
                    let text = question.text.clone();
                    self.emit(Line::block(move |width| question_rows(&text, width)));
                }
                let asked = first_line(&question.text, 120);
                self.asking = Some(Asking { question, selected: 0, internal: false, secret: false });
                notify("Waiting for your answer", &asked);
            }
            Event::Notice(text) => {
                self.flush_partial();
                self.end_thinking();
                self.notice(&text);
            }
            Event::Swarm { id, description, items } => {
                self.flush_partial();
                self.end_thinking();
                let description = text::sanitize_row(&description).into_owned();
                let items = items.iter().map(|item| text::sanitize_row(item).into_owned()).collect();
                self.swarm = Some(swarm::Swarm::new(id, description, items));
            }
            Event::Started | Event::Finished { .. } => {}
            Event::Sub { label, slot, event } => self.on_sub(label, slot, *event),
        }
    }

    pub(super) fn on_sub(&mut self, label: String, slot: Option<Slot>, event: Event) {
        // A swarm's subagents show in its panel and leave a line when they end, not a line per call.
        let member = slot.filter(|slot| self.swarm.as_ref().is_some_and(|s| s.id == slot.swarm)).map(|slot| slot.index);
        match event {
            Event::ToolStart { id, summary, .. } => match (member, &mut self.swarm) {
                (Some(index), Some(swarm)) => {
                    let summary = text::sanitize_row(&summary);
                    if !is_bookkeeping(&summary, false) {
                        swarm.call(index, &summary);
                    }
                }
                _ => {
                    let summary = text::sanitize_row(&summary).into_owned();
                    self.running.push(Running { id: sub_key(&label, &id), summary, label: Some(label), started: Instant::now() })
                }
            },
            Event::ToolEnd { id, is_error, brief, .. } if member.is_none() => {
                let running = self.take_running(&sub_key(&label, &id));
                let elapsed = running.as_ref().map(|r| r.started.elapsed());
                let summary = running.map(|r| r.summary).unwrap_or_default();
                if !is_bookkeeping(&summary, is_error) {
                    self.tool_line(Some(&label), &summary, is_error, &text::sanitize_row(&brief), elapsed);
                }
            }
            Event::Started => {
                if let (Some(index), Some(swarm)) = (member, &mut self.swarm) {
                    swarm.start(index);
                }
            }
            Event::Finished { is_error } => {
                let Some((index, swarm)) = member.zip(self.swarm.as_mut()) else { return };
                let Some((item, calls, took)) = swarm.finish(index, is_error) else { return };
                self.begin(Kind::Tool);
                self.emit(Line::block(move |width| vec![swarm::finished_row(&item, is_error, calls, took, width)]));
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
            (format!("{FAINT}✓{RESET}"), String::new())
        } else {
            (format!("{FAINT}✓{RESET}"), format!(" {DIM}· {brief}{RESET}"))
        };
        let took = elapsed.filter(|e| e.as_secs() >= 1).map(|e| format!(" {DIM}· {}{RESET}", precise_clock(e)));
        let note = format!("{note}{}", took.unwrap_or_default());
        let (label, summary) = (label.map(str::to_string), summary.to_string());
        self.begin(Kind::Tool);
        self.emit(Line::block(move |width| {
            let lead = label.as_ref().map_or(0, |l| text::width(l) + 5);
            let summary = clip(&tool_text(&summary), width.saturating_sub(lead + 2 + text::width(&note)).max(20));
            let line = match &label {
                Some(label) => format!("  {DIM}↳ {label}:{RESET} {glyph} {DIM}{summary}{RESET}{note}"),
                None => format!("{glyph} {}{note}", tool_summary(&summary)),
            };
            vec![fit(&line, width)]
        }));
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
            self.emit(Line::Fit(format!("  {FAINT}│{RESET} {DIM}{line}{RESET}")));
        }
        self.hidden_note(skipped, if is_error { "earlier" } else { "more" });
    }

    /// The row under a call's output that says `count` lines were left out, and, the first time
    /// in a session, that Ctrl+O shows them: only the newest call's key does anything, so the
    /// rows after it would just repeat themselves.
    fn hidden_note(&mut self, count: usize, which: &str) {
        if count == 0 {
            return;
        }
        let teach = !std::mem::replace(&mut self.ctrl_o_taught, true);
        self.commit(hidden_row(count, which, teach));
    }

    pub(super) fn push_detail(&mut self, title: String, body: String) {
        self.details.push((title, body));
        if self.details.len() > DETAIL_KEPT {
            self.details.remove(0);
        }
        self.detail_back = 0;
    }
}
