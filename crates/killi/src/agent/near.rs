//! Near-duplicate keys: calls that differ only in how they trim or page their output share
//! one key per turn, so the model is warned once about repetition the exact repeat breaker
//! cannot see (it hashes the exact arguments).

use super::*;

/// Read/search calls against one target in a turn at which the model is warned once, however
/// their tuning arguments differ. A warning only: paging through a long file or narrowing a
/// search legitimately retunes the call, so near-duplicates are never blocked or stopped.
pub(super) const NEAR_REPEAT_WARN_AT: u32 = 5;
/// Bash runs of one command in a turn at which the model is warned once, however their
/// output filters differ. Lower than for reads: a rerun pays the command's whole cost
/// again, and a long output it already produced is saved to a file the model can read.
pub(super) const NEAR_BASH_WARN_AT: u32 = 3;

/// The pipe stages that only trim a command's output, so runs differing in them alone are
/// one command run again. Anything else (`python`, `jq`, …) transforms the output and stays
/// part of the key.
const OUTPUT_FILTERS: [&str; 12] = [
    "tail", "head", "grep", "rg", "awk", "sed", "wc", "sort", "uniq", "cat", "less", "more",
];

/// Identifies a call by its tool and target alone, so a call that only retunes its output
/// shares the key. `read`, `grep` and `glob` count by path, with offset, pattern and limit
/// dropped by construction; a grep/glob without a path searches the working directory, so
/// it counts against `.`. `bash` counts by its command with output-filter pipes and
/// redirects stripped.
pub(super) fn near_key(call: &ToolCall) -> Option<u64> {
    let args = parse_args(call).ok()?;
    let target: Cow<str> = match call.name.as_str() {
        "read" => args["path"].as_str().filter(|path| !path.is_empty())?.into(),
        "grep" | "glob" => args["path"]
            .as_str()
            .filter(|path| !path.is_empty())
            .unwrap_or(".")
            .into(),
        "bash" => base_command(args["command"].as_str()?)?.into(),
        _ => return None,
    };
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    (&call.name, target.as_ref()).hash(&mut hasher);
    Some(hasher.finish())
}

/// The command with its output-filter pipes and redirects stripped: what remains is the
/// work the command does, so runs that differ only in how they trim its output share it.
/// Only known filters are stripped — a pipe into anything else stays part of the command —
/// and a filter that starts the command is the command itself (`tail -f`), not a trim.
fn base_command(command: &str) -> Option<String> {
    let mut base = String::new();
    for (i, segment) in pipe_segments(command.trim()).into_iter().enumerate() {
        if i > 0 {
            let first = segment.split_whitespace().next().unwrap_or_default();
            if OUTPUT_FILTERS.contains(&first) {
                break;
            }
            base.push('|');
        }
        base.push_str(strip_redirects(segment).trim());
    }
    let base = base.trim();
    (!base.is_empty()).then(|| base.to_string())
}

/// Splits a command at its top-level pipes, leaving quoted ones alone. `||` is a choice,
/// not a pipe; `|&` pipes both streams and splits like `|`.
fn pipe_segments(command: &str) -> Vec<&str> {
    let bytes = command.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            escaped = false;
        } else if let Some(q) = quote {
            if b == b'\\' && q != b'\'' {
                escaped = true;
            } else if b == q {
                quote = None;
            }
        } else {
            match b {
                b'\'' | b'"' | b'`' => quote = Some(b),
                b'\\' => escaped = true,
                b'|' if bytes.get(i + 1) != Some(&b'|') => {
                    segments.push(&command[start..i]);
                    i += usize::from(bytes.get(i + 1) == Some(&b'&'));
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    segments.push(&command[start..]);
    segments
}

/// Removes a segment's output redirects (`> file`, `2>> log`, `&> all`, `2>&1`), leaving
/// quoted text and input redirects alone: where the output goes doesn't change the work.
fn strip_redirects(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = String::new();
    let mut copied = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            escaped = false;
        } else if let Some(q) = quote {
            if b == b'\\' && q != b'\'' {
                escaped = true;
            } else if b == q {
                quote = None;
            }
        } else if b == b'>' {
            // An fd number glued to the operator is a whole word of digits (`2>`, `12>`);
            // digits inside a word (`x2>`, `-2>`) belong to the word.
            let mut start = i;
            while start > copied
                && !bytes[start - 1].is_ascii_whitespace()
                && !matches!(bytes[start - 1], b'|' | b';' | b'&' | b'(' | b')' | b'<')
            {
                start -= 1;
            }
            if start == i || !bytes[start..i].iter().all(u8::is_ascii_digit) {
                start = i;
            }
            if start > copied && bytes[start - 1] == b'&' {
                start -= 1;
            }
            let mut end = i + 1;
            if bytes.get(end) == Some(&b'>') {
                end += 1;
            }
            end = match bytes.get(end) {
                Some(&b'&') => {
                    let mut j = end + 1;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j > end + 1 {
                        j
                    } else {
                        redirect_target(bytes, end + 1)
                    }
                }
                _ => redirect_target(bytes, end),
            };
            out.push_str(&segment[copied..start]);
            copied = end;
            i = end;
            continue;
        } else {
            match b {
                b'\'' | b'"' | b'`' => quote = Some(b),
                b'\\' => escaped = true,
                _ => {}
            }
        }
        i += 1;
    }
    out.push_str(&segment[copied..]);
    out
}

/// The end of a redirect's target: a quoted string or a word, after any whitespace.
fn redirect_target(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    match bytes.get(i) {
        Some(&q @ (b'\'' | b'"')) => {
            i += 1;
            while i < bytes.len() && bytes[i] != q {
                i += usize::from(bytes[i] == b'\\' && q == b'"') + 1;
            }
            (i + 1).min(bytes.len())
        }
        _ => {
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                i += 1;
            }
            i
        }
    }
}