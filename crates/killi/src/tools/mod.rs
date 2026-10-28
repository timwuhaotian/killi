//! The model's tools. Definitions are built once per session and never change, so the
//! cached prompt prefix survives.

mod bash;
mod edit;
mod files;
mod search;
mod web;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub use bash::{OwnSession, Running, Tasks, Tracked, kill_all, track};
#[cfg(test)]
pub(crate) use bash::registered;
pub use files::{image_type, image_url};
pub use web::{CONTAINER, start_searxng};

pub const MAX_LINES: usize = 2000;
pub const MAX_BYTES: usize = 50 * 1024;

/// Which service `web_search` asks, from `[search]` in the config.
#[derive(Clone, Default)]
pub struct SearchSettings {
    /// "brave" (the default), "tavily" or "searxng".
    pub provider: String,
    pub api_key: Option<String>,
    /// The SearXNG instance; only "searxng" reads it.
    pub url: Option<String>,
}

pub struct Context {
    pub cwd: PathBuf,
    /// Where full outputs go when a result is truncated.
    pub spill_dir: PathBuf,
    pub tasks: RefCell<Tasks>,
    /// The calls waiting on a command now.
    pub running: RefCell<Running>,
    /// Whether the model can see images.
    pub images: Cell<bool>,
    /// Which service `web_search` asks; /web-search-config swaps it while the session runs.
    pub search: RefCell<SearchSettings>,
    /// Test-only scratch folder removed when the context drops, so `cargo test` leaves
    /// no `killi-*` folders behind in the temporary folder.
    #[cfg(test)]
    pub temp: Option<crate::test_temp::TempDir>,
}

impl Context {
    pub fn new(cwd: PathBuf, spill_dir: PathBuf) -> Self {
        Self {
            cwd,
            spill_dir,
            tasks: RefCell::new(Tasks::default()),
            running: RefCell::default(),
            images: Cell::new(true),
            search: RefCell::default(),
            #[cfg(test)]
            temp: None,
        }
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
pub async fn run(ctx: &Context, name: &str, args: Value, wake: bash::Wake<'_>) -> Output {
    match name {
        "read" => files::read(ctx, &args),
        "write" => files::write(ctx, &args),
        "edit" => edit::edit(ctx, &args),
        "bash" => bash::bash(ctx, &args, wake).await,
        "task_output" => bash::task_output(ctx, &args, wake).await,
        "task_stop" => bash::task_stop(ctx, &args),
        "grep" => search::grep(ctx, args).await,
        "glob" => search::glob(ctx, args).await,
        "fetch_url" => web::fetch_url(ctx, &args).await,
        "web_search" => web::web_search(ctx, &args).await,
        _ => Output::error(format!("Unknown tool `{name}`.")),
    }
}

/// A `u64` tool argument, whether the model sent a number or a string holding one.
pub fn uint(args: &Value, key: &str) -> Option<u64> {
    match args.get(key) {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

/// One line for the terminal: what the call touches, with paths relative to `cwd` and a
/// leading `cd <cwd> &&` left out.
/// `n` and `noun`, the noun made plural unless `n` is 1: "1 file", "3 files".
pub fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

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
            match field("cwd") {
                "" => command.to_string(),
                dir => format!("[{dir}] {command}"),
            }
        }
        "grep" => format!("{:?} {}", field("pattern"), field("path")),
        "glob" => format!("{} {}", field("pattern"), field("path")),
        "task_output" | "task_stop" => format!("task {}", args.get("id").map(Value::to_string).unwrap_or_default()),
        "fetch_url" => field("url").to_string(),
        "web_search" => field("query").to_string(),
        "agent" => field("description").to_string(),
        "agent_swarm" => format!("{} ({})", field("description"), count(args["items"].as_array().map_or(0, Vec::len), "item")),
        "skill" => field("name").to_string() + field("query"),
        "mcp" => format!("{} {}", field("server"), field("tool")),
        "ask_user_question" => field("question").to_string(),
        _ => String::new(),
    };
    let detail = strip_cwd(&detail, &cwd);
    format!("{name} {}", crate::llm::truncate(&detail, 120)).trim_end().to_string()
}

/// `cwd` out of a summary, only as a path prefix at a `/` boundary, so `/x/killi-old`
/// survives intact next to `/x/killi`, and a `/` root leaves `etc/hosts`, not `.etc.hosts`.
fn strip_cwd(detail: &str, cwd: &str) -> String {
    if detail == cwd {
        return ".".to_string();
    }
    if cwd == "/" {
        return detail.strip_prefix('/').unwrap_or(detail).to_string();
    }
    detail.strip_prefix(&format!("{cwd}/")).unwrap_or(detail).to_string()
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
                "cwd": {"type": "string", "description": "Directory to run in, absolute or relative to the working directory"},
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
            "Search file contents with a regex (Rust syntax), honoring .gitignore. Returns `path:line: text`; context lines use `-`.",
            json!({"type": "object", "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "File or directory, default working directory"},
                "glob": {"type": "string", "description": "Only files matching, e.g. *.rs"},
                "ignore_case": {"type": "boolean"},
                "context": {"type": "integer", "description": "Lines before and after each match"},
                "output": {"type": "string", "enum": ["content", "files", "count"], "description": "files: only paths; count: matches per file"},
                "multiline": {"type": "boolean", "description": "Let the pattern span lines (. matches newline)"},
                "limit": {"type": "integer", "description": "Max matches (files in files/count mode), default 100"}
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
            "Fetch a web page or file over HTTP(S) and return its main text as Markdown. Pages that need JavaScript are rendered in a headless browser automatically; GitHub blob URLs fetch the raw file.",
            json!({"type": "object", "properties": {
                "url": {"type": "string"},
                "render": {"type": "boolean", "description": "Run the page's JavaScript first"},
                "screenshot": {"type": "boolean", "description": "Return a picture of the page instead; to check a local dev server's UI"},
                "raw": {"type": "boolean", "description": "Return the body as is (HTML, JSON) instead of extracted text"}
            }, "required": ["url"]}),
        ),
        tool(
            "web_search",
            "Search the web; returns titles, URLs and snippets. Read a result with fetch_url.",
            json!({"type": "object", "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer", "description": "Default 8, max 20"}
            }, "required": ["query"]}),
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
            "later",
            "Note something you won't do now: a blocker, a bug seen in passing, an improvement. Call it the moment it comes up, saying where; never keep it for the final message. `done` with an item's number crosses it off.",
            json!({"type": "object", "properties": {
                "note": {"type": "string"},
                "done": {"type": "integer", "description": "An item's number, to remove it"}
            }}),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_numbers_parse_where_a_u64_is_expected() {
        assert_eq!(uint(&json!({"n": 300}), "n"), Some(300));
        assert_eq!(uint(&json!({"n": "300"}), "n"), Some(300));
        assert_eq!(uint(&json!({"n": " 300 "}), "n"), Some(300));
        assert_eq!(uint(&json!({}), "n"), None);
        assert_eq!(uint(&json!({"n": true}), "n"), None);
        assert_eq!(uint(&json!({"n": "many"}), "n"), None);
        assert_eq!(uint(&json!({"n": "-3"}), "n"), None);
        assert_eq!(uint(&json!({"n": 1.5}), "n"), None);
    }

    #[test]
    fn a_bash_summary_shows_where_it_ran_and_drops_a_leading_cd() {
        let cwd = Path::new("/w/app");
        assert_eq!(summary("bash", &json!({"command": "cd /w/app && cargo test"}), cwd), "bash cargo test");
        assert_eq!(summary("bash", &json!({"command": "git status", "cwd": "../other"}), cwd), "bash [../other] git status");
    }

    #[test]
    fn a_summary_strips_the_cwd_only_as_a_path_prefix() {
        let cwd = Path::new("/x/killi");
        assert_eq!(summary("read", &json!({"path": "/x/killi/src/a.rs"}), cwd), "read src/a.rs");
        assert_eq!(summary("read", &json!({"path": "/x/killi-old/src/a.rs"}), cwd), "read /x/killi-old/src/a.rs");
        assert_eq!(summary("read", &json!({"path": "/x/killi"}), cwd), "read .");
        let root = Path::new("/");
        assert_eq!(summary("read", &json!({"path": "/etc/hosts"}), root), "read etc/hosts");
    }
}
