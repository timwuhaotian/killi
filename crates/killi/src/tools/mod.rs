//! The model's tools. Definitions are built once per session and never change, so the
//! cached prompt prefix survives.

mod bash;
mod edit;
mod files;
mod search;
mod web;

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub use bash::Tasks;

pub const MAX_LINES: usize = 2000;
pub const MAX_BYTES: usize = 50 * 1024;

pub struct Context {
    pub cwd: PathBuf,
    /// Where full outputs go when a result is truncated.
    pub spill_dir: PathBuf,
    pub tasks: RefCell<Tasks>,
    /// Whether the model can see images.
    pub images: bool,
}

impl Context {
    pub fn new(cwd: PathBuf, spill_dir: PathBuf) -> Self {
        Self { cwd, spill_dir, tasks: RefCell::new(Tasks::default()), images: true }
    }
}

pub struct Output {
    pub text: String,
    pub is_error: bool,
    /// A few words for the terminal, such as "42 lines" or "exit 1". Never sent to the model.
    pub brief: String,
    /// Extra lines for the terminal, such as an edit's diff. Never sent to the model.
    pub display: Option<String>,
    /// An image for the model, as a data URL, sent after the tool results.
    pub image: Option<String>,
}

impl Output {
    pub fn ok(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: false, brief: String::new(), display: None, image: None }
    }
    pub fn error(text: impl Into<String>) -> Self {
        let text = text.into();
        let first = text.lines().next().unwrap_or_default();
        let brief = match crate::llm::truncate(first, 80) {
            short if short.len() == first.len() => short.to_string(),
            short => format!("{}…", short.rsplit_once(' ').map_or(short, |(head, _)| head)),
        };
        Self { text, is_error: true, brief, display: None, image: None }
    }
    pub fn brief(mut self, brief: impl Into<String>) -> Self {
        self.brief = brief.into();
        self
    }
}

/// Runs the tools that need nothing but the working directory; the agent handles the rest.
pub async fn run(ctx: &Context, name: &str, args: Value) -> Output {
    match name {
        "read" => files::read(ctx, &args),
        "write" => files::write(ctx, &args),
        "edit" => edit::edit(ctx, &args),
        "bash" => bash::bash(ctx, &args).await,
        "task_output" => bash::task_output(ctx, &args).await,
        "task_stop" => bash::task_stop(ctx, &args),
        "grep" => search::grep(ctx, args).await,
        "glob" => search::glob(ctx, args).await,
        "fetch_url" => web::fetch_url(&args).await,
        _ => Output::error(format!("Unknown tool `{name}`.")),
    }
}

/// One line for the terminal: what the call touches, with paths relative to `cwd` and a
/// leading `cd <cwd> &&` left out.
pub fn summary(name: &str, args: &Value, cwd: &Path) -> String {
    let field = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or_default();
    let cwd = cwd.display().to_string();
    let detail = match name {
        "read" | "write" | "edit" => field("path").to_string(),
        "bash" => {
            let command = field("command").lines().next().unwrap_or_default();
            let command = [format!("cd {cwd} && "), format!("cd \"{cwd}\" && "), format!("cd '{cwd}' && ")]
                .iter()
                .find_map(|prefix| command.strip_prefix(prefix.as_str()))
                .unwrap_or(command);
            command.to_string()
        }
        "grep" => format!("{:?} {}", field("pattern"), field("path")),
        "glob" => format!("{} {}", field("pattern"), field("path")),
        "task_output" | "task_stop" => format!("task {}", args.get("id").map(Value::to_string).unwrap_or_default()),
        "fetch_url" => field("url").to_string(),
        "agent" => field("description").to_string(),
        "agent_swarm" => format!("{} ({} items)", field("description"), args["items"].as_array().map_or(0, Vec::len)),
        "skill" => field("name").to_string() + field("query"),
        "mcp" => format!("{} {}", field("server"), field("tool")),
        "ask_user_question" => field("question").to_string(),
        _ => String::new(),
    };
    let detail = detail.replace(&format!("{cwd}/"), "").replace(&cwd, ".");
    format!("{name} {}", crate::llm::truncate(&detail, 120)).trim_end().to_string()
}

fn tool(name: &str, description: &str, parameters: Value) -> Value {
    json!({"type": "function", "function": {"name": name, "description": description, "parameters": parameters}})
}

/// Every tool the model always has. The optional `skill` and `mcp` tools are added by the
/// agent when the session starts with skills or MCP servers.
pub fn definitions() -> Vec<Value> {
    vec![
        tool(
            "read",
            "Read a file. Text lines are prefixed `N\\t`; shows up to 2000 lines or 50KB, continue with offset. Images (png, jpg, gif, webp) are shown to you.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "description": "1-based first line"},
                "limit": {"type": "integer", "description": "Max lines"}
            }, "required": ["path"]}),
        ),
        tool(
            "write",
            "Create or overwrite a file. Use only for new files or complete rewrites; prefer edit.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            }, "required": ["path", "content"]}),
        ),
        tool(
            "edit",
            "Replace exact text in a file. Every old_string is matched against the original file, must be unique unless replace_all, and must not overlap another edit. Copy text from read without the `N\\t` prefix; keep it short but unique. Put several changes to one file in one call.",
            json!({"type": "object", "properties": {
                "path": {"type": "string"},
                "edits": {"type": "array", "items": {"type": "object", "properties": {
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"},
                    "replace_all": {"type": "boolean"}
                }, "required": ["old_string", "new_string"]}}
            }, "required": ["path", "edits"]}),
        ),
        tool(
            "bash",
            "Run a bash command in the working directory; each call is a fresh shell, stdin closed, stderr merged into stdout. Long output keeps its tail and saves the rest to a file. A command still running at the timeout moves to the background.",
            json!({"type": "object", "properties": {
                "command": {"type": "string"},
                "timeout": {"type": "integer", "description": "Seconds, default 60, max 300"},
                "run_in_background": {"type": "boolean", "description": "For servers and watchers: return at once with a task id"}
            }, "required": ["command"]}),
        ),
        tool(
            "task_output",
            "Read new output of a background task and whether it finished.",
            json!({"type": "object", "properties": {
                "id": {"type": "integer"},
                "wait": {"type": "integer", "description": "Seconds to wait for it to finish, max 300"}
            }, "required": ["id"]}),
        ),
        tool(
            "task_stop",
            "Stop a background task.",
            json!({"type": "object", "properties": {"id": {"type": "integer"}}, "required": ["id"]}),
        ),
        tool(
            "grep",
            "Search file contents with a regex (Rust syntax), honoring .gitignore. Returns `path:line: text`.",
            json!({"type": "object", "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "File or directory, default working directory"},
                "glob": {"type": "string", "description": "Only files matching, e.g. *.rs"},
                "ignore_case": {"type": "boolean"},
                "limit": {"type": "integer", "description": "Max matches, default 100"}
            }, "required": ["pattern"]}),
        ),
        tool(
            "glob",
            "Find files by glob pattern (e.g. **/*.ts), honoring .gitignore.",
            json!({"type": "object", "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "Directory, default working directory"}
            }, "required": ["pattern"]}),
        ),
        tool(
            "fetch_url",
            "Fetch a web page or file over HTTP(S) and return its main text as Markdown.",
            json!({"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}),
        ),
        tool(
            "todo_write",
            "Replace your visible task list. Use it for work with several steps; keep exactly one item in_progress.",
            json!({"type": "object", "properties": {"todos": {"type": "array", "items": {"type": "object", "properties": {
                "content": {"type": "string"},
                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
            }, "required": ["content", "status"]}}}, "required": ["todos"]}),
        ),
        tool(
            "ask_user_question",
            "Ask the user a multiple-choice question and wait for the answer. Only when you can't decide from the code or the request.",
            json!({"type": "object", "properties": {
                "question": {"type": "string"},
                "options": {"type": "array", "items": {"type": "string"}, "description": "2 to 4 choices; the user can also type their own"}
            }, "required": ["question", "options"]}),
        ),
        tool(
            "agent",
            "Run a subagent on a self-contained task and get its final report. It has your tools and model. Several calls in one reply run in parallel. fork: true gives it this whole conversation; otherwise it sees only the prompt, so make the prompt complete.",
            json!({"type": "object", "properties": {
                "description": {"type": "string", "description": "3-5 words"},
                "prompt": {"type": "string"},
                "fork": {"type": "boolean"}
            }, "required": ["description", "prompt"]}),
        ),
        tool(
            "agent_swarm",
            "Run many subagents on the same kind of task over different inputs: each item fills {{item}} in prompt_template, and each subagent sees only its own prompt, with your tools and model. Up to 128 items; they queue and several run at once. Returns every report in item order. For a few differently shaped tasks, make several agent calls instead.",
            json!({"type": "object", "properties": {
                "description": {"type": "string", "description": "3-5 words"},
                "prompt_template": {"type": "string", "description": "Contains {{item}}"},
                "items": {"type": "array", "items": {"type": "string"}, "description": "2 to 128, each giving a distinct prompt"}
            }, "required": ["description", "prompt_template", "items"]}),
        ),
        tool(
            "exit_plan_mode",
            "In plan mode, present your finished plan for the user to approve before you change anything.",
            json!({"type": "object", "properties": {"plan": {"type": "string", "description": "Markdown"}}, "required": ["plan"]}),
        ),
    ]
}

pub fn resolve(ctx: &Context, path: &str) -> PathBuf {
    let path = match path.strip_prefix("~/") {
        Some(rest) => crate::config::home().join(rest),
        None => PathBuf::from(path),
    };
    if path.is_absolute() { path } else { ctx.cwd.join(path) }
}

pub fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8000)].contains(&0)
}

/// Writes `text` to a new file in the spill directory and returns its path.
pub fn spill(ctx: &Context, stem: &str, text: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(&ctx.spill_dir).ok()?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let path = ctx.spill_dir.join(format!("{stem}-{nanos}.txt"));
    std::fs::write(&path, text).ok()?;
    Some(path)
}
