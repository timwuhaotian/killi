use super::*;

/// Puts `text` on the clipboard, through the terminal (OSC 52, which reaches the local
/// clipboard over SSH too) and through pbcopy.
pub(super) fn copy_to_clipboard(text: &str) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut out = stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = out.flush();
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
/// comes back to the pane), otherwise macOS's through plain `osascript`, whose notifications
/// belong to Script Editor. Kil's version is not consulted: a pane outlives the app, and
/// its shell keeps the `TERM_PROGRAM_VERSION` it started with.
pub(super) fn notify(headline: &str, detail: &str) {
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    if shows_osc9(&program, std::env::var_os("KITTY_WINDOW_ID").is_some()) {
        let mut out = stdout();
        let _ = write!(out, "{}", osc9_sequence(headline, detail));
        let _ = out.flush();
        return;
    }
    let mut command = std::process::Command::new("/usr/bin/osascript");
    command.args(notification_args(headline, detail));
    // Reaped on a thread: an unreaped child would linger as a zombie per notification.
    if let Ok(mut child) = command.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
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

/// The `osascript` arguments showing `headline` over `detail`: quotes become `'`, so model
/// text can't close the AppleScript string, and control characters are sanitized away.
/// No `tell application` wrapper: `display notification` is a Standard Addition, so the
/// banner belongs to Script Editor whatever application is told.
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
