use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
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

fn text_reader(path: &Path) -> io::Result<Option<BufReader<File>>> {
    let mut reader = BufReader::new(File::open(path)?);
    if is_binary(reader.fill_buf()?) {
        return Ok(None);
    }
    Ok(Some(reader))
}

/// Reads lossily like `String::from_utf8_lossy(...).lines()`, without holding the whole file.
fn next_line(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> io::Result<Option<String>> {
    buf.clear();
    if reader.read_until(b'\n', buf)? == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    Ok(Some(String::from_utf8_lossy(buf).into_owned()))
}

struct PreviousLine {
    number: usize,
    text: String,
    matched: bool,
}

impl PreviousLine {
    fn new(number: usize, text: String, matched: bool) -> Self {
        let text = if text.len() > MAX_MATCH_LINE { format!("{}… [truncated]", truncate(&text, MAX_MATCH_LINE)) } else { text };
        Self { number, text, matched }
    }
}

struct Pending {
    from: usize,
    to: usize,
    text: String,
}

fn push_line(out: &mut String, path: &str, line: &PreviousLine) {
    let sep = if line.matched { ':' } else { '-' };
    out.push_str(&format!("{path}{sep}{}{sep} {}\n", line.number + 1, line.text));
}

fn flush_pending(pending: &mut Option<Pending>, out: &mut String) -> Result<(), Stop> {
    let Some(pending) = pending.take() else { return Ok(()) };
    if out.len() + pending.text.len() > MAX_BYTES {
        return Err(Stop::Bytes);
    }
    out.push_str(&pending.text);
    Ok(())
}

fn stream_content(
    reader: &mut impl BufRead,
    regex: &Regex,
    context: usize,
    limit: usize,
    path: &str,
    out: &mut String,
    matches: &mut usize,
) -> io::Result<FileResult> {
    let mut buf = Vec::new();
    let mut previous: VecDeque<PreviousLine> = VecDeque::new();
    let mut pending: Option<Pending> = None;
    let mut printed: Option<usize> = None;
    let mut number = 0;
    let mut stop_after_pending = false;
    let mut stop = None;
    while !stop_after_pending || pending.is_some() {
        let Some(text) = next_line(reader, &mut buf)? else { break };
        let matched = regex.is_match(&text);
        let line = PreviousLine::new(number, text, matched);
        let in_pending = pending.as_ref().is_some_and(|chunk| (chunk.from..=chunk.to).contains(&number));
        if matched && !stop_after_pending {
            if *matches == limit {
                if let Some(chunk) = pending.as_mut() {
                    if in_pending {
                        push_line(&mut chunk.text, path, &line);
                    }
                    stop_after_pending = true;
                } else {
                    stop = Some(Stop::Limit);
                    break;
                }
            } else {
                *matches += 1;
                if let Some(chunk) = pending.as_mut() {
                    chunk.to = chunk.to.max(number + context);
                    printed = Some(printed.map_or(chunk.to, |p| p.max(chunk.to)));
                    if in_pending {
                        push_line(&mut chunk.text, path, &line);
                    }
                } else {
                    let from = number.saturating_sub(context).max(printed.map_or(0, |p| p + 1));
                    let to = number + context;
                    let mut chunk = String::new();
                    if context > 0 && printed.is_some_and(|p| from > p + 1) && from <= to {
                        chunk.push_str("--\n");
                    }
                    for line in previous.iter().filter(|line| line.number >= from) {
                        push_line(&mut chunk, path, line);
                    }
                    if from <= number {
                        push_line(&mut chunk, path, &line);
                    }
                    printed = Some(printed.map_or(to, |p| p.max(to)));
                    if from <= to {
                        pending = Some(Pending { from, to, text: chunk });
                    }
                }
            }
        } else if in_pending && let Some(chunk) = pending.as_mut() {
            push_line(&mut chunk.text, path, &line);
        }
        if pending.as_ref().is_some_and(|chunk| chunk.to == number)
            && let Err(stop) = flush_pending(&mut pending, out)
        {
            return Ok(FileResult { matches: 0, stop: Some(stop) });
        }
        if context > 0 {
            previous.push_back(line);
            if previous.len() > context {
                previous.pop_front();
            }
        }
        number += 1;
    }
    if let Err(failed) = flush_pending(&mut pending, out) {
        stop = Some(failed);
    } else if stop_after_pending {
        stop = Some(Stop::Limit);
    }
    Ok(FileResult { matches: 0, stop })
}

fn stream_matches(reader: &mut impl BufRead, regex: &Regex, files_only: bool) -> io::Result<FileResult> {
    let mut buf = Vec::new();
    let mut matches = 0;
    while let Some(line) = next_line(reader, &mut buf)? {
        if regex.is_match(&line) {
            matches += 1;
            if files_only {
                break;
            }
        }
    }
    Ok(FileResult { matches, stop: None })
}

fn walk_error_path(error: &ignore::Error) -> Option<&Path> {
    match error {
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::WithLineNumber { err, .. } | ignore::Error::WithDepth { err, .. } => walk_error_path(err),
        ignore::Error::Partial(errors) => errors.iter().find_map(walk_error_path),
        ignore::Error::Loop { child, .. } => Some(child),
        _ => None,
    }
}

fn stop_note(stop: Stop, limit: usize) -> String {
    match stop {
        Stop::Limit => format!("{limit} matches limit reached; use limit={} for more or refine the pattern.", limit * 2),
        Stop::Bytes => "50KB limit reached; refine the pattern or path.".into(),
    }
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
    let mut errors = SearchErrors::default();
    let mut scanned_files = 0;
    let mut scanned_bytes = 0_u64;
    let mut scan_limited = false;
    'files: for entry in walk {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let path = walk_error_path(&error).unwrap_or(root).to_path_buf();
                errors.record(cwd, &path, error);
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                errors.record(cwd, entry.path(), error);
                continue;
            }
        };
        if metadata.len() > MAX_FILE_SIZE {
            continue;
        }
        if scanned_files == MAX_SCAN_FILES || scanned_bytes.saturating_add(metadata.len()) > MAX_SCAN_BYTES {
            scan_limited = true;
            break;
        }
        scanned_files += 1;
        scanned_bytes += metadata.len();
        let shown_path = entry.path().strip_prefix(cwd).unwrap_or(entry.path()).display().to_string();
        if multiline {
            let bytes = match std::fs::read(entry.path()) {
                Ok(bytes) => bytes,
                Err(error) => {
                    errors.record(cwd, entry.path(), error);
                    continue;
                }
            };
            if is_binary(&bytes) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().collect();
            let ranges = match_ranges(&regex, &text, &lines, true);
            if ranges.is_empty() || lines.is_empty() {
                continue;
            }
            if mode != Mode::Content {
                if files == limit {
                    notes.push(format!("{limit} files limit reached; use limit={} for more or refine the pattern.", limit * 2));
                    break;
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
                    notes.push(stop_note(Stop::Limit, limit));
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
                    let line = PreviousLine::new(n, lines[n].to_string(), is_match[n]);
                    push_line(&mut chunk, &shown_path, &line);
                }
                if out.len() + chunk.len() > MAX_BYTES {
                    notes.push(stop_note(Stop::Bytes, limit));
                    break 'files;
                }
                out.push_str(&chunk);
                printed = Some(printed.map_or(to, |p| p.max(to)));
            }
            continue;
        }
        let mut reader = match text_reader(entry.path()) {
            Ok(Some(reader)) => reader,
            Ok(None) => continue,
            Err(error) => {
                errors.record(cwd, entry.path(), error);
                continue;
            }
        };
        let result = if mode == Mode::Content {
            stream_content(&mut reader, &regex, context, limit, &shown_path, &mut out, &mut matches)
        } else {
            stream_matches(&mut reader, &regex, mode == Mode::Files)
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                errors.record(cwd, entry.path(), error);
                continue;
            }
        };
        if let Some(stop) = result.stop {
            notes.push(stop_note(stop, limit));
            break;
        }
        if mode != Mode::Content {
            if result.matches == 0 {
                continue;
            }
            if files == limit {
                notes.push(format!("{limit} files limit reached; use limit={} for more or refine the pattern.", limit * 2));
                break;
            }
            files += 1;
            matches += result.matches;
            out.push_str(&if mode == Mode::Files { format!("{shown_path}\n") } else { format!("{shown_path}:{}\n", result.matches) });
        }
    }
    if scan_limited {
        notes.push(format!("Search incomplete: internal scan work limit reached ({MAX_SCAN_FILES} files or 1GiB); narrow the pattern or path."));
    }
    if let Some(note) = errors.note() {
        notes.push(note);
    }
    let incomplete = scan_limited || errors.count > 0;
    if matches == 0 {
        let mut text = "No matches found.".to_string();
        if !notes.is_empty() {
            text.push_str(&format!("\n[{}]", notes.join(" ")));
        }
        return if incomplete { Output::error(text) } else { Output::ok(text) };
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
    let mut errors = SearchErrors::default();
    let mut scanned = 0;
    let mut scan_limited = false;
    for entry in walk {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let path = walk_error_path(&error).unwrap_or(root).to_path_buf();
                errors.record(cwd, &path, error);
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Err(error) = entry.metadata() {
            errors.record(cwd, entry.path(), error);
            continue;
        }
        if scanned == MAX_SCAN_FILES {
            scan_limited = true;
            break;
        }
        scanned += 1;
        let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
        if matcher.is_match(relative) {
            if found.len() == MAX_FILES {
                more = true;
                break;
            }
            found.push(entry.path().strip_prefix(cwd).unwrap_or(entry.path()).display().to_string());
        }
    }
    let incomplete = scan_limited || errors.count > 0;
    if found.is_empty() {
        let mut text = format!("No files match `{pattern}` under {}.", display_root(cwd, root));
        if scan_limited {
            text.push_str(&format!("\n[Search incomplete: internal {MAX_SCAN_FILES} files scan limit reached; narrow the pattern or path.]"));
        }
        if let Some(note) = errors.note() {
            text.push_str(&format!("\n[{note}]"));
        }
        return if incomplete { Output::error(text) } else { Output::ok(text) };
    }
    let count = found.len();
    let mut out = found.join("\n");
    if more {
        out.push_str(&format!("\n[{MAX_FILES} results limit reached; narrow the pattern or path.]"));
    }
    if scan_limited {
        out.push_str(&format!("\n[Search incomplete: internal {MAX_SCAN_FILES} files scan limit reached; narrow the pattern or path.]"));
    }
    if let Some(note) = errors.note() {
        out.push_str(&format!("\n[{note}]"));
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
    fn streaming_grep_keeps_every_match_and_overlapping_context() {
        let dir = tree();
        let contents = b"before\nneedle one\nneedle two\nafter\nafter two\n";
        let mut reader = std::io::Cursor::new(contents.as_slice());
        let regex = Regex::new("needle").unwrap();
        let mut out = String::new();
        let mut matches = 0;
        stream_content(&mut reader, &regex, 0, 10, "x.txt", &mut out, &mut matches).unwrap();
        assert_eq!(matches, 2);
        assert_eq!(out, "x.txt:2: needle one\nx.txt:3: needle two\n");

        let mut reader = std::io::Cursor::new(contents.as_slice());
        let mut out = String::new();
        let mut matches = 0;
        stream_content(&mut reader, &regex, 2, 10, "x.txt", &mut out, &mut matches).unwrap();
        assert_eq!(
            out,
            "x.txt-1- before\nx.txt:2: needle one\nx.txt:3: needle two\nx.txt-4- after\nx.txt-5- after two\n"
        );
        let _ = dir;
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
    fn grep_reports_traversal_errors_instead_of_no_matches() {
        let dir = tree();
        let missing = dir.join("missing");
        let out = grep_blocking(&dir, &missing, &json!({"pattern": "alpha"}));
        assert!(out.is_error, "{}", out.text);
        assert!(out.text.contains("No matches found."));
        assert!(out.text.contains("Search incomplete: 1 error"), "{}", out.text);
        assert!(out.text.contains("missing"), "{}", out.text);
        let out = glob_blocking(&dir, &missing, &json!({"pattern": "**/*.rs"}));
        assert!(out.is_error, "{}", out.text);
        assert!(out.text.contains("Search incomplete: 1 error"), "{}", out.text);
    }

    #[cfg(unix)]
    #[test]
    fn grep_marks_partial_results_when_a_file_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tree();
        let denied = dir.join("denied.txt");
        std::fs::write(&denied, "alpha\n").unwrap();
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o0)).unwrap();
        let out = grep_blocking(&dir, &dir, &json!({"pattern": "alpha"}));
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!out.is_error, "{}", out.text);
        assert!(out.text.contains("notes.txt:1: alpha notes"));
        assert!(out.text.contains("Search incomplete: 1 error"), "{}", out.text);
        assert!(out.text.contains("denied.txt"), "{}", out.text);
    }

    #[test]
    fn streaming_grep_preserves_lossy_utf8_and_binary_detection() {
        let dir = tree();
        std::fs::write(dir.join("lossy.txt"), b"alpha \xff\n").unwrap();
        std::fs::write(dir.join("binary.txt"), b"alpha\0hidden\n").unwrap();
        let out = grep_blocking(&dir, &dir, &json!({"pattern": "alpha", "glob": "*.txt"}));
        assert!(out.text.contains("lossy.txt:1: alpha �"), "{}", out.text);
        assert!(!out.text.contains("binary.txt"));
    }

    #[test]
    fn glob_matches_relative_paths() {
        let dir = tree();
        assert_eq!(glob_blocking(&dir, &dir, &json!({"pattern": "**/*.rs"})).text, "src/a.rs");
        assert!(glob_blocking(&dir, &dir, &json!({"pattern": "*.rs"})).text.starts_with("No files match"));
    }
}
