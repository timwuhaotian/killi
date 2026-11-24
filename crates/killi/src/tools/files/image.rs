//! Images for the model: the media type an extension claims, a check of the bytes against it,
//! and a large image shrunk to a JPEG by `sips`, ImageMagick or ffmpeg before it is sent. A
//! shrinker runs in a session of its own under a deadline, so a hung converter is killed with
//! everything it started instead of freezing the session.

use std::fs;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;

use crate::tools::{Context, Output, OwnSession, Tracked, track};

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
/// How long a shrinker may take before it is killed.
const SHRINK_TIMEOUT: Duration = Duration::from_secs(20);
/// How often the blocking path checks whether its shrinker has exited.
const SHRINK_POLL: Duration = Duration::from_millis(10);

/// ImageMagick arguments to shrink into an 80-quality JPEG at most 1568px on the long side.
#[cfg(any(test, not(target_os = "macos")))]
fn imagemagick_args() -> [&'static str; 4] {
    ["-resize", "1568x1568>", "-quality", "80"]
}

/// An ffmpeg filter that shrinks the long side to at most 1568px, keeping the aspect ratio.
/// The quotes keep the commas inside `min()` from splitting the filter chain.
#[cfg(any(test, not(target_os = "macos")))]
fn ffmpeg_filter() -> &'static str {
    "scale='min(1568,iw)':'min(1568,ih)':force_original_aspect_ratio=decrease"
}

/// ImageMagick (`tool` is `convert` or `magick`) on the first frame only, so an animated GIF
/// gives one JPEG instead of one file per frame.
#[cfg(any(test, not(target_os = "macos")))]
fn imagemagick(tool: &str, full: &Path, out: &Path) -> Command {
    let mut input = full.as_os_str().to_owned();
    input.push("[0]");
    let mut command = Command::new(tool);
    command.arg(input).args(imagemagick_args()).arg(out);
    command
}

/// ffmpeg on the first frame only: an image file holds one frame.
#[cfg(any(test, not(target_os = "macos")))]
fn ffmpeg(full: &Path, out: &Path) -> Command {
    let mut command = Command::new("ffmpeg");
    command
        .args(["-y", "-i"])
        .arg(full)
        .args(["-vf", ffmpeg_filter(), "-frames:v", "1", "-q:v", "4"])
        .arg(out);
    command
}

/// The commands that can shrink `full` into `out` as a JPEG, in the order to try them.
#[cfg(target_os = "macos")]
fn shrinkers(full: &Path, out: &Path) -> Vec<Command> {
    let mut sips = Command::new("/usr/bin/sips");
    sips.args([
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
    .arg(out);
    vec![sips]
}

/// The commands that can shrink `full` into `out` as a JPEG, in the order to try them.
#[cfg(not(target_os = "macos"))]
fn shrinkers(full: &Path, out: &Path) -> Vec<Command> {
    vec![
        imagemagick("convert", full, out),
        imagemagick("magick", full, out),
        ffmpeg(full, out),
    ]
}

/// How one shrinker run ended.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ran {
    Done,
    /// It could not start, or exited with a failure; the next shrinker may still work.
    Failed,
    /// It ran past its deadline and was killed; no other shrinker is tried.
    TimedOut,
}

/// `command` with no terminal and nothing to read, in a session of its own so `killpg`
/// reaches whatever it starts.
fn quiet(command: &mut Command) -> &mut Command {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .own_session()
}

fn kill_group(pid: u32) {
    // SAFETY: the child leads its own session and has not been reaped, so the group is its.
    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
}

/// Runs `command`, blocking the thread for at most `limit`. A child that can't be waited for
/// is not killed: it may have been reaped already, and its pid handed to someone else.
fn run_blocking(mut command: Command, limit: Duration) -> Ran {
    let Ok(mut child) = quiet(&mut command).spawn() else {
        return Ran::Failed;
    };
    let _tracked = track(child.id());
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ran::Done,
            Ok(Some(_)) | Err(_) => return Ran::Failed,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(SHRINK_POLL),
            Ok(None) => {
                kill_group(child.id());
                let _ = child.wait();
                return Ran::TimedOut;
            }
        }
    }
}

/// A running shrinker. Dropping it before the shrinker is reaped, at the deadline or when the
/// call is interrupted, kills its whole group.
struct Shrinking {
    child: tokio::process::Child,
    _tracked: Option<Tracked>,
}

impl Drop for Shrinking {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            kill_group(pid);
        }
    }
}

/// Runs `command` without blocking the runtime, for at most `limit`.
async fn run_async(mut command: Command, limit: Duration) -> Ran {
    quiet(&mut command);
    let mut command = tokio::process::Command::from(command);
    let Ok(child) = command.kill_on_drop(true).spawn() else {
        return Ran::Failed;
    };
    let mut shrinking = Shrinking {
        _tracked: child.id().map(track),
        child,
    };
    match tokio::time::timeout(limit, shrinking.child.wait()).await {
        Ok(Ok(status)) if status.success() => Ran::Done,
        Ok(_) => Ran::Failed,
        Err(_) => Ran::TimedOut,
    }
}

/// A private folder for one shrink's output, removed with whatever a shrinker left in it.
/// Its name is new to this call and it is created only if nothing holds that name, so no one
/// else can plant or read the file a shrinker writes there.
struct ShrinkDir(PathBuf);

impl ShrinkDir {
    fn new() -> Option<Self> {
        for attempt in 0..8 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!(
                "killi-image-{}-{nanos}-{attempt}",
                std::process::id()
            ));
            match fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => return Some(Self(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return None,
            }
        }
        None
    }

    fn out(&self) -> PathBuf {
        self.0.join("image.jpg")
    }
}

impl Drop for ShrinkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `read` of an image, and a screenshot: the image attached for the model, or why it can't be.
pub(in crate::tools) async fn read_image(
    ctx: &Context,
    path: &str,
    full: &Path,
    media_type: &str,
) -> Output {
    if !ctx.images.get() {
        return Output::error(format!(
            "{path} is an image, and this model can't see images."
        ));
    }
    match image_url_async(path, full, media_type).await {
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

/// The size of an image whose bytes match `media_type`.
fn checked_size(name: &str, full: &Path, media_type: &str) -> Result<u64, String> {
    check_magic(name, full, media_type)?;
    Ok(fs::metadata(full)
        .map_err(|e| format!("Could not read {name}: {e}"))?
        .len())
}

fn read_whole(name: &str, full: &Path) -> Result<Vec<u8>, String> {
    fs::read(full).map_err(|e| format!("Could not read {name}: {e}"))
}

fn data_url(bytes: &[u8], media_type: &str) -> (String, usize) {
    (
        format!(
            "data:{media_type};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ),
        bytes.len(),
    )
}

/// The data URL for a large image once its shrinkers have run: the JPEG they made, or else
/// the image as it is where that can be sent (macOS always has `sips`, so a failure there is
/// reported instead).
fn after_shrink(
    name: &str,
    full: &Path,
    media_type: &str,
    size: u64,
    ran: Ran,
    dir: Option<ShrinkDir>,
) -> Result<(String, usize), String> {
    if ran == Ran::Done
        && let Some(bytes) = dir.as_ref().and_then(|dir| fs::read(dir.out()).ok())
    {
        return Ok(data_url(&bytes, "image/jpeg"));
    }
    if cfg!(target_os = "macos") {
        return Err(match ran {
            Ran::TimedOut => format!(
                "Shrinking {name} ({} KB) took more than {} s, so it was not sent.",
                size / 1024,
                SHRINK_TIMEOUT.as_secs()
            ),
            _ => format!("Could not shrink {name} ({} KB) to send it.", size / 1024),
        });
    }
    Ok(data_url(&read_whole(name, full)?, media_type))
}

/// An image file as a data URL for the model, and how many bytes it holds; a large one is
/// shrunk to a JPEG when a shrinker is around. `name` is how errors refer to it. This blocks
/// the thread while a shrinker runs, at most `SHRINK_TIMEOUT` per shrinker; the tools use
/// `image_url_async`.
pub fn image_url(name: &str, full: &Path, media_type: &str) -> Result<(String, usize), String> {
    let size = checked_size(name, full, media_type)?;
    if size <= SHRINK_ABOVE {
        return Ok(data_url(&read_whole(name, full)?, media_type));
    }
    let dir = ShrinkDir::new();
    let mut ran = Ran::Failed;
    if let Some(dir) = &dir {
        for command in shrinkers(full, &dir.out()) {
            ran = run_blocking(command, SHRINK_TIMEOUT);
            if ran != Ran::Failed {
                break;
            }
        }
    }
    after_shrink(name, full, media_type, size, ran, dir)
}

/// `image_url` without blocking the runtime: an interrupted call kills its shrinker.
async fn image_url_async(
    name: &str,
    full: &Path,
    media_type: &str,
) -> Result<(String, usize), String> {
    let size = checked_size(name, full, media_type)?;
    if size <= SHRINK_ABOVE {
        return Ok(data_url(&read_whole(name, full)?, media_type));
    }
    let dir = ShrinkDir::new();
    let mut ran = Ran::Failed;
    if let Some(dir) = &dir {
        for command in shrinkers(full, &dir.out()) {
            ran = run_async(command, SHRINK_TIMEOUT).await;
            if ran != Ran::Failed {
                break;
            }
        }
    }
    after_shrink(name, full, media_type, size, ran, dir)
}

#[cfg(test)]
mod tests;
