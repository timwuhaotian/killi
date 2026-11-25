use super::*;

impl Agent {
    /// Notes something for later, or crosses an item off, in the project's `later.md`.
    pub(super) fn later(&mut self, args: &Value) -> tools::Output {
        if let Some(number) = crate::later::done_arg(args) {
            return match crate::later::done(&self.ctx.cwd, number) {
                Ok(item) => {
                    tools::Output::ok(format!("Crossed off {number}: {item}")).brief("done")
                }
                Err(e) => tools::Output::error(format!("{e:#}")),
            };
        }
        let Some(note) = args["note"]
            .as_str()
            .map(str::trim)
            .filter(|note| !note.is_empty())
        else {
            return tools::Output::error(
                "later needs `note` (what, and where) or `done` (an item's number).",
            );
        };
        match crate::later::add(&self.ctx.cwd, note) {
            Ok(open) => {
                let items = crate::later::list(&self.ctx.cwd);
                tools::Output::ok(format!(
                    "Noted for later; {open} open. The user sees the list with /later.\n{}",
                    crate::later::numbered(&items)
                ))
                .brief(format!("{open} open"))
            }
            Err(e) => tools::Output::error(format!("{e:#}")),
        }
    }

    pub(super) fn todo_write(&mut self, args: &Value) -> tools::Output {
        let Ok(todos) = serde_json::from_value::<Vec<Todo>>(args["todos"].clone()) else {
            return tools::Output::error("todo_write needs `todos`: a list of {content, status}.");
        };
        if let Some(bad) = todos
            .iter()
            .find(|t| !matches!(t.status.as_str(), "pending" | "in_progress" | "completed"))
        {
            return tools::Output::error(format!(
                "Unknown status `{}`; use pending, in_progress or completed.",
                bad.status
            ));
        }
        let in_progress = todos.iter().filter(|t| t.status == "in_progress").count();
        if in_progress > 1 {
            return tools::Output::error(format!(
                "Only one item can be in_progress, got {in_progress}. Mark the others pending or completed and call todo_write again."
            ));
        }
        self.todos = todos.clone();
        let open = todos.iter().filter(|t| t.status != "completed").count();
        (self.sink)(Event::Todos(todos));
        tools::Output::ok(format!(
            "Task list updated: {open} open, {} done.",
            self.todos.len() - open
        ))
        .brief(format!("{open} open"))
    }

    pub(super) async fn ask(&mut self, args: &Value) -> tools::Output {
        if !self.controls.interactive {
            return tools::Output::error(
                "No one can answer questions in this session. Decide yourself, and say in your final message what you assumed.",
            );
        }
        let (text, options) = match question_args(args) {
            Ok(parsed) => parsed,
            Err(why) => return tools::Output::error(why),
        };
        match self.wait_for_answer(text, options, false).await {
            Reply::Answered(answer) => {
                tools::Output::ok(format!("The user answered: {answer}")).brief(answer)
            }
            Reply::Dismissed(notes) => tools::Output::ok(with_notes(
                "The question was dismissed without an answer.",
                notes,
            ))
            .brief("dismissed"),
        }
    }

    pub(super) async fn exit_plan(&mut self, args: &Value) -> tools::Output {
        if !self.controls.plan.get() {
            return tools::Output::error("Plan mode is off; go ahead without exit_plan_mode.");
        }
        if self.depth > 0 {
            return tools::Output::error(
                "Only the main agent can leave plan mode; report your plan instead.",
            );
        }
        let plan = args["plan"].as_str().unwrap_or_default().to_string();
        if plan_dismissed(&self.messages) {
            return tools::Output::ok(PLAN_DISMISSED_AGAIN).brief("dismissed");
        }
        if !self.controls.interactive {
            self.controls.plan.set(false);
            self.plan_announced = false;
            return tools::Output::ok(
                "No one is here to approve, so plan mode is off. Carry out the plan.",
            );
        }
        let options = vec!["Approve and start".to_string(), "Keep planning".to_string()];
        let feedback = match self.wait_for_answer(plan, options, true).await {
            Reply::Answered(answer) => answer,
            Reply::Dismissed(notes) => {
                return tools::Output::ok(with_notes(PLAN_DISMISSED, notes)).brief("dismissed");
            }
        };
        match feedback.as_str() {
            "Approve and start" => {
                self.controls.plan.set(false);
                self.plan_announced = false;
                tools::Output::ok("The user approved the plan. Plan mode is off; carry it out now.").brief("approved")
            }
            feedback if feedback != "Keep planning" => {
                tools::Output::ok(format!("The user wants changes to the plan: {feedback}\nStay in plan mode and revise it."))
                    .brief("changes requested")
            }
            _ => tools::Output::ok("The user isn't ready to approve. Stay in plan mode, refine the plan, and ask again.")
                .brief("not yet"),
        }
    }

    async fn wait_for_answer(&self, text: String, options: Vec<String>, plan: bool) -> Reply {
        let _asleep_ok = self.awake.pause();
        let (reply, answer) = oneshot::channel();
        (self.sink)(Event::Ask(Question {
            text,
            options,
            plan,
            reply,
        }));
        Reply::of(answer.await.unwrap_or_default())
    }
}

/// Ends a reply that dismissed its question but carries corrections to earlier answers the
/// user made while it waited.
const NOT_ANSWERED: &str = "(this question was not answered)";

impl Question {
    /// Ends the question unanswered, as Esc does. Corrections to earlier answers made while it
    /// waited still reach the model, and the reply still reads as a dismissal.
    pub fn dismiss(self, corrections: &[String]) {
        let reply = if corrections.is_empty() {
            String::new()
        } else {
            format!("{}\n{NOT_ANSWERED}", corrections.join("\n"))
        };
        let _ = self.reply.send(reply);
    }
}

/// What the user did with a question.
#[derive(Debug, PartialEq)]
enum Reply {
    Answered(String),
    /// Dismissed, with the corrections to earlier answers sent beside it, if any.
    Dismissed(Option<String>),
}

impl Reply {
    /// Reads the text a question's reply carried: empty, or ending in `NOT_ANSWERED`, is a
    /// dismissal.
    fn of(text: String) -> Self {
        if text.trim().is_empty() {
            return Reply::Dismissed(None);
        }
        match text.strip_suffix(NOT_ANSWERED) {
            Some(notes) => {
                Reply::Dismissed(Some(notes.trim().to_string()).filter(|n| !n.is_empty()))
            }
            None => Reply::Answered(text),
        }
    }
}

/// A dismissal's answer to the model, with the corrections that came with it.
fn with_notes(answer: &str, notes: Option<String>) -> String {
    match notes {
        Some(notes) => format!("{answer}\n{notes}"),
        None => answer.to_string(),
    }
}

/// What `exit_plan_mode` answers when the user dismisses the plan (Esc, or an empty reply):
/// they mean "stop", so the model ends its turn and waits rather than asking again.
const PLAN_DISMISSED: &str = "The user dismissed the plan without approving it. Stop now: end your turn with one short line saying the plan awaits their feedback, and don't call exit_plan_mode again until they reply.";

/// The answer to `exit_plan_mode` called again before the user said anything since a
/// dismissal; the plan isn't shown, so a model that misses the first answer can't loop on it.
const PLAN_DISMISSED_AGAIN: &str = "The user already dismissed the plan and hasn't replied since; it wasn't shown again. Stop now: end your turn with one short line saying the plan awaits their feedback, and don't call exit_plan_mode again until they reply.";

/// Whether the last plan was dismissed and the user hasn't spoken since: going back from the
/// end, a dismissal's result comes before any message the user wrote. Reminders, replies and
/// other tool results don't count as the user's reply.
fn plan_dismissed(messages: &[Value]) -> bool {
    for message in messages.iter().rev() {
        let content = &message["content"];
        match message["role"].as_str() {
            Some("tool") => {
                let text = content.as_str().unwrap_or_default();
                if text.starts_with(PLAN_DISMISSED) || text.starts_with(PLAN_DISMISSED_AGAIN) {
                    return true;
                }
            }
            Some("user") => {
                let reminder = content.as_str().is_some_and(|text| {
                    let text = text.trim();
                    text.starts_with("<system-reminder>") && text.ends_with("</system-reminder>")
                });
                if !reminder {
                    return false;
                }
            }
            _ => {}
        }
    }
    false
}

/// The question and choices of an `ask_user_question` call. Malformed arguments are an error
/// for the model to fix, as asking the user a blank question or one with its choices dropped
/// leaves them nothing to answer.
pub(super) fn question_args(args: &Value) -> Result<(String, Vec<String>), String> {
    const SHAPE: &str = "ask_user_question takes `question`, a string, and `options`, an array of 2 to 4 strings. Nothing was shown to the user; call it again.";
    let text = args["question"].as_str().map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(format!("`question` is missing or empty. {SHAPE}"));
    }
    let raw = args["options"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    // An extra array layer around the choices ([["a", "b"]]) loses nothing flattened.
    let items: Vec<&Value> = raw
        .iter()
        .flat_map(|item| match item.as_array() {
            Some(inner) => inner.iter().collect::<Vec<_>>(),
            None => vec![item],
        })
        .collect();
    let options: Vec<String> = items.iter().filter_map(|item| option_text(item)).collect();
    if options.len() < items.len() {
        return Err(format!(
            "Every option must be a string, or an object with a `label`. {SHAPE}"
        ));
    }
    Ok((text.to_string(), options))
}

/// An option as the user sees it: a string, the `label` of an object with its `description`
/// after it, or the same two as XML elements inside a string, whatever their names. The
/// object shape is the one other agents' question tools take. `option` stands in for `label`
/// (the key some models reach for), and a lone `description` is the option's whole text; an
/// object with its own `question` is a nested question, not a choice, and stays an error so
/// nothing is silently dropped.
fn option_text(option: &Value) -> Option<String> {
    if let Some(text) = option.as_str() {
        return match tagged_option(text) {
            Some((label, description)) => choice(label, description),
            None => Some(text.to_string()),
        };
    }
    let field = |key: &str| {
        option[key]
            .as_str()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    };
    if option.get("question").is_some() {
        return None;
    }
    choice(
        field("label").or_else(|| field("option")),
        field("description"),
    )
}

/// A label with its description after it, as one line of text.
fn choice(label: Option<String>, description: Option<String>) -> Option<String> {
    match (label, description) {
        (Some(label), Some(description)) => Some(format!("{label} — {description}")),
        (Some(label), None) => Some(label),
        (None, description) => description,
    }
}

/// The `label` and `description` elements of a choice written as XML, whatever names the model
/// gives them: `<desc>17G</desc><value>All of it</value>`. Only when those elements are the
/// whole string: anything else, a stray `<` or an element of another name included, is the
/// choice's own text.
fn tagged_option(text: &str) -> Option<(Option<String>, Option<String>)> {
    /// Element names that hold a choice's label.
    const LABELS: [&str; 6] = ["option", "label", "text", "value", "title", "name"];
    /// Element names that hold a choice's description.
    const DESCRIPTIONS: [&str; 5] = ["description", "desc", "detail", "details", "summary"];
    let mut rest = text.trim();
    let (mut label, mut description) = (None, None);
    while !rest.is_empty() {
        let after = rest.strip_prefix('<')?;
        let (name, inner) = after.split_once('>')?;
        let (value, tail) = inner.split_once(&format!("</{name}>"))?;
        let value = value.trim().to_string();
        let value = (!value.is_empty()).then_some(value);
        let lower = name.to_ascii_lowercase();
        if LABELS.contains(&lower.as_str()) {
            label = value;
        } else if DESCRIPTIONS.contains(&lower.as_str()) {
            description = value;
        } else {
            return None;
        }
        rest = tail.trim_start();
    }
    (label.is_some() || description.is_some()).then_some((label, description))
}

#[cfg(test)]
mod tests {
    use super::super::tests::idle_agent;
    use super::*;

    fn exit_plan_call(id: &str) -> (String, ToolCall) {
        (
            id.to_string(),
            ToolCall {
                extra_content: None,
                id: id.into(),
                name: "exit_plan_mode".into(),
                arguments: json!({"plan": format!("plan {id}")}).to_string(),
            },
        )
    }

    fn result(agent: &Agent, id: &str) -> String {
        agent
            .messages
            .iter()
            .find(|m| m["tool_call_id"] == id)
            .and_then(|m| m["content"].as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// A user who answers every plan with `reply`, counting the plans shown.
    fn planning_agent(name: &str, reply: &'static str) -> (Agent, Rc<Cell<usize>>) {
        let shown = Rc::new(Cell::new(0));
        let count = shown.clone();
        let sink: Sink = Rc::new(move |event| {
            if let Event::Ask(question) = event {
                count.set(count.get() + 1);
                let _ = question.reply.send(reply.to_string());
            }
        });
        let controls = Controls {
            interactive: true,
            ..Controls::default()
        };
        controls.plan.set(true);
        (idle_agent(name, controls, sink), shown)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_dismissed_plan_stops_the_turn_and_is_not_shown_again_until_the_user_speaks() {
        let (mut agent, shown) = planning_agent("plan-dismissed", "");
        agent
            .execute_calls(vec![exit_plan_call("a")], false)
            .await
            .unwrap();
        assert_eq!(shown.get(), 1);
        let first = result(&agent, "a");
        assert!(first.starts_with(PLAN_DISMISSED), "{first}");
        assert!(first.contains("Stop now"), "{first}");
        assert!(!first.contains("ask again"), "{first}");
        assert!(agent.controls.plan.get(), "plan mode stays on");

        agent
            .push(json!({"role": "user", "content": "<system-reminder>a note</system-reminder>"}))
            .unwrap();
        agent
            .execute_calls(vec![exit_plan_call("b")], false)
            .await
            .unwrap();
        assert_eq!(shown.get(), 1, "a repeat in the same turn isn't shown");
        let again = result(&agent, "b");
        assert!(again.starts_with(PLAN_DISMISSED_AGAIN), "{again}");

        agent
            .push(json!({"role": "user", "content": "try a smaller plan"}))
            .unwrap();
        agent
            .execute_calls(vec![exit_plan_call("c")], false)
            .await
            .unwrap();
        assert_eq!(
            shown.get(),
            2,
            "the user's reply lets the plan be shown again"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn keep_planning_still_asks_for_a_refined_plan() {
        let (mut agent, shown) = planning_agent("plan-keep", "Keep planning");
        for id in ["a", "b"] {
            agent
                .execute_calls(vec![exit_plan_call(id)], false)
                .await
                .unwrap();
        }
        assert_eq!(shown.get(), 2);
        assert!(result(&agent, "b").contains("ask again"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_plan_dismissed_with_a_correction_pending_is_a_dismissal_that_keeps_it() {
        let shown = Rc::new(Cell::new(0));
        let count = shown.clone();
        let sink: Sink = Rc::new(move |event| {
            if let Event::Ask(question) = event {
                count.set(count.get() + 1);
                question.dismiss(&[
                    "Corrected an earlier answer to “Which db?”: now “sqlite” (was “pg”)"
                        .to_string(),
                ]);
            }
        });
        let controls = Controls {
            interactive: true,
            ..Controls::default()
        };
        controls.plan.set(true);
        let mut agent = idle_agent("plan-dismissed-noted", controls, sink);
        agent
            .execute_calls(vec![exit_plan_call("a")], false)
            .await
            .unwrap();
        let first = result(&agent, "a");
        assert!(first.starts_with(PLAN_DISMISSED), "{first}");
        assert!(first.contains("now “sqlite” (was “pg”)"), "{first}");
        assert!(!first.contains("wants changes"), "{first}");
        assert!(agent.controls.plan.get(), "plan mode stays on");
        agent
            .execute_calls(vec![exit_plan_call("b")], false)
            .await
            .unwrap();
        assert_eq!(shown.get(), 1, "it counts as a dismissal: not shown again");
        assert!(result(&agent, "b").starts_with(PLAN_DISMISSED_AGAIN));
    }

    #[test]
    fn a_reply_reads_as_an_answer_or_a_dismissal() {
        assert_eq!(Reply::of("  ".into()), Reply::Dismissed(None));
        assert_eq!(Reply::of("sqlite".into()), Reply::Answered("sqlite".into()));
        let (reply, answer) = oneshot::channel();
        let question = Question {
            text: "q".into(),
            options: Vec::new(),
            plan: false,
            reply,
        };
        question.dismiss(&["fixed one".into(), "fixed two".into()]);
        assert_eq!(
            Reply::of(answer.blocking_recv().unwrap()),
            Reply::Dismissed(Some("fixed one\nfixed two".into()))
        );
        let (reply, answer) = oneshot::channel();
        Question {
            text: "q".into(),
            options: Vec::new(),
            plan: false,
            reply,
        }
        .dismiss(&[]);
        assert_eq!(
            Reply::of(answer.blocking_recv().unwrap()),
            Reply::Dismissed(None)
        );
    }

    #[test]
    fn only_a_dismissal_since_the_users_last_message_counts() {
        let dismissed = json!({"role": "tool", "tool_call_id": "a", "content": PLAN_DISMISSED});
        let reminder = json!({"role": "user", "content": "<system-reminder>x</system-reminder>"});
        let prompt =
            json!({"role": "user", "content": "<system-reminder>x</system-reminder>\n\ngo"});
        let reply = json!({"role": "assistant", "content": "ok"});
        assert!(plan_dismissed(&[
            dismissed.clone(),
            reminder.clone(),
            reply.clone()
        ]));
        assert!(!plan_dismissed(&[dismissed.clone(), prompt.clone()]));
        assert!(!plan_dismissed(&[prompt, reply]));
        assert!(!plan_dismissed(&[
            json!({"role": "tool", "tool_call_id": "r", "content": format!("1: {PLAN_DISMISSED}")})
        ]));
    }
}
