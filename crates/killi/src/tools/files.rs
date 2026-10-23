use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use super::{Context, MAX_BYTES, MAX_LINES, Output, is_binary, resolve};
use crate::llm::truncate;

const MAX_LINE_CHARS: usize = 2000;
/// Larger text files are not read whole: the file, its decoded copy and its lines would take
/// several times its size in memory. `bash` can read part of one (`sed -n`, `tail`).
const MAX_READ: u64 = 10 * 1024 * 1024;

pub fn read(ctx: &Context, args: &Value) -> Output {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return Output::error("read needs `path`.");
    };
    let full = resolve(ctx, path);
    if full.is_dir() {
        return Output::error(format!("{path} is a directory; use glob to list it."));
    }
    if let Some(media_type) = image_type(&full) {
        return read_image(ctx, path, &full, media_type);
    }
    if let Ok(meta) = fs::metadata(&full)
        && meta.len() > MAX_READ
    {
        return Output::error(format!(
            "{path} is {} MB, too large to read whole. Search it with grep, or read part of it with bash (`sed -n '1000,1100p'`, `head`, `tail`).",
            meta.len() / (1024 * 1024)
        ));
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

    let lines = || text.split_terminator('\n');
    let total = lines().count();
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    if offset > total {
        return Output::error(format!("Offset {offset} is beyond the end of {path} ({total} lines)."));
    }
    let limit = args.get("limit").and_then(Value::as_u64).map_or(MAX_LINES, |l| (l as usize).clamp(1, MAX_LINES));

    let mut out = String::new();
    let mut last = offset - 1;
    let mut hit_bytes = false;
    for (i, line) in lines().enumerate().skip(offset - 1).take(limit) {
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
    let brief = match (offset, total) {
        (1, 1) => "1 line".to_string(),
        (1, _) if last == total => format!("{total} lines"),
        _ => format!("lines {offset}-{last} of {total}"),
    };
    if last < total {
        let why = if hit_bytes { " (50KB limit)" } else { "" };
        out.push_str(&format!("[Showing lines {offset}-{last} of {total}{why}. Use offset={} to continue.]", last + 1));
    }
    Output::ok(out).brief(brief)
}

/// The media type of an image file the model can be shown, by its extension.
pub fn image_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Large images are shrunk with macOS's `sips` to at most 1568 pixels on the long side.
const SHRINK_ABOVE: u64 = 500 * 1024;

pub(super) fn read_image(ctx: &Context, path: &str, full: &Path, media_type: &str) -> Output {
    if !ctx.images {
        return Output::error(format!("{path} is an image, and this model can't see images."));
    }
    match image_url(path, full, media_type) {
        Ok((url, bytes)) => {
            let mut output = Output::ok(format!("{path} is an image ({} KB); it is attached below.", bytes / 1024))
                .brief(format!("image, {} KB", bytes / 1024));
            output.image = Some(url);
            output
        }
        Err(why) => Output::error(why),
    }
}

/// An image file as a data URL for the model, and how many bytes it holds; a large one is
/// sent as a JPEG that `sips` made. `name` is how errors refer to it.
pub fn image_url(name: &str, full: &Path, media_type: &str) -> Result<(String, usize), String> {
    let size = fs::metadata(full).map_err(|e| format!("Could not read {name}: {e}"))?.len();
    let (bytes, media_type) = if size > SHRINK_ABOVE {
        let out = std::env::temp_dir().join(format!("killi-image-{}.jpg", std::process::id()));
        let shrunk = Command::new("/usr/bin/sips")
            .args(["-Z", "1568", "-s", "format", "jpeg", "-s", "formatOptions", "80"])
            .arg(full)
            .arg("--out")
            .arg(&out)
            .output()
            .is_ok_and(|o| o.status.success());
        let bytes = if shrunk { fs::read(&out).ok() } else { None };
        let _ = fs::remove_file(&out);
        match bytes {
            Some(bytes) => (bytes, "image/jpeg"),
            None => return Err(format!("Could not shrink {name} ({} KB) to send it.", size / 1024)),
        }
    } else {
        (fs::read(full).map_err(|e| format!("Could not read {name}: {e}"))?, media_type)
    };
    Ok((format!("data:{media_type};base64,{}", base64(&bytes)), bytes.len()))
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
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
    let lines = content.lines().count();
    Output::ok(format!("{verb} {path} ({}).", super::count(lines, "line"))).brief(format!("{} {}", verb.to_lowercase(), super::count(lines, "line")))
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
        Context::new(dir.to_path_buf(), dir.join("spill"))
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
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
    fn a_file_too_large_to_read_whole_is_not_read() {
        let dir = std::env::temp_dir().join(format!("killi-bigread-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = fs::File::create(dir.join("big.log")).unwrap();
        file.set_len(MAX_READ + 1).unwrap();
        let out = read(&ctx(&dir), &json!({"path": "big.log"}));
        assert!(out.is_error && out.text.contains("too large to read whole"), "{}", out.text);
        fs::write(dir.join("ok.txt"), "a\nb\n").unwrap();
        assert_eq!(read(&ctx(&dir), &json!({"path": "ok.txt", "offset": 2})).text, "2\tb\n");
        assert!(read(&ctx(&dir), &json!({"path": "ok.txt", "offset": 3})).is_error);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn crlf_and_bom_round_trip() {
        let file = TextFile::decode("\u{feff}a\r\nb\r\n".as_bytes());
        assert_eq!(file.text, "a\nb\n");
        assert_eq!(file.encode("a\nc\n"), "\u{feff}a\r\nc\r\n");
    }
}
