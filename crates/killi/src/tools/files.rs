use std::fs;

use serde_json::Value;

use super::{Context, MAX_BYTES, MAX_LINES, Output, is_binary, resolve};
use crate::llm::truncate;

const MAX_LINE_CHARS: usize = 2000;

pub fn read(ctx: &Context, args: &Value) -> Output {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return Output::error("read needs `path`.");
    };
    let full = resolve(ctx, path);
    if full.is_dir() {
        return Output::error(format!("{path} is a directory; use glob to list it."));
    }
    let bytes = match fs::read(&full) {
        Ok(bytes) => bytes,
        Err(e) => return Output::error(format!("Could not read {path}: {e}")),
    };
    if is_binary(&bytes) {
        return Output::error(format!("{path} is a binary file ({} bytes); not shown.", bytes.len()));
    }
    let text = TextFile::decode(&bytes).text;
    if text.is_empty() {
        return Output::ok("(empty file)");
    }

    let lines: Vec<&str> = text.split_terminator('\n').collect();
    let total = lines.len();
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    if offset > total {
        return Output::error(format!("Offset {offset} is beyond the end of {path} ({total} lines)."));
    }
    let limit = args.get("limit").and_then(Value::as_u64).map_or(MAX_LINES, |l| (l as usize).clamp(1, MAX_LINES));

    let mut out = String::new();
    let mut last = offset - 1;
    let mut hit_bytes = false;
    for (i, line) in lines.iter().enumerate().skip(offset - 1).take(limit) {
        let entry = if line.len() > MAX_LINE_CHARS {
            format!("{}\t{}… [line truncated: {} chars]\n", i + 1, truncate(line, MAX_LINE_CHARS), line.len())
        } else {
            format!("{}\t{line}\n", i + 1)
        };
        if !out.is_empty() && out.len() + entry.len() > MAX_BYTES {
            hit_bytes = true;
            break;
        }
        out.push_str(&entry);
        last = i + 1;
    }
    if last < total {
        let why = if hit_bytes { " (50KB limit)" } else { "" };
        out.push_str(&format!("[Showing lines {offset}-{last} of {total}{why}. Use offset={} to continue.]", last + 1));
    }
    Output::ok(out)
}

pub fn write(ctx: &Context, args: &Value) -> Output {
    let (Some(path), Some(content)) =
        (args.get("path").and_then(Value::as_str), args.get("content").and_then(Value::as_str))
    else {
        return Output::error("write needs `path` and `content`.");
    };
    let full = resolve(ctx, path);
    let existed = full.exists();
    if let Some(parent) = full.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        return Output::error(format!("Could not create {}: {e}", parent.display()));
    }
    if let Err(e) = fs::write(&full, content) {
        return Output::error(format!("Could not write {path}: {e}"));
    }
    let verb = if existed { "Overwrote" } else { "Created" };
    Output::ok(format!("{verb} {path} ({} lines).", content.lines().count()))
}

/// A text file as read and edit see it: BOM removed, and CRLF shown as LF when every line
/// ends in CRLF, so both tools work on the same view and writes restore the original form.
pub struct TextFile {
    pub text: String,
    bom: bool,
    crlf: bool,
}

impl TextFile {
    pub fn decode(bytes: &[u8]) -> Self {
        let raw = String::from_utf8_lossy(bytes);
        let (bom, raw) = match raw.strip_prefix('\u{feff}') {
            Some(rest) => (true, rest.to_string()),
            None => (false, raw.into_owned()),
        };
        let newlines = raw.matches('\n').count();
        let crlf = newlines > 0 && raw.matches("\r\n").count() == newlines;
        let text = if crlf { raw.replace("\r\n", "\n") } else { raw };
        Self { text, bom, crlf }
    }

    pub fn encode(&self, text: &str) -> String {
        let body = if self.crlf { text.replace('\n', "\r\n") } else { text.to_string() };
        if self.bom { format!("\u{feff}{body}") } else { body }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(dir: &std::path::Path) -> Context {
        Context { cwd: dir.to_path_buf(), spill_dir: dir.join("spill") }
    }

    #[test]
    fn read_numbers_lines_and_points_to_the_rest() {
        let dir = std::env::temp_dir().join(format!("killi-read-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let body: String = (1..=5).map(|n| format!("line {n}\r\n")).collect();
        fs::write(dir.join("a.txt"), body).unwrap();
        let out = read(&ctx(&dir), &json!({"path": "a.txt", "offset": 2, "limit": 2}));
        assert_eq!(out.text, "2\tline 2\n3\tline 3\n[Showing lines 2-3 of 5. Use offset=4 to continue.]");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn crlf_and_bom_round_trip() {
        let file = TextFile::decode("\u{feff}a\r\nb\r\n".as_bytes());
        assert_eq!(file.text, "a\nb\n");
        assert_eq!(file.encode("a\nc\n"), "\u{feff}a\r\nc\r\n");
    }
}
