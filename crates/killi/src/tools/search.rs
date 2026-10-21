use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use regex::RegexBuilder;
use serde_json::Value;

use super::{Context, MAX_BYTES, Output, is_binary, resolve};
use crate::llm::truncate;

const DEFAULT_MATCHES: usize = 100;
const MAX_MATCH_LINE: usize = 500;
const MAX_FILES: usize = 1000;
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

pub async fn grep(ctx: &Context, args: Value) -> Output {
    let (cwd, root) = (ctx.cwd.clone(), search_root(ctx, &args));
    tokio::task::spawn_blocking(move || grep_blocking(&cwd, &root, &args))
        .await
        .unwrap_or_else(|e| Output::error(format!("grep failed: {e}")))
}

pub async fn glob(ctx: &Context, args: Value) -> Output {
    let (cwd, root) = (ctx.cwd.clone(), search_root(ctx, &args));
    tokio::task::spawn_blocking(move || glob_blocking(&cwd, &root, &args))
        .await
        .unwrap_or_else(|e| Output::error(format!("glob failed: {e}")))
}

fn search_root(ctx: &Context, args: &Value) -> PathBuf {
    match args.get("path").and_then(Value::as_str).filter(|p| !p.is_empty()) {
        Some(path) => resolve(ctx, path),
        None => ctx.cwd.clone(),
    }
}

/// Walks files under `root`, honoring .gitignore, including dotfiles but never `.git`.
fn walker(root: &Path, glob: Option<&str>) -> Result<ignore::Walk, String> {
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false).sort_by_file_name(|a, b| a.cmp(b)).filter_entry(|e| e.file_name() != ".git");
    if let Some(glob) = glob {
        let mut overrides = OverrideBuilder::new(root);
        overrides.add(glob).map_err(|e| format!("Invalid glob `{glob}`: {e}"))?;
        builder.overrides(overrides.build().map_err(|e| format!("Invalid glob `{glob}`: {e}"))?);
    }
    Ok(builder.build())
}

fn grep_blocking(cwd: &Path, root: &Path, args: &Value) -> Output {
    let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
        return Output::error("grep needs `pattern`.");
    };
    let ignore_case = args.get("ignore_case").and_then(Value::as_bool).unwrap_or(false);
    let regex = match RegexBuilder::new(pattern).case_insensitive(ignore_case).build() {
        Ok(regex) => regex,
        Err(e) => return Output::error(format!("Invalid regex: {e}")),
    };
    let limit = args.get("limit").and_then(Value::as_u64).map_or(DEFAULT_MATCHES, |l| l.max(1) as usize);
    let walk = match walker(root, args.get("glob").and_then(Value::as_str)) {
        Ok(walk) => walk,
        Err(message) => return Output::error(message),
    };

    let mut out = String::new();
    let mut matches = 0;
    let mut notes = Vec::new();
    'files: for entry in walk.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) || entry.metadata().is_ok_and(|m| m.len() > MAX_FILE_SIZE) {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else { continue };
        if is_binary(&bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let shown_path = entry.path().strip_prefix(cwd).unwrap_or(entry.path()).display().to_string();
        for (n, line) in text.lines().enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            if matches == limit {
                notes.push(format!("{limit} matches limit reached; use limit={} for more or refine the pattern.", limit * 2));
                break 'files;
            }
            let line = if line.len() > MAX_MATCH_LINE {
                format!("{}… [truncated]", truncate(line, MAX_MATCH_LINE))
            } else {
                line.to_string()
            };
            let entry = format!("{shown_path}:{}: {line}\n", n + 1);
            if out.len() + entry.len() > MAX_BYTES {
                notes.push("50KB limit reached; refine the pattern or path.".into());
                break 'files;
            }
            out.push_str(&entry);
            matches += 1;
        }
    }
    if matches == 0 {
        return Output::ok("No matches found.");
    }
    if !notes.is_empty() {
        out.push_str(&format!("[{}]", notes.join(" ")));
    }
    Output::ok(out.trim_end().to_string()).brief(format!("{matches} match{}", if matches == 1 { "" } else { "es" }))
}

fn glob_blocking(cwd: &Path, root: &Path, args: &Value) -> Output {
    let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
        return Output::error("glob needs `pattern`.");
    };
    let matcher = match GlobBuilder::new(pattern).literal_separator(true).build() {
        Ok(glob) => glob.compile_matcher(),
        Err(e) => return Output::error(format!("Invalid glob `{pattern}`: {e}")),
    };
    let walk = match walker(root, None) {
        Ok(walk) => walk,
        Err(message) => return Output::error(message),
    };
    let mut found = Vec::new();
    let mut more = false;
    for entry in walk.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
        if matcher.is_match(relative) {
            if found.len() == MAX_FILES {
                more = true;
                break;
            }
            found.push(entry.path().strip_prefix(cwd).unwrap_or(entry.path()).display().to_string());
        }
    }
    if found.is_empty() {
        return Output::ok(format!("No files match `{pattern}` under {}.", display_root(cwd, root)));
    }
    let count = found.len();
    let mut out = found.join("\n");
    if more {
        out.push_str(&format!("\n[{MAX_FILES} results limit reached; narrow the pattern or path.]"));
    }
    Output::ok(out).brief(format!("{count}{} file{}", if more { "+" } else { "" }, if count == 1 { "" } else { "s" }))
}

fn display_root(cwd: &Path, root: &Path) -> String {
    match root.strip_prefix(cwd) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".into(),
        Ok(relative) => relative.display().to_string(),
        Err(_) => root.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tree() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("killi-search-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "alpha notes\n").unwrap();
        std::fs::write(dir.join(".git/config"), "alpha\n").unwrap();
        dir
    }

    #[test]
    fn grep_finds_matches_with_line_numbers_and_skips_dot_git() {
        let dir = tree();
        let out = grep_blocking(&dir, &dir, &json!({"pattern": "alpha"}));
        assert_eq!(out.text, "notes.txt:1: alpha notes\nsrc/a.rs:1: fn alpha() {}");
        let out = grep_blocking(&dir, &dir, &json!({"pattern": "ALPHA", "ignore_case": true, "glob": "*.rs"}));
        assert_eq!(out.text, "src/a.rs:1: fn alpha() {}");
    }

    #[test]
    fn glob_matches_relative_paths() {
        let dir = tree();
        assert_eq!(glob_blocking(&dir, &dir, &json!({"pattern": "**/*.rs"})).text, "src/a.rs");
        assert!(glob_blocking(&dir, &dir, &json!({"pattern": "*.rs"})).text.starts_with("No files match"));
    }
}
