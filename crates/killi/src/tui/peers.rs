use super::*;

impl App {
    /// Answers another session: what this one is doing at once, a question from this
    /// conversation through a side request that keeps nothing, and a message by queueing it
    /// for the model's next step, or for the next prompt while idle.
    pub(super) fn on_peer(&mut self, incoming: live::Incoming) {
        let live::Incoming { request, reply } = incoming;
        let working = self.live.as_ref().is_some_and(|l| l.entry.working);
        match request {
            live::Request::Status => {
                let todos: Vec<Value> = self
                    .todos
                    .iter()
                    .map(|t| json!({"content": t.content, "status": t.status}))
                    .collect();
                let waiting = self.controls.aside.borrow().waiting();
                let _ = reply.send(json!({"ok": true, "working": working, "plan": self.controls.plan.get(), "todos": todos, "waiting": waiting}));
            }
            live::Request::Ask { from, text } => {
                if self.peer_asking.len() >= MAX_PEER_ASKS {
                    let _ = reply.send(json!({"error": "it is answering other sessions' questions; ask again in a minute"}));
                    return;
                }
                let ask = self
                    .controls
                    .aside
                    .borrow()
                    .ask_with(&live::ask_note(&live::sender(from)), &text);
                self.peer_ask_id += 1;
                let id = self.peer_ask_id;
                self.peer_asking.push((
                    id,
                    peer_label(from),
                    text::sanitize_row(&text).into_owned(),
                    Instant::now(),
                ));
                let question = text::sanitize(&text).into_owned();
                self.peer_asks.push(Box::pin(async move {
                    PeerAnswer {
                        id,
                        from,
                        question,
                        answer: ask.await,
                        reply,
                    }
                }));
            }
            live::Request::Tell { from, text } => {
                if self.peer_tells >= MAX_PEER_TELLS {
                    let _ = reply.send(json!({"error": format!("it has taken {MAX_PEER_TELLS} messages since its user's last prompt; send this one after the next")}));
                    return;
                }
                self.peer_tells += 1;
                self.controls
                    .peer_notes
                    .borrow_mut()
                    .push_back(live::tell_note(&live::sender(from), from, &text));
                let (delivered, note) = if working {
                    (
                        "It is working: its model reads the message at its next step, or with its user's next prompt if the turn ends first.",
                        "For the agent's next step",
                    )
                } else {
                    (
                        "It is idle: its user sees the message now, and its model gets it with their next prompt.",
                        "For the agent, with your next prompt",
                    )
                };
                let _ = reply.send(json!({"ok": true, "delivered": delivered}));
                let who = peer_label(from);
                let shown = text::sanitize(&text).into_owned();
                if !working {
                    notify(&format!("Message from {who}"), &first_line(&shown, 120));
                }
                let tag = format!("{who} says");
                self.held_asides.push(Line::block(move |width| {
                    aside_rows(&tag, "", &shown, note, width)
                }));
                self.release_asides(!working);
            }
        }
    }

    /// Sends another session the answer to its question and shows both here.
    pub(super) fn on_peer_answer(&mut self, answered: PeerAnswer) {
        let PeerAnswer {
            id,
            from,
            question,
            answer,
            reply,
        } = answered;
        self.peer_asking.retain(|(asked, ..)| *asked != id);
        let tag = format!("{} asks", peer_label(from));
        match answer {
            Ok((text, usage)) => {
                let _ = reply.send(json!({"ok": true, "answer": text}));
                let cached = usage.cached * 100 / usage.prompt.max(1);
                let note = format!(
                    "Answered from this conversation, not kept · {} in · {cached}% cached",
                    short(usage.prompt)
                );
                let shown = text::sanitize(&text).into_owned();
                self.held_asides.push(Line::block(move |width| {
                    aside_rows(&tag, &question, &shown, &note, width)
                }));
                self.release_asides(self.busy.is_none());
            }
            Err(e) => {
                let why = gist(&format!("{e:#}"));
                self.notice(&format!(
                    "{tag} “{}”; no answer: {why}",
                    first_line(&question, 80)
                ));
                let _ = reply.send(json!({"error": why}));
            }
        }
    }

    /// The live area's rows for questions from other sessions being answered.
    pub(super) fn peer_rows(&self, frame: char, width: usize) -> Vec<String> {
        self.peer_asking
            .iter()
            .map(|(_, who, question, since)| {
                let question = clip(&question.split_whitespace().collect::<Vec<_>>().join(" "), width.saturating_sub(who.len() + 16));
                fit(&format!("{LAGOON}{frame}{RESET} {BOLD}{FIN}{who} asks{RESET} {question}{DIM} · {}{RESET}", clock(since.elapsed())), width)
            })
            .collect()
    }

    /// Brings this session's file up to date with its model, log and branch.
    pub(super) fn live_sync(&mut self, agent: &Agent) {
        let branch = self
            .status
            .branch
            .as_ref()
            .map(|b| b.trim_end_matches('*').to_string());
        if let Some(live) = &mut self.live {
            live.update(|e| {
                e.model = agent.model_alias().to_string();
                e.log = agent.session.log_path.clone();
                e.branch = branch;
            });
        }
    }

    /// Says in this session's file that a turn started on `prompt`, or that it ended.
    pub(super) fn live_turn(&mut self, prompt: Option<&str>) {
        if let Some(live) = &mut self.live {
            live.update(|e| {
                e.working = prompt.is_some();
                e.since = live::now();
                if let Some(prompt) = prompt {
                    e.prompt = first_line(prompt, 200);
                }
            });
        }
    }

    /// The banner's line about the other sessions working in this repository, if any.
    pub(super) fn peers_line(&self) -> Option<String> {
        let others = self.live.as_ref()?.here().others();
        if others.is_empty() {
            return None;
        }
        let now = live::now();
        let list: Vec<String> = others.iter().map(|e| live::describe(e, now)).collect();
        Some(format!(
            "{DIM}Also in this repository: {}. /sessions lists every session.{RESET}",
            list.join("; ")
        ))
    }

    /// Lists the other live sessions, those in this repository first.
    pub(super) fn show_sessions(&mut self) {
        let tree = live::work_tree(&self.cwd);
        let me = std::process::id();
        let mut others: Vec<live::Entry> =
            live::list().into_iter().filter(|e| e.pid != me).collect();
        if others.is_empty() {
            return self.notice("No other killi session is running on this machine.");
        }
        others.sort_by_key(|e| e.tree != tree);
        let now = live::now();
        let rows: Vec<String> = others
            .iter()
            .map(|e| {
                let mark = if e.tree == tree {
                    format!("{LAGOON}●{RESET}")
                } else {
                    format!("{FAINT}○{RESET}")
                };
                let headless = if e.socket.is_none() {
                    format!(" {DIM}· headless{RESET}")
                } else {
                    String::new()
                };
                format!("{mark} {}{headless}", live::describe(e, now))
            })
            .collect();
        let here = if others.iter().any(|e| e.tree == tree) {
            format!("{LAGOON}●{RESET}{DIM} works in this repository. ")
        } else {
            DIM.to_string()
        };
        let hint = format!(
            "{here}Ask the agent to ask one about its work or to leave it a message; it uses `killi ask` and `killi tell`.{RESET}"
        );
        self.begin(Kind::Note);
        self.emit(Line::block(move |width| {
            rows.iter()
                .flat_map(|row| wrap_words(row, width, 2))
                .chain(wrap_words(&hint, width, 0))
                .collect()
        }));
    }
}
