use super::*;

impl Agent {
    /// Notes something for later, or crosses an item off, in the project's `later.md`.
    pub(super) fn later(&mut self, args: &Value) -> tools::Output {
        if let Some(number) = crate::later::done_arg(args) {
            return match crate::later::done(&self.ctx.cwd, number) {
                Ok(item) => tools::Output::ok(format!("Crossed off {number}: {item}")).brief("done"),
                Err(e) => tools::Output::error(format!("{e:#}")),
            };
        }
        let Some(note) = args["note"].as_str().map(str::trim).filter(|note| !note.is_empty()) else {
            return tools::Output::error("later needs `note` (what, and where) or `done` (an item's number).");
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
        if let Some(bad) = todos.iter().find(|t| !matches!(t.status.as_str(), "pending" | "in_progress" | "completed")) {
            return tools::Output::error(format!("Unknown status `{}`; use pending, in_progress or completed.", bad.status));
        }
        self.todos = todos.clone();
        let open = todos.iter().filter(|t| t.status != "completed").count();
        (self.sink)(Event::Todos(todos));
        tools::Output::ok(format!("Task list updated: {open} open, {} done.", self.todos.len() - open))
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
            Some(answer) => tools::Output::ok(format!("The user answered: {answer}")).brief(answer),
            None => tools::Output::ok("The question was dismissed without an answer.").brief("dismissed"),
        }
    }

    pub(super) async fn exit_plan(&mut self, args: &Value) -> tools::Output {
        if !self.controls.plan.get() {
            return tools::Output::error("Plan mode is off; go ahead without exit_plan_mode.");
        }
        if self.depth > 0 {
            return tools::Output::error("Only the main agent can leave plan mode; report your plan instead.");
        }
        let plan = args["plan"].as_str().unwrap_or_default().to_string();
        if !self.controls.interactive {
            self.controls.plan.set(false);
            self.plan_announced = false;
            return tools::Output::ok("No one is here to approve, so plan mode is off. Carry out the plan.");
        }
        let options = vec!["Approve and start".to_string(), "Keep planning".to_string()];
        match self.wait_for_answer(plan, options, true).await.as_deref() {
            Some("Approve and start") => {
                self.controls.plan.set(false);
                self.plan_announced = false;
                tools::Output::ok("The user approved the plan. Plan mode is off; carry it out now.").brief("approved")
            }
            Some(feedback) if feedback != "Keep planning" => {
                tools::Output::ok(format!("The user wants changes to the plan: {feedback}\nStay in plan mode and revise it."))
                    .brief("changes requested")
            }
            _ => tools::Output::ok("The user isn't ready to approve. Stay in plan mode, refine the plan, and ask again.")
                .brief("not yet"),
        }
    }

    async fn wait_for_answer(&self, text: String, options: Vec<String>, plan: bool) -> Option<String> {
        let (reply, answer) = oneshot::channel();
        (self.sink)(Event::Ask(Question { text, options, plan, reply }));
        answer.await.ok().filter(|a| !a.trim().is_empty())
    }
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
    let items = args["options"].as_array().map(Vec::as_slice).unwrap_or_default();
    let options: Vec<String> = items.iter().filter_map(option_text).collect();
    if options.len() < items.len() {
        return Err(format!("Every option must be a string, or an object with a `label`. {SHAPE}"));
    }
    Ok((text.to_string(), options))
}

/// An option as the user sees it: a string, or the `label` of an object with its `description`
/// after it, which is the shape other agents' question tools take.
fn option_text(option: &Value) -> Option<String> {
    if let Some(text) = option.as_str() {
        return Some(text.to_string());
    }
    let label = option["label"].as_str().map(str::trim).filter(|l| !l.is_empty())?;
    Some(match option["description"].as_str().map(str::trim).filter(|d| !d.is_empty()) {
        Some(description) => format!("{label} — {description}"),
        None => label.to_string(),
    })
}
