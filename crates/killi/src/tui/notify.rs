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

/// A desktop notification, `headline` over a line of `detail`: the terminal's own when it
/// supports OSC 9 (Kil, iTerm2, WezTerm, Ghostty, kitty show it in their name, so a click
/// comes back to the pane), otherwise the desktop's (macOS through plain `osascript`, whose
/// notifications belong to Script Editor, Linux through `notify-send`). Kil's version is not
/// consulted: a pane outlives the app, and its shell keeps the `TERM_PROGRAM_VERSION` it
/// started with.
pub(super) fn notify(headline: &str, detail: &str) {
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    if shows_osc9(&program, std::env::var_os("KITTY_WINDOW_ID").is_some()) {
        let mut out = stdout();
        let _ = write!(out, "{}", osc9_sequence(headline, detail));
        let _ = out.flush();
        return;
    }
    desktop_notify(headline, detail);
}

/// macOS's notification through plain `osascript`.
#[cfg(target_os = "macos")]
fn desktop_notify(headline: &str, detail: &str) {
    let mut command = std::process::Command::new("/usr/bin/osascript");
    command.args(notification_args(headline, detail));
    // Reaped on a thread: an unreaped child would linger as a zombie per notification.
    if let Ok(mut child) = command.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Linux's notification through `notify-send`, silently skipped when missing.
#[cfg(target_os = "linux")]
fn desktop_notify(headline: &str, detail: &str) {
    let mut command = std::process::Command::new("notify-send");
    command.args(notify_send_args(headline, detail));
    if let Ok(mut child) = command.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn desktop_notify(_headline: &str, _detail: &str) {}

/// The `notify-send` arguments showing `headline` over `detail`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn notify_send_args(headline: &str, detail: &str) -> Vec<String> {
    let headline = text::sanitize(headline);
    let detail = text::sanitize(detail);
    let body = if detail.is_empty() {
        headline.into_owned()
    } else {
        format!("{headline} — {detail}")
    };
    vec!["killi".to_string(), body]
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

/// The `osascript` arguments showing `headline` over `detail`: quotes become `'`, so model
/// text can't close the AppleScript string, and control characters are sanitized away.
/// No `tell application` wrapper: `display notification` is a Standard Addition, so the
/// banner belongs to Script Editor whatever application is told.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn notification_args(headline: &str, detail: &str) -> Vec<String> {
    let quote = |text: &str| text::sanitize(text).replace('"', "'");
    let headline = quote(headline);
    let detail = quote(detail);
    let plain = if detail.is_empty() {
        format!("display notification \"{headline}\" with title \"killi\"")
    } else {
        format!("display notification \"{detail}\" with title \"killi\" subtitle \"{headline}\"")
    };
    vec!["-e".into(), plain]
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

    #[test]
    fn notify_send_shows_headline_over_detail() {
        assert_eq!(
            notify_send_args("Done", "All tests pass"),
            ["killi", "Done — All tests pass"]
        );
        assert_eq!(notify_send_args("Done", ""), ["killi", "Done"]);
    }
}
