mod glob;
mod grep;

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use regex::{Regex, RegexBuilder};
use serde_json::Value;

use super::{Context, MAX_BYTES, Output, is_binary, resolve, uint};
use crate::llm::truncate;

use glob::glob_blocking;
use grep::grep_blocking;
#[cfg(test)]
use grep::stream_content;

const DEFAULT_MATCHES: usize = 100;
const MAX_MATCH_LINE: usize = 500;
const MAX_CONTEXT: u64 = 20;
const MAX_FILES: usize = 1000;
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
const MAX_SCAN_FILES: usize = 100_000;
const MAX_SCAN_BYTES: u64 = 1024 * 1024 * 1024;

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
    match args
        .get("path")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    {
        Some(path) => resolve(ctx, path),
        None => ctx.cwd.clone(),
    }
}

/// Walks files under `root`, honoring .gitignore, including dotfiles but never `.git`.
fn walker(root: &Path, glob: Option<&str>) -> Result<ignore::Walk, String> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|e| e.file_name() != ".git");
    if let Some(glob) = glob {
        let mut overrides = OverrideBuilder::new(root);
        overrides
            .add(glob)
            .map_err(|e| format!("Invalid glob `{glob}`: {e}"))?;
        builder.overrides(
            overrides
                .build()
                .map_err(|e| format!("Invalid glob `{glob}`: {e}"))?,
        );
    }
    Ok(builder.build())
}

#[derive(Default)]
struct SearchErrors {
    count: usize,
    examples: Vec<String>,
}

impl SearchErrors {
    fn record(&mut self, cwd: &Path, path: &Path, error: impl std::fmt::Display) {
        self.count += 1;
        if self.examples.len() < 3 {
            let path = path.strip_prefix(cwd).unwrap_or(path).display();
            self.examples.push(format!("{path}: {error}"));
        }
    }

    fn note(&self) -> Option<String> {
        (self.count > 0).then(|| {
            format!(
                "Search incomplete: {} error{}; representative paths: {}.",
                self.count,
                if self.count == 1 { "" } else { "s" },
                self.examples.join("; ")
            )
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Stop {
    Limit,
    Bytes,
}

struct FileResult {
    matches: usize,
    stop: Option<Stop>,
}

fn walk_error_path(error: &ignore::Error) -> Option<&Path> {
    match error {
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::WithLineNumber { err, .. } | ignore::Error::WithDepth { err, .. } => {
            walk_error_path(err)
        }
        ignore::Error::Partial(errors) => errors.iter().find_map(walk_error_path),
        ignore::Error::Loop { child, .. } => Some(child),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
