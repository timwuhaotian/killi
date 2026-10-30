use super::*;

/// A /btw question and its answer on the way.
pub(super) type PendingAside = (String, Pin<Box<dyn Future<Output = Result<(String, Usage)>>>>);

/// A recap on its way after a turn that used tools; dropped, which stops the request, when the
/// user sends anything else.
pub(super) type PendingRecap = Pin<Box<dyn Future<Output = Result<(String, Usage)>>>>;

/// The local SearXNG start `/web-search-config` began with Docker, ending in the instance's
/// URL or the reason it failed.
pub(super) type PendingSearxng = Pin<Box<dyn Future<Output = std::result::Result<String, String>>>>;

/// A question from another session on its way to an answer from this conversation.
pub(super) type PendingPeerAsk = Pin<Box<dyn Future<Output = PeerAnswer>>>;

/// Another session's question, answered or not, and where the answer goes.
pub(super) struct PeerAnswer {
    pub(super) id: u64,
    pub(super) from: Option<u32>,
    pub(super) question: String,
    pub(super) answer: Result<(String, Usage)>,
    pub(super) reply: oneshot::Sender<Value>,
}

/// What `run_turn` ended with.
pub(super) enum Ended {
    Done(Result<(String, Stop)>),
    Interrupted,
}

/// Session totals before and after a turn, to report what the turn cost.
#[derive(Clone, Copy)]
pub(super) struct Totals {
    pub(super) requests: u32,
    pub(super) tool_calls: u32,
    pub(super) prompt: u64,
    pub(super) cached: u64,
    pub(super) completion: u64,
}

impl Totals {
    pub(super) fn of(agent: &Agent) -> Self {
        let usage = &agent.stats.usage;
        Self { requests: agent.stats.requests, tool_calls: agent.stats.tool_calls, prompt: usage.prompt, cached: usage.cached, completion: usage.completion }
    }
}

pub(super) enum Input {
    /// A message to the model, with the images pasted into it.
    Prompt(String, Vec<Image>),
    /// A command with its argument and the images pasted into it.
    Command(String, String, Vec<Image>),
    Resume(PathBuf),
    Model(String),
    /// The model every session falls back to on a rate limit: `/backup-model <alias>`, or
    /// `none` to drop it. Saved in the config, so every session starts on it.
    BackupModel(String),
    Effort(String),
    /// Turns an MCP server on or off; true when picked from the /mcp list, which then reopens.
    Mcp(String, bool),
    /// Deletes every session but the newest this many and the current one.
    Prune(usize),
    /// Applies a /web-search-config pick: the provider, the SearXNG instance and the key.
    Search(String, Option<String>, Option<String>),
    /// Applies a /provider pick: the preset's name and its pasted API key.
    Provider(String, String),
}

pub(super) enum LastEnd {
    Done,
    Interrupted,
}

pub(super) struct Running {
    pub(super) id: String,
    pub(super) summary: String,
    pub(super) label: Option<String>,
    pub(super) started: Instant,
}

pub(super) struct Asking {
    pub(super) question: Question,
    pub(super) selected: usize,
    /// The /web-search-config flow asked it, not the model: `answer` routes it back there.
    pub(super) internal: bool,
    /// The answer is a secret, so the echo shows that it was set, not what it is.
    pub(super) secret: bool,
}

/// A model question already answered in this turn, kept so ← can revisit it while the
/// next question waits. Its answer already went to the model; changing it here records a
/// correction in `App::ask_corrections`, sent with the waiting question's answer.
pub(super) struct PastAsk {
    pub(super) text: String,
    pub(super) options: Vec<String>,
    pub(super) selected: usize,
    pub(super) answer: String,
}

#[derive(Default)]
pub(super) struct Status {
    pub(super) model: String,
    /// The model this session falls back to on a rate limit, if one is set.
    pub(super) backup: Option<String>,
    /// The thinking effort, as `Thinking::label` names it.
    pub(super) thinking: String,
    pub(super) window: Option<u64>,
    pub(super) context: u64,
    pub(super) branch: Option<String>,
    pub(super) prompt: u64,
    pub(super) cached: u64,
    pub(super) completion: u64,
}

pub(super) struct App {
    pub(super) out: Stdout,
    pub(super) width: usize,
    pub(super) height: usize,
    /// Rows from the top of the live area to the cursor, to find the area again.
    pub(super) cursor_row: usize,
    /// The live area's rows and cursor column as last painted, so a tick repaints only what
    /// changed and the rows that didn't, such as the status line, never blink.
    pub(super) shown: Vec<String>,
    pub(super) shown_col: usize,
    /// Lines waiting to be printed above the live area.
    pub(super) pending: Vec<String>,
    pub(super) editor: Editor,
    pub(super) markdown: Markdown,
    /// Streamed text after the last line break.
    pub(super) partial: String,
    /// The streaming preview's rows for the unfinished tail, with the tail, the width and the
    /// Markdown state they were styled from: every tick redraws while it streams otherwise.
    pub(super) preview_cache: Option<(String, usize, u64, Vec<String>)>,
    /// The streaming reply's sanitizer: an escape sequence a delta ends in the middle of waits
    /// in it for the next one.
    pub(super) stream: text::Sanitizer,
    /// The reasoning stream's sanitizer, one thought at a time.
    pub(super) reasoning: text::Sanitizer,
    /// A blank line in the model's text, printed only if more text follows it.
    pub(super) held_blank: bool,
    /// The last line printed was blank.
    pub(super) last_blank: bool,
    pub(super) last_kind: Kind,
    pub(super) thinking: Option<(Instant, String)>,
    pub(super) running: Vec<Running>,
    /// The `agent_swarm` call in flight, whose subagents the live area lists.
    pub(super) swarm: Option<swarm::Swarm>,
    pub(super) todos: Vec<Todo>,
    pub(super) asking: Option<Asking>,
    /// This turn's answered model questions, oldest first, for ←/→ navigation.
    pub(super) ask_past: Vec<PastAsk>,
    /// The `ask_past` index on screen instead of the waiting question, when revisiting.
    pub(super) ask_view: Option<usize>,
    /// Corrections to answers already sent, delivered with the waiting question's answer.
    pub(super) ask_corrections: Vec<String>,
    pub(super) picker: Option<Picker>,
    /// A /web-search-config flow between its questions.
    pub(super) search: Option<SearchFlow>,
    /// A /provider flow between its questions: the preset being added.
    pub(super) provider: Option<ProviderFlow>,
    /// The SearXNG instance Docker is starting for that flow, when one is.
    pub(super) searxng: Option<PendingSearxng>,
    /// Input to act on before reading keys again, such as `/model <alias>`.
    pub(super) next_input: Option<Input>,
    /// An effort picked while the agent works; it applies once the turn ends.
    pub(super) effort_later: Option<String>,
    /// What the slash menu searches, and the choices it offers after /model and /effort.
    pub(super) entries: Vec<Entry>,
    /// The session's skills, so a `/name` sent while the agent works can expand one and queue
    /// its prompt; refreshed by /reload.
    pub(super) skills: crate::skills::Skills,
    pub(super) model_choices: Vec<Item>,
    pub(super) effort_choices: Vec<Item>,
    pub(super) menu_selected: usize,
    /// The text the menu's selection belongs to; another text starts it over.
    pub(super) menu_text: String,
    /// The text the menu was closed on with Esc; it opens again when the text changes.
    pub(super) menu_closed: Option<String>,
    /// The workspace's paths the `@` menu searches, read when an `@` word is at the cursor
    /// and dropped when none is, so each `@` sees the files as they are then.
    pub(super) files: Option<Vec<String>>,
    /// Bumped whenever `files` is read or dropped, so the `@` menu's cached filter knows
    /// the listing it was computed from.
    pub(super) files_gen: u64,
    /// The `@` menu for the text before the cursor and the listing it was filtered from:
    /// filtering 20k paths reruns on every draw while streaming otherwise.
    pub(super) file_menu_cache: Option<(String, u64, Option<Suggestions>)>,
    pub(super) busy: Option<Instant>,
    /// A request being retried: the attempt and when its wait ends, for the working row.
    pub(super) retry: Option<(u32, Instant)>,
    /// A snapshot started with the turn still runs on a blocking thread, for the working row.
    pub(super) snapshotting: bool,
    /// How many snapshots have started: the snapshot word advances one per turn.
    pub(super) snapshot_seq: usize,
    /// The latest tool outputs and thoughts, for Ctrl+O.
    pub(super) details: Vec<(String, String)>,
    /// How far back Ctrl+O has gone from the newest.
    pub(super) detail_back: usize,
    /// The first row that left output out has said how to read it; the rest don't repeat it.
    pub(super) ctrl_o_taught: bool,
    pub(super) status: Status,
    pub(super) controls: Controls,
    pub(super) spinner: usize,
    pub(super) exit_armed: Option<Instant>,
    pub(super) queued: Vec<String>,
    /// Ctrl+S ended the running turn to send the queued messages at once.
    pub(super) send_now: bool,
    /// A /btw question for the running turn to send.
    pub(super) aside_asked: Option<String>,
    /// The running turn called `later`, so a reply that leaves something for later noted it.
    pub(super) later_noted: bool,
    /// The /btw question being answered and since when, for the live area.
    pub(super) aside: Option<(String, Instant)>,
    /// Answered /btw blocks waiting for the model's paragraph to end before they are printed.
    pub(super) held_asides: Vec<Line>,
    /// Whether a turn that used tools is followed by a recap.
    pub(super) auto_recap: bool,
    pub(super) recap: Option<PendingRecap>,
    pub(super) last_end: Option<LastEnd>,
    pub(super) cwd: PathBuf,
    pub(super) notify_after: Duration,
    /// Whether the model takes images, so a pasted one is attached or turned away.
    pub(super) sees_images: bool,
    /// The banner and what was printed after it, while its top row is on screen.
    pub(super) since_banner: Option<SinceBanner>,
    /// Screen rows above the live area that hold content, the shell's lines above killi's start
    /// included. While the conversation is shorter than the screen, the live area is padded
    /// above so it ends on the bottom row. A full screen when unknown, as after a resize.
    pub(super) content_rows: usize,
    /// Fullscreen mode: the conversation is kept in `history`, not in the terminal's scrollback.
    pub(super) fullscreen: bool,
    pub(super) history: History,
    /// The rows the conversation had on screen at the last paint, for PageUp and PageDown.
    pub(super) view_rows: usize,
    /// Fullscreen mode: the text a drag selected in the conversation, if any.
    pub(super) selection: Option<Selection>,
    /// The pointer's (row, column) while the button is held after a press in the conversation.
    pub(super) drag: Option<(usize, usize)>,
    /// The conversation point of the last left press, when it landed and which press of a run of
    /// presses on it this is (capped at 3), for double- and triple-clicks.
    pub(super) pressed: Option<((usize, usize), Instant, usize)>,
    /// Whether the banner says it is a resumed session, for drawing it again.
    pub(super) banner_resumed: bool,
    /// When the branch was last read, so two git spawns don't run after every input.
    pub(super) branch_at: Option<Instant>,
    /// This session's file among the live sessions; none with `[agent] peers = false`.
    pub(super) live: Option<live::Registration>,
    /// Requests from other sessions; none when this one takes none.
    pub(super) peers: Option<mpsc::UnboundedReceiver<live::Incoming>>,
    /// Questions from other sessions being answered, and what the live area shows of each:
    /// its id, who asked, the question and since when.
    pub(super) peer_asks: FuturesUnordered<PendingPeerAsk>,
    pub(super) peer_asking: Vec<(u64, String, String, Instant)>,
    pub(super) peer_ask_id: u64,
    /// Messages from other sessions taken since the last prompt.
    pub(super) peer_tells: usize,
    /// /undo said it would undo another session's work too; the next /undo goes ahead.
    pub(super) undo_warned: bool,
}

/// The lines printed from the banner on, so it can be drawn again in place.
#[derive(Default)]
pub(super) struct SinceBanner {
    /// How many of `lines` the banner itself is.
    pub(super) banner: usize,
    pub(super) lines: Vec<String>,
    /// The screen rows `lines` take.
    pub(super) rows: usize,
    pub(super) resumed: bool,
}

#[derive(PartialEq, Clone, Copy)]
pub(super) enum Kind {
    None,
    Text,
    /// Tool calls, thoughts and retries: the work between prose.
    Tool,
    /// A notice, an error, a question or a block like /help: set apart from what is around it.
    Note,
}

pub(super) enum Idle {
    Continue,
    Exit,
    Input(Input),
}
