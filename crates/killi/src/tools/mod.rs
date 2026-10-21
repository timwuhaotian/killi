//! The model's tools. Definitions are built once per session and never change, so the
//! cached prompt prefix survives.

mod bash;
mod edit;
mod files;
mod search;

use std::path::PathBuf;

use serde_json::{Value, json};

pub const MAX_LINES: usize = 2000;
pub const MAX_BYTES: usize = 50 * 1024;

pub struct Context {
    pub cwd: PathBuf,
    /// Where full outputs go when a result is truncated.
    pub spill_dir: PathBuf,
}

pub struct Output {
    pub text: String,
    pub is_error: bool,
}

impl Output {
    pub fn ok(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: false }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: true }
    }
}

pub async fn run(ctx: &Context, name: &str, args: Value) -> Output {
    match name {
        "read" => files::read(ctx, &args),
        "write" => files::write(ctx, &args),
        "edit" => edit::edit(ctx, &args),
        "bash" => bash::bash(ctx, &args).await,
        "grep" => search::grep(ctx, args).await,
        "glob" => search::glob(ctx, args).await,
        _ => Output::error(format!("Unknown tool `{name}`. Available: read, write, edit, bash, grep, glob.")),
    }
}

/// One line for the terminal: what the call touches.
pub fn summary(name: &str, args: &Value) -> String {
    let field = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or_default();
    let detail = match name {
        "read" | "write" | "edit" => field("path").to_string(),
        "bash" => field("command").lines().next().unwrap_or_default().to_string(),
        "grep" => format!("{:?} {}", field("pattern"), field("path")),
        "glob" => format!("{} {}", field("pattern"), field("path")),
        _ => String::new(),
    };
    format!("{name} {}", crate::llm::truncate(&detail, 120)).trim_end().to_string()
}

pub fn definitions() -> Vec<Value> {
    let tool = |name: &str, description: &str, parameters: Value| {
        json!({"type": "function", "function": {"name": name, "description": description, "parameters": parameters}})
    };
    vec![
        tool(
            "read",
            "Read a text file. Lines are prefixed `N\\t`. Shows up to 2000 lines or 50KB; continue with offset.",
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
            "Run a bash command in the working directory; each call is a fresh shell, stdin closed, stderr merged into stdout. Long output keeps its tail and saves the rest to a file.",
            json!({"type": "object", "properties": {
                "command": {"type": "string"},
                "timeout": {"type": "integer", "description": "Seconds, default 60, max 300"}
            }, "required": ["command"]}),
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
