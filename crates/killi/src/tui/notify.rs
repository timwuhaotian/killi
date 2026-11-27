use super::*;

/// Puts `text` on the clipboard, through the terminal (OSC 52, which reaches the local
/// clipboard over SSH too) and through the platform's tool.
pub(super) fn copy_to_clipboard(text: &str) {
    use std::io::Write as _;
    let mut out = stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = out.flush();
    copy_via_tool(text);
}

/// The clipboard writer on macOS: pbcopy, where OSC 52 alone is not enough.
#[cfg(target_os = "macos")]
fn copy_via_tool(text: &str) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    if let Ok(mut child) = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

/// The first clipboard writer that spawns wins; when none does, OSC 52 alone is fine.
#[cfg(target_os = "linux")]
fn copy_via_tool(text: &str) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    for &(program, args) in copy_candidates(wayland) {
        if let Ok(mut child) = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return;
        }
    }
}

/// Elsewhere the terminal's OSC 52 is the only clipboard.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn copy_via_tool(_text: &str) {}

/// Clipboard writers to try in order: Wayland's `wl-copy` first when on Wayland, then X11's.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn copy_candidates(wayland: bool) -> &'static [(&'static str, &'static [&'static str])] {
    const WAYLAND: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    const X11: &[(&str, &[&str])] = &[
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    if wayland { WAYLAND } else { X11 }
}

/// Standard base64 with padding, as OSC 52 wants it.
pub(super) fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Whether [`notify`] and [`report`] write anything: `[agent] notify`, read once at startup.
/// The scripts that drive killi in a pty turn it off.
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Turns every notification and status report on or off for the rest of the process.
pub(super) fn set_enabled(on: bool) {
    ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Writes `sequence` to the terminal, unless notifications are off or a unit test runs.
fn write_terminal(sequence: &str) {
    if cfg!(test) || !ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let mut out = stdout();
    let _ = write!(out, "{sequence}");
    let _ = out.flush();
}

/// A notification, `headline` over a line of `detail`, always written to the terminal: only
/// a notification the terminal posts itself can bring a click back to the pane. Terminals
/// that show OSC 9 (Kil, iTerm2, WezTerm, Ghostty, kitty) get it with the text; every other
/// one gets a plain BEL, which it rings, bounces or badges as it does for any bell. A desktop
/// notification killi posts itself (`osascript`, `notify-send`) can't find the pane, and on
/// macOS belongs to Script Editor, so killi posts none. Kil's version is not consulted: a
/// pane outlives the app, and its shell keeps the `TERM_PROGRAM_VERSION` it started with.
pub(super) fn notify(headline: &str, detail: &str) {
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    write_terminal(&notification_sequence(
        &program,
        std::env::var_os("KITTY_WINDOW_ID").is_some(),
        headline,
        detail,
    ));
}

/// What [`notify`] writes for the terminal named by `TERM_PROGRAM` (or kitty, by its window id).
pub(super) fn notification_sequence(
    program: &str,
    kitty: bool,
    headline: &str,
    detail: &str,
) -> String {
    if shows_osc9(program, kitty) {
        osc9_sequence(headline, detail)
    } else {
        "\x07".to_string()
    }
}

/// Whether the terminal named by `TERM_PROGRAM` (or kitty, by its window id) shows OSC 9 itself.
pub(super) fn shows_osc9(program: &str, kitty: bool) -> bool {
    kitty || ["kil", "iTerm.app", "WezTerm", "ghostty"].contains(&program)
}

/// The OSC 9 sequence showing `headline` over `detail`: both are sanitized, so model text
/// in them can't end the sequence early with a BEL or an escape.
pub(super) fn osc9_sequence(headline: &str, detail: &str) -> String {
    let headline = text::sanitize(headline);
    let detail = text::sanitize(detail);
    let body = if detail.is_empty() {
        headline.into_owned()
    } else {
        format!("{headline} — {detail}")
    };
    format!("\x1b]9;killi: {body}\x07")
}

/// What the session is doing, as the Program Status Protocol (OSC 7501) names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TermState {
    /// A turn is running.
    Working,
    /// A question waits for the user's answer.
    Asking,
    /// The turn finished.
    Done,
    /// The turn failed.
    Failed,
    /// Nothing is running and nothing waits (the user ended the turn).
    Idle,
}

/// Reports the session's state to the terminal with OSC 7501. Terminals that read it (Orca)
/// show it beside the pane and can notify as themselves, so a click comes back to the pane;
/// the rest ignore an OSC they don't know.
pub(super) fn report(status: TermState, msg: &str) {
    write_terminal(&status_sequence(status, msg));
}

/// The OSC 7501 report of `status` with `msg` as its text: base64, as the protocol wants,
/// so no character of it can end the sequence, and cut to stay inside its 2048-byte cap.
pub(super) fn status_sequence(status: TermState, msg: &str) -> String {
    let state = match status {
        TermState::Working => "working",
        TermState::Asking => "blocked:kind=question",
        TermState::Done => "done",
        TermState::Failed => "error",
        TermState::Idle => "idle",
    };
    let msg = text::sanitize(msg);
    let mut cut = msg.len().min(2048);
    while !msg.is_char_boundary(cut) {
        cut -= 1;
    }
    let text = if msg.is_empty() {
        String::new()
    } else {
        format!(":msg={}", base64(msg[..cut].as_bytes()))
    };
    format!("\x1b]7501;state={state}:app=killi{text}\x1b\\")
}

/// The first line of `text` that says something, markdown marks dropped, cut to `chars`:
/// what a notification shows of a reply or a question.
pub(super) fn first_line(text: &str, chars: usize) -> String {
    let mut fenced = false;
    let mut line = String::new();
    for raw in text.lines() {
        let raw = raw.trim();
        if raw.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || raw.is_empty() {
            continue;
        }
        let plain: String = raw.chars().filter(|c| !matches!(c, '*' | '`')).collect();
        line = plain.trim_start_matches(['#', '>', '-', ' ']).to_string();
        if !line.is_empty() {
            break;
        }
    }
    if line.chars().count() <= chars {
        return line;
    }
    let cut: String = line.chars().take(chars - 1).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_tries_wl_copy_first_then_x11_tools() {
        let names: Vec<_> = copy_candidates(true).iter().map(|c| c.0).collect();
        assert_eq!(names, ["wl-copy", "xclip", "xsel"]);
    }

    #[test]
    fn without_wayland_only_x11_tools() {
        let names: Vec<_> = copy_candidates(false).iter().map(|c| c.0).collect();
        assert_eq!(names, ["xclip", "xsel"]);
    }
}
