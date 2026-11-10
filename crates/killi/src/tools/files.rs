use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use base64::Engine;
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
            "{path} is {} MB, too large to read whole. Search or read part of it with bash (`rg`/`grep`, `sed -n '1000,1100p'`, `head`, `tail`).",
            meta.len() / (1024 * 1024)
        ));
    }
    let bytes = match fs::read(&full) {
        Ok(bytes) => bytes,
        Err(e) => return Output::error(format!("Could not read {path}: {e}")),
    };
    if is_binary(&bytes) {
        return Output::error(format!(
            "{path} is a binary file ({} bytes); not shown.",
            bytes.len()
        ));
    }
    let text = TextFile::decode(bytes).text;
    if text.is_empty() {
        return Output::ok("(empty file)");
    }

    let lines = || text.split_terminator('\n');
    let total = lines().count();
    let offset = super::uint(args, "offset").unwrap_or(1).max(1) as usize;
    if offset > total {
        return Output::error(format!(
            "Offset {offset} is beyond the end of {path} ({total} lines)."
        ));
    }
    let limit = super::uint(args, "limit").map_or(MAX_LINES, |l| (l as usize).clamp(1, MAX_LINES));

    let mut out = String::new();
    let mut entry = String::new();
    let mut last = offset - 1;
    let mut hit_bytes = false;
    for (i, line) in lines().enumerate().skip(offset - 1).take(limit) {
        entry.clear();
        if line.len() > MAX_LINE_CHARS {
            use std::fmt::Write;
            let _ = writeln!(
                entry,
                "{}\t{}… [line truncated: {} chars]",
                i + 1,
                truncate(line, MAX_LINE_CHARS),
                line.len()
            );
        } else {
            use std::fmt::Write;
            let _ = writeln!(entry, "{}\t{line}", i + 1);
        }
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
        out.push_str(&format!(
            "[Showing lines {offset}-{last} of {total}{why}. Use offset={} to continue.]",
            last + 1
        ));
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

/// Large images are shrunk to at most 1568 pixels on the long side.
const SHRINK_ABOVE: u64 = 500 * 1024;

/// ImageMagick arguments to shrink into an 80-quality JPEG at most 1568px on the long side.
#[cfg(any(test, not(target_os = "macos")))]
fn imagemagick_args() -> [&'static str; 4] {
    ["-resize", "1568x1568>", "-quality", "80"]
}

/// An ffmpeg filter that shrinks the width to at most 1568px, keeping the aspect ratio.
#[cfg(any(test, not(target_os = "macos")))]
fn ffmpeg_filter() -> &'static str {
    "scale=min(1568,iw):-1"
}

/// A large image shrunk into `out` as a JPEG; false when no shrinker could do it.
#[cfg(target_os = "macos")]
fn shrink(full: &Path, out: &Path) -> bool {
    Command::new("/usr/bin/sips")
        .args([
            "-Z",
            "1568",
            "-s",
            "format",
            "jpeg",
            "-s",
            "formatOptions",
            "80",
        ])
        .arg(full)
        .arg("--out")
        .arg(out)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A large image shrunk into `out` as a JPEG; false when no shrinker could do it.
#[cfg(not(target_os = "macos"))]
fn shrink(full: &Path, out: &Path) -> bool {
    for tool in ["convert", "magick"] {
        if Command::new(tool)
            .arg(full)
            .args(imagemagick_args())
            .arg(out)
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return true;
        }
    }
    Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(full)
        .args(["-vf", ffmpeg_filter(), "-q:v", "4"])
        .arg(out)
        .output()
        .is_ok_and(|o| o.status.success())
}

pub(super) fn read_image(ctx: &Context, path: &str, full: &Path, media_type: &str) -> Output {
    if !ctx.images.get() {
        return Output::error(format!(
            "{path} is an image, and this model can't see images."
        ));
    }
    match image_url(path, full, media_type) {
        Ok((url, bytes)) => {
            let mut output = Output::ok(format!(
                "{path} is an image ({} KB); it is attached below.",
                bytes / 1024
            ))
            .brief(format!("image, {} KB", bytes / 1024));
            output.image = Some(url);
            output
        }
        Err(why) => Output::error(why),
    }
}

/// What the first bytes of a file say it is, when it claims to be `media_type`.
fn detected_kind(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG") {
        return Some("PNG");
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("JPEG");
    }
    if head.starts_with(b"GIF8") {
        return Some("GIF");
    }
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Some("WebP");
    }
    None
}

/// Refuses a file whose magic bytes don't match the media type its extension claimed. A Git
/// LFS pointer says so explicitly, so the user pulls the real file instead of sending text.
fn check_magic(name: &str, full: &Path, media_type: &str) -> Result<(), String> {
    let mut head = [0u8; 256];
    let n = fs::File::open(full)
        .and_then(|mut file| file.read(&mut head))
        .map_err(|e| format!("Could not read {name}: {e}"))?;
    let head = &head[..n];
    if head.starts_with(b"version https://git-lfs") {
        return Err(format!(
            "{name} is a Git LFS pointer, not the image itself; fetch it with `git lfs pull` and try again."
        ));
    }
    let expected = match media_type {
        "image/png" => "PNG",
        "image/jpeg" => "JPEG",
        "image/gif" => "GIF",
        "image/webp" => "WebP",
        _ => return Ok(()),
    };
    match detected_kind(head) {
        Some(found) if found == expected => Ok(()),
        Some(found) => Err(format!(
            "{name} claims to be {media_type} but its bytes look like {found}; rename it or save it in the right format."
        )),
        None if head
            .iter()
            .all(|b| b.is_ascii_graphic() || b.is_ascii_whitespace()) =>
        {
            Err(format!(
                "{name} claims to be {media_type} but its contents look like text; save it in the right format or fix the extension."
            ))
        }
        None => Err(format!(
            "{name} claims to be {media_type} but its magic bytes don't match; save it in the right format or fix the extension."
        )),
    }
}

/// An image file as a data URL for the model, and how many bytes it holds; a large one is
/// shrunk to a JPEG when a shrinker is around. `name` is how errors refer to it.
pub fn image_url(name: &str, full: &Path, media_type: &str) -> Result<(String, usize), String> {
    check_magic(name, full, media_type)?;
    let size = fs::metadata(full)
        .map_err(|e| format!("Could not read {name}: {e}"))?
        .len();
    let (bytes, media_type) = if size > SHRINK_ABOVE {
        let out = std::env::temp_dir().join(format!("killi-image-{}.jpg", std::process::id()));
        let shrunk = shrink(full, &out);
        let bytes = if shrunk { fs::read(&out).ok() } else { None };
        let _ = fs::remove_file(&out);
        match bytes {
            Some(bytes) => (bytes, "image/jpeg"),
            #[cfg(target_os = "macos")]
            None => {
                return Err(format!(
                    "Could not shrink {name} ({} KB) to send it.",
                    size / 1024
                ));
            }
            #[cfg(not(target_os = "macos"))]
            None => (
                fs::read(full).map_err(|e| format!("Could not read {name}: {e}"))?,
                media_type,
            ),
        }
    } else {
        (
            fs::read(full).map_err(|e| format!("Could not read {name}: {e}"))?,
            media_type,
        )
    };
    Ok((
        format!(
            "data:{media_type};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        ),
        bytes.len(),
    ))
}

pub fn write(ctx: &Context, args: &Value) -> Output {
    let (Some(path), Some(content)) = (
        args.get("path").and_then(Value::as_str),
        args.get("content").and_then(Value::as_str),
    ) else {
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
    Output::ok(format!("{verb} {path} ({}).", super::count(lines, "line"))).brief(format!(
        "{} {}",
        verb.to_lowercase(),
        super::count(lines, "line")
    ))
}

/// A text file as read and edit see it: BOM removed, and CRLF shown as LF when every line
/// ends in CRLF, so both tools work on the same view and writes restore the original form.
pub struct TextFile {
    pub text: String,
    bom: bool,
    crlf: bool,
}

impl TextFile {
    pub fn decode(bytes: Vec<u8>) -> Self {
        let raw = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        };
        let (bom, raw) = match raw.strip_prefix('\u{feff}') {
            Some(rest) => (true, rest.to_string()),
            None => (false, raw),
        };
        let mut newlines = 0;
        let mut all_crlf = true;
        for (i, _) in raw.match_indices('\n') {
            newlines += 1;
            if i == 0 || raw.as_bytes()[i - 1] != b'\r' {
                all_crlf = false;
            }
        }
        let crlf = newlines > 0 && all_crlf;
        let text = if crlf { raw.replace("\r\n", "\n") } else { raw };
        Self { text, bom, crlf }
    }

    pub fn encode(&self, text: &str) -> String {
        let body = if self.crlf {
            text.replace('\n', "\r\n")
        } else {
            text.to_string()
        };
        if self.bom {
            format!("\u{feff}{body}")
        } else {
            body
        }
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
    fn read_numbers_lines_and_points_to_the_rest() {
        let dir = crate::test_temp::TempDir::new("killi-read");
        let body: String = (1..=5).map(|n| format!("line {n}\r\n")).collect();
        fs::write(dir.join("a.txt"), body).unwrap();
        let out = read(
            &ctx(&dir),
            &json!({"path": "a.txt", "offset": 2, "limit": 2}),
        );
        assert_eq!(
            out.text,
            "2\tline 2\n3\tline 3\n[Showing lines 2-3 of 5. Use offset=4 to continue.]"
        );
    }

    #[test]
    fn offset_and_limit_arrive_as_strings_too() {
        let dir = crate::test_temp::TempDir::new("killi-read-str");
        fs::write(dir.join("a.txt"), "a\nb\nc\n").unwrap();
        let out = read(
            &ctx(&dir),
            &json!({"path": "a.txt", "offset": "2", "limit": "1"}),
        );
        assert_eq!(
            out.text,
            "2\tb\n[Showing lines 2-2 of 3. Use offset=3 to continue.]"
        );
    }

    #[test]
    fn a_file_too_large_to_read_whole_is_not_read() {
        let dir = crate::test_temp::TempDir::new("killi-bigread");
        let file = fs::File::create(dir.join("big.log")).unwrap();
        file.set_len(MAX_READ + 1).unwrap();
        let out = read(&ctx(&dir), &json!({"path": "big.log"}));
        assert!(
            out.is_error && out.text.contains("too large to read whole"),
            "{}",
            out.text
        );
        fs::write(dir.join("ok.txt"), "a\nb\n").unwrap();
        assert_eq!(
            read(&ctx(&dir), &json!({"path": "ok.txt", "offset": 2})).text,
            "2\tb\n"
        );
        assert!(read(&ctx(&dir), &json!({"path": "ok.txt", "offset": 3})).is_error);
    }

    #[test]
    fn image_bytes_must_match_the_claimed_media_type() {
        let dir = crate::test_temp::TempDir::new("killi-magic");
        let png = dir.join("a.png");
        fs::write(&png, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00]).unwrap();
        let (url, _) = image_url("a.png", &png, "image/png").unwrap();
        assert!(url.starts_with("data:image/png;base64,"), "{url}");

        let jpeg = dir.join("b.png");
        fs::write(&jpeg, [0xFF, 0xD8, 0xFF, 0xE0, 0x00]).unwrap();
        let err = image_url("b.png", &jpeg, "image/png").unwrap_err();
        assert!(err.contains("JPEG") && err.contains("image/png"), "{err}");

        let lfs = dir.join("c.png");
        fs::write(
            &lfs,
            "version https://git-lfs/spec/v1\noid sha256:abc\nsize 12\n",
        )
        .unwrap();
        let err = image_url("c.png", &lfs, "image/png").unwrap_err();
        assert!(err.contains("Git LFS"), "{err}");

        let text = dir.join("d.png");
        fs::write(&text, "just some words").unwrap();
        let err = image_url("d.png", &text, "image/png").unwrap_err();
        assert!(err.contains("text"), "{err}");
    }

    #[test]
    fn shrink_args_stay_stable() {
        assert_eq!(
            imagemagick_args(),
            ["-resize", "1568x1568>", "-quality", "80"]
        );
        assert_eq!(ffmpeg_filter(), "scale=min(1568,iw):-1");
    }

    #[test]
    fn small_images_pass_through_unshrunk() {
        let dir = crate::test_temp::TempDir::new("killi-smallimg");
        let path = dir.join("s.png");
        let bytes = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x01, 0x02];
        fs::write(&path, bytes).unwrap();
        let (url, len) = image_url("s.png", &path, "image/png").unwrap();
        assert_eq!(len, bytes.len());
        assert!(url.starts_with("data:image/png;base64,"), "{url}");
    }

    #[test]
    fn crlf_and_bom_round_trip() {
        let file = TextFile::decode("\u{feff}a\r\nb\r\n".as_bytes().to_vec());
        assert_eq!(file.text, "a\nb\n");
        assert_eq!(file.encode("a\nc\n"), "\u{feff}a\r\nc\r\n");
    }
}
