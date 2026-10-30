use super::*;

/// Receives everything the agent does, for a terminal to show.
pub type Sink = Rc<dyn Fn(Event)>;

pub enum Event {
    Text(String),
    Reasoning(String),
    Retry {
        attempt: u32,
        wait: Duration,
        reason: String,
    },
    ToolStart {
        id: String,
        summary: String,
    },
    ToolEnd {
        id: String,
        is_error: bool,
        brief: String,
        display: Option<String>,
        output: String,
    },
    /// After each request: that request's usage, and the context it used.
    Usage(Usage),
    Todos(Vec<Todo>),
    Ask(Question),
    Notice(String),
    /// An `agent_swarm` call began: its tool call id, its description, and each subagent's item
    /// in order. Subagents are told apart from the call's other work by their `Slot`.
    Swarm {
        id: String,
        description: String,
        items: Vec<String>,
    },
    /// A subagent began its work. Only seen inside `Sub`.
    Started,
    /// A subagent ended, and whether it failed. Only seen inside `Sub`.
    Finished {
        is_error: bool,
    },
    /// Something a subagent did, labeled with its description, and where it sits in an
    /// `agent_swarm` call when it belongs to one.
    Sub {
        label: String,
        slot: Option<Slot>,
        event: Box<Event>,
    },
}

/// A subagent's place in an `agent_swarm` call: the call's tool id and its item's index.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    pub swarm: String,
    pub index: usize,
}

/// A question for the user. `plan` marks a plan waiting for approval.
pub struct Question {
    pub text: String,
    pub options: Vec<String>,
    pub plan: bool,
    pub reply: oneshot::Sender<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Todo {
    pub content: String,
    pub status: String,
}

/// An image pasted into a prompt: the `[Image #N]` placeholder that stands for it in the
/// text, and the image as a data URL.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub label: String,
    pub url: String,
}

/// A message typed during a turn, with the images pasted into it.
pub type Steered = (String, Vec<Image>);

/// What the user can change while the agent works, and the conversation they can ask about.
#[derive(Clone, Default)]
pub struct Controls {
    /// Messages typed during a turn, sent at the next step.
    pub steer: Rc<RefCell<VecDeque<Steered>>>,
    pub plan: Rc<Cell<bool>>,
    /// Swarm mode: the agent is told to split its work across `agent_swarm` subagents.
    pub swarm: Rc<Cell<bool>>,
    /// The session goal set with `/goal`, if any: the model is reminded of it, and
    /// ending the session waits until it is done or cleared.
    pub goal: Rc<RefCell<Option<String>>>,
    /// Whether someone can answer questions; headless runs and subagents can't.
    pub interactive: bool,
    /// The conversation as it stands, for /btw while the agent works.
    pub aside: Rc<RefCell<Aside>>,
    /// Where this session works among the live ones, so the model hears about the others in
    /// its tree; none for subagents and with `[agent] peers = false`.
    pub here: Option<Rc<live::Here>>,
    /// Messages from other sessions, as reminders, for the model's next step or prompt.
    pub peer_notes: Rc<RefCell<VecDeque<String>>>,
}

/// What a side question needs: the model, the tools (sent unchanged so the cached prefix
/// hits) and the conversation. It holds the history weakly, so appending to it never copies it.
#[derive(Clone, Default)]
pub struct Aside {
    pub(super) client: Option<Rc<Client>>,
    pub(super) tools: Rc<Vec<Value>>,
    pub(super) messages: Weak<Vec<Value>>,
    /// What the tools are waiting on, for the question to mention.
    pub(super) ctx: Weak<tools::Context>,
    pub(super) cache_key: String,
}

impl Aside {
    /// Asks `question` about the conversation so far without adding to it. The answer and the
    /// request's usage come back; tool calls in the reply are ignored.
    pub fn ask(&self, question: &str) -> impl Future<Output = Result<(String, Usage)>> + 'static {
        self.ask_with(BTW_NOTE, question)
    }

    /// Like `ask`, under another note: for a question from another session.
    pub fn ask_with(
        &self,
        note: &str,
        question: &str,
    ) -> impl Future<Output = Result<(String, Usage)>> + 'static {
        let answer = self.side(note, question, self.waiting().as_deref());
        async move {
            let (text, usage) = answer.await?;
            Ok((
                if text.trim().is_empty() {
                    "(No answer: the model tried to use tools.)".to_string()
                } else {
                    text
                },
                usage,
            ))
        }
    }

    /// A recap of the conversation so far, cut to `RECAP_CHARS`, asked the same way as a side
    /// question so it adds nothing to the conversation and the cached prefix still hits.
    pub fn recap(&self) -> impl Future<Output = Result<(String, Usage)>> + 'static {
        let answer = self.side(RECAP_NOTE, "", None);
        async move {
            let (text, usage) = answer.await?;
            let text = fit_recap(&text);
            if text.is_empty() {
                bail!("the model sent no recap");
            }
            Ok((text, usage))
        }
    }

    /// What the calls in flight are waiting on, as `/btw` is told it.
    pub fn waiting(&self) -> Option<String> {
        self.ctx
            .upgrade()
            .and_then(|ctx| ctx.running.borrow().describe())
    }

    fn side(
        &self,
        note: &str,
        question: &str,
        running: Option<&str>,
    ) -> impl Future<Output = Result<(String, Usage)>> + 'static + use<> {
        let request = self
            .messages
            .upgrade()
            .map(|history| side_request(&history, note, question, running));
        let (client, tools, cache_key) = (
            self.client.clone(),
            self.tools.clone(),
            self.cache_key.clone(),
        );
        async move {
            let (Some(client), Some(request)) = (client, request) else {
                bail!("there is no conversation to ask about yet")
            };
            let reply = client
                .complete(&request, &tools, &cache_key, &mut |_| {})
                .await?;
            Ok((reply.text, reply.usage))
        }
    }
}

/// A recap on one line of whitespace-collapsed text, at most `RECAP_MAX` characters. Models
/// overshoot the `RECAP_CHARS` they are asked for, so the hard limit leaves room, and a recap
/// beyond it keeps the sentences that fit whole; only when not even the first fits is it cut
/// with an ellipsis.
pub(super) fn fit_recap(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= RECAP_MAX {
        return text;
    }
    let ends = |i: usize| {
        matches!(chars[i], '.' | '!' | '?' | '。' | '！' | '？')
            && chars.get(i + 1).is_none_or(|c| *c == ' ' || !c.is_ascii())
    };
    if let Some(end) = (0..RECAP_MAX).rev().find(|&i| ends(i)) {
        return chars[..=end].iter().collect();
    }
    let cut: String = chars[..RECAP_MAX - 1].iter().collect();
    format!("{}…", cut.trim_end())
}

/// The history with a side question at the end. Tool calls still running get a placeholder
/// result first, as providers reject a question between a call and its result; `running` says
/// what they are waiting on and for how long, so "is it stuck?" has facts to go on.
pub(super) fn side_request(
    history: &[Value],
    note: &str,
    question: &str,
    running: Option<&str>,
) -> Vec<Value> {
    let mut request = history.to_vec();
    for id in unanswered(history) {
        request.push(
            json!({"role": "tool", "tool_call_id": id, "content": "Still running; no result yet."}),
        );
    }
    let running = running
        .map(|r| format!("<system-reminder>Waiting on right now:\n{r}</system-reminder>\n\n"))
        .unwrap_or_default();
    request.push(
        json!({"role": "user", "content": format!("{note}\n\n{running}{question}").trim_end()}),
    );
    request
}

/// What the log's last plan reminder said, so a resumed session keeps hearing the mode it
/// ended in instead of announcing a change on its first step.
pub(super) fn last_plan_state(messages: &[Value]) -> bool {
    messages
        .iter()
        .rev()
        .filter(|message| message["role"] == "user")
        .find_map(|message| {
            let text = message["content"].as_str()?;
            if text == plan::ON {
                Some(true)
            } else if text == plan::OFF {
                Some(false)
            } else {
                None
            }
        })
        .unwrap_or(false)
}

/// IDs of the last assistant message's tool calls that have no result yet.
pub(super) fn unanswered(messages: &[Value]) -> Vec<String> {
    let Some(index) = messages.iter().rposition(|m| m["role"] == "assistant") else {
        return Vec::new();
    };
    let answered: HashSet<&str> = messages[index + 1..]
        .iter()
        .filter_map(|m| m["tool_call_id"].as_str())
        .collect();
    messages[index]["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["id"].as_str())
        .filter(|id| !answered.contains(id))
        .map(String::from)
        .collect()
}

#[derive(Clone, Default)]
pub struct Settings {
    /// Compact once billed input, an estimated appended suffix and output reserve pass this:
    /// an explicit limit in tokens, `None` for a share of the model's context window.
    pub compact_at: Option<u64>,
    /// Extra command prefixes bash may run in plan mode.
    pub plan_allow: Vec<String>,
    /// Subagents an `agent_swarm` call runs at once.
    pub swarm_parallel: usize,
    pub search: crate::tools::SearchSettings,
    pub skills: Skills,
    pub hooks: Rc<HooksConfig>,
    pub mcp: Rc<crate::mcp::Mcp>,
    /// Instruction files already in the system prompt.
    pub instruction_files: Vec<std::path::PathBuf>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum Stop {
    #[default]
    Completed,
    MaxTokens,
    RepeatBreaker,
}

/// How often one exact tool call was made, and a hash of what its latest result said.
#[derive(Default)]
pub(super) struct Repeat {
    count: u32,
    result: u64,
}

impl Repeat {
    /// Counts another identical call. Returns how many there have been, and whether this one
    /// got the same result as the one before it.
    pub(super) fn note(&mut self, result: u64) -> (u32, bool) {
        let same = self.count > 0 && self.result == result;
        self.count += 1;
        self.result = result;
        (self.count, same)
    }
}

#[derive(Default)]
pub struct Stats {
    pub requests: u32,
    pub tool_calls: u32,
    pub usage: Usage,
    /// Prompt tokens of the first request: the system prompt, tools and first message.
    pub first_prompt: Option<u64>,
    /// Input tokens of the latest request: how full the context is.
    pub context: u64,
}

/// An in-flight `agent_swarm` call's results so far: its tool-call id, its item labels,
/// and each finished subagent's `(text, is_error)` at its item's index.
pub(super) struct PartialSwarm {
    pub(super) id: String,
    pub(super) items: Vec<String>,
    pub(super) results: Vec<Option<(String, bool)>>,
}

/// Length `RECAP_NOTE` asks the model to keep a recap to, in characters.
#[cfg(test)]
pub(super) const RECAP_CHARS: usize = 100;

/// Longest recap shown, in characters; see `fit_recap`.
pub(super) const RECAP_MAX: usize = 160;

pub(super) const RECAP_NOTE: &str = "<system-reminder>The user is back at this session and wants a recap. Reply with the recap only: what they are after, what was done, and the latest result or what is left. Use their language, at most 100 characters, one or two plain sentences, no markdown. Don't call tools. This request and your reply won't be kept in the conversation.</system-reminder>";

pub(super) const BTW_NOTE: &str = "<system-reminder>A side question from the user. Answer it briefly from what this conversation already shows. Don't call tools. The question and your answer won't be kept in the conversation.</system-reminder>";
