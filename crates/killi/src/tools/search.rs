use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use regex::{Regex, RegexBuilder};
use serde_json::Value;

use super::{Context, MAX_BYTES, Output, is_binary, resolve};
use crate::llm::truncate;

const DEFAULT_MATCHES: usize = 100;
const MAX_MATCH_LINE: usize = 500;
const MAX_CONTEXT: u64 = 20;
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

#[derive(PartialEq)]
enum Mode {
    Content,
    Files,
    Count,
}

/// Line ranges (first, last; 0-based) of every match in `text`.
fn match_ranges(regex: &Regex, text: &str, lines: &[&str], multiline: bool) -> Vec<(usize, usize)> {
    if !multiline {
        return lines.iter().enumerate().filter(|(_, line)| regex.is_match(line)).map(|(n, _)| (n, n)).collect();
    }
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let line_of = |offset: usize| starts.partition_point(|&s| s <= offset).saturating_sub(1);
    regex
        .find_iter(text)
        .map(|m| {
            let last_line = lines.len().saturating_sub(1);
            let first = line_of(m.start()).min(last_line);
            (first, line_of(m.end().saturating_sub(1).max(m.start())).clamp(first, last_line))
        })
        .collect()
}

fn grep_blocking(cwd: &Path, root: &Path, args: &Value) -> Output {
    let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
        return Output::error("grep needs `pattern`.");
    };
    let flag = |key: &str| args.get(key).and_then(Value::as_bool).unwrap_or(false);
    let multiline = flag("multiline");
    let regex = match RegexBuilder::new(pattern)
        .case_insensitive(flag("ignore_case"))
        .multi_line(multiline)
        .dot_matches_new_line(multiline)
        .build()
    {
        Ok(regex) => regex,
        Err(e) => return Output::error(format!("Invalid regex: {e}")),
    };
    let mode = match args.get("output").and_then(Value::as_str) {
        Some("files") => Mode::Files,
        Some("count") => Mode::Count,
        _ => Mode::Content,
    };
    let context = args.get("context").and_then(Value::as_u64).map_or(0, |c| c.min(MAX_CONTEXT) as usize);
    let limit = args.get("limit").and_then(Value::as_u64).map_or(DEFAULT_MATCHES, |l| l.max(1) as usize);
    let walk = match walker(root, args.get("glob").and_then(Value::as_str)) {
        Ok(walk) => walk,
        Err(message) => return Output::error(message),
    };

    let mut out = String::new();
    let mut matches = 0;
    let mut files = 0;
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
        let lines: Vec<&str> = text.lines().collect();
        let ranges = match_ranges(&regex, &text, &lines, multiline);
        if ranges.is_empty() || lines.is_empty() {
            continue;
        }
        let shown_path = entry.path().strip_prefix(cwd).unwrap_or(entry.path()).display().to_string();
        if mode != Mode::Content {
            if files == limit {
                notes.push(format!("{limit} files limit reached; use limit={} for more or refine the pattern.", limit * 2));
                break 'files;
            }
            files += 1;
            matches += ranges.len();
            out.push_str(&if mode == Mode::Files { format!("{shown_path}\n") } else { format!("{shown_path}:{}\n", ranges.len()) });
            continue;
        }
        let mut is_match = vec![false; lines.len()];
        for &(first, last) in &ranges {
            is_match[first..=last.min(lines.len().saturating_sub(1))].fill(true);
        }
        let mut printed: Option<usize> = None;
        for &(first, last) in &ranges {
            if matches == limit {
                notes.push(format!("{limit} matches limit reached; use limit={} for more or refine the pattern.", limit * 2));
                break 'files;
            }
            matches += 1;
            let from = first.saturating_sub(context).max(printed.map_or(0, |p| p + 1));
            let to = (last + context).min(lines.len().saturating_sub(1));
            let mut chunk = String::new();
            if context > 0 && printed.is_some_and(|p| from > p + 1) && from <= to {
                chunk.push_str("--\n");
            }
            for n in from..=to {
                let line = lines[n];
                let line = if line.len() > MAX_MATCH_LINE {
                    format!("{}… [truncated]", truncate(line, MAX_MATCH_LINE))
                } else {
                    line.to_string()
                };
                let (sep_a, sep_b) = if is_match[n] { (':', ':') } else { ('-', '-') };
                chunk.push_str(&format!("{shown_path}{sep_a}{}{sep_b} {line}\n", n + 1));
            }
            if out.len() + chunk.len() > MAX_BYTES {
                notes.push("50KB limit reached; refine the pattern or path.".into());
                break 'files;
            }
            out.push_str(&chunk);
            printed = Some(printed.map_or(to, |p| p.max(to)));
        }
    }
    if matches == 0 {
        return Output::ok("No matches found.");
    }
    if !notes.is_empty() {
        out.push_str(&format!("[{}]", notes.join(" ")));
    }
    let brief = match mode {
        Mode::Content => format!("{matches} match{}", if matches == 1 { "" } else { "es" }),
        _ => format!("{files} file{}", if files == 1 { "" } else { "s" }),
    };
    Output::ok(out.trim_end().to_string()).brief(brief)
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

    /// A small tree of files, removed when it goes out of scope.
    struct Tree(PathBuf);

    impl std::ops::Deref for Tree {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tree() -> Tree {
        let dir = std::env::temp_dir().join(format!("killi-search-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "alpha notes\n").unwrap();
        std::fs::write(dir.join(".git/config"), "alpha\n").unwrap();
        Tree(dir)
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
    fn grep_shows_context_and_other_output_modes() {
        let dir = tree();
        std::fs::write(dir.join("long.txt"), "a\nb\nneedle\nc\nd\ne\nf\ng\nneedle\nh\n").unwrap();
        let out = grep_blocking(&dir, &dir, &json!({"pattern": "needle", "context": 1, "glob": "long.txt"}));
        assert_eq!(out.text, "long.txt-2- b\nlong.txt:3: needle\nlong.txt-4- c\n--\nlong.txt-8- g\nlong.txt:9: needle\nlong.txt-10- h");
        let out = grep_blocking(&dir, &dir, &json!({"pattern": "alpha", "output": "files"}));
        assert_eq!(out.text, "notes.txt\nsrc/a.rs");
        let out = grep_blocking(&dir, &dir, &json!({"pattern": "needle", "output": "count"}));
        assert_eq!(out.text, "long.txt:2");
    }

    #[test]
    fn grep_matches_across_lines() {
        let dir = tree();
        let out = grep_blocking(&dir, &dir, &json!({"pattern": r"alpha\(\) \{\}\nfn beta", "multiline": true}));
        assert_eq!(out.text, "src/a.rs:1: fn alpha() {}\nsrc/a.rs:2: fn beta() {}");
        let out = grep_blocking(&dir, &dir, &json!({"pattern": "$", "multiline": true, "glob": "*.txt"}));
        assert!(!out.is_error, "{}", out.text);
    }

    #[test]
    fn glob_matches_relative_paths() {
        let dir = tree();
        assert_eq!(glob_blocking(&dir, &dir, &json!({"pattern": "**/*.rs"})).text, "src/a.rs");
        assert!(glob_blocking(&dir, &dir, &json!({"pattern": "*.rs"})).text.starts_with("No files match"));
    }
}
