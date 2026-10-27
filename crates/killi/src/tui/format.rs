use super::*;

/// A faint dot between two pieces of a muted line, which goes back to muted after it.
pub(super) fn dot() -> String {
    format!(" {FAINT}·{RESET}{DIM} ")
}

/// `text` in `color` inside a dim line: it resets, colors, and goes back to dim after.
pub(super) fn tint(color: impl std::fmt::Display, text: &str) -> String {
    format!("{RESET}{color}{text}{RESET}{DIM}")
}

/// Whether the context has reached the share of the window where the meter turns warn.
pub(super) fn near_full(used: u64, window: u64) -> bool {
    used * 100 / window.max(1) >= 80
}

/// How full the context window is, as a ten-cell lagoon meter that turns warn from 80%.
pub(super) fn gauge(used: u64, window: u64) -> String {
    let filled = (used.saturating_mul(10).div_ceil(window.max(1)) as usize).min(10);
    let color = if near_full(used, window) { WARN } else { LAGOON };
    format!("{RESET}{color}{}{RESET}{FAINT}{}{RESET}{DIM}", "━".repeat(filled), "╌".repeat(10 - filled))
}

/// `text` cut to `max` columns with an ellipsis.
pub(super) fn clip(text: &str, max: usize) -> String {
    if text::width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    for c in text.chars() {
        if text::width(&out) + 2 > max {
            break;
        }
        out.push(c);
    }
    out + "…"
}

/// The effort as a label and its value, as the banner and status line show them; nothing for
/// the provider's default.
pub(super) fn thinking_parts(thinking: &str) -> Option<(&'static str, &str)> {
    match thinking {
        "on" => None,
        "off" => Some(("thinking", "off")),
        effort => Some(("effort", effort)),
    }
}

/// `line` on one row: cut with an ellipsis where the window ends.
pub(super) fn fit(line: &str, width: usize) -> String {
    ellipsize(line, width)
}

/// The indexes of the rows that differ from those on screen, or None when the area changed size
/// and has to be painted whole.
pub(super) fn changed_rows(shown: &[String], rows: &[String]) -> Option<Vec<usize>> {
    (shown.len() == rows.len()).then(|| (0..rows.len()).filter(|&i| shown[i] != rows[i]).collect())
}

/// Blank rows that put a live area of `live` rows on the screen's last row, when `content`
/// rows above it leave room.
pub(super) fn padding(height: usize, content: usize, live: usize) -> usize {
    height.saturating_sub(content + live)
}

/// Moves the cursor from one row of the live area to another.
pub(super) fn move_rows(out: &mut impl Write, from: usize, to: usize) -> std::io::Result<()> {
    match to.cmp(&from) {
        std::cmp::Ordering::Less => queue!(out, MoveUp((from - to) as u16)),
        std::cmp::Ordering::Greater => queue!(out, MoveDown((to - from) as u16)),
        std::cmp::Ordering::Equal => Ok(()),
    }
}

/// What to try for the failures people hit most: the provider's kind and status first, then
/// words in the text — never digits, so "prompt is too long: 214013 tokens" can't read as a
/// 401, and "4010" can't read as one either.
pub(super) fn error_hint(error: &anyhow::Error) -> Option<&'static str> {
    if let Some(provider) = error.downcast_ref::<ProviderError>()
        && let Some(hint) = provider_hint(provider.kind, provider.status)
    {
        return Some(hint);
    }
    error_hint_text(&format!("{error:#}"))
}

/// The hint for a classified provider failure.
fn provider_hint(kind: ErrorKind, status: Option<u16>) -> Option<&'static str> {
    match kind {
        ErrorKind::Overflow => Some("The conversation no longer fits the model: /compact summarizes it, /new starts over."),
        ErrorKind::Auth => Some("Check api_key or api_key_env for this model's provider in ~/.killi/config.toml."),
        ErrorKind::Quota => Some("This provider's usage limit is spent: pick another model with /model, or wait until the reset time above."),
        ErrorKind::RateLimited => Some("The provider is limiting requests: wait a moment, or pick another model with /model."),
        ErrorKind::Transport => Some("Can't reach the provider: check your network, proxy and base_url."),
        ErrorKind::Media | ErrorKind::BadResponse | ErrorKind::Other => match status {
            Some(401 | 403) => Some("Check api_key or api_key_env for this model's provider in ~/.killi/config.toml."),
            Some(402) => Some("This provider's usage limit is spent: pick another model with /model, or wait until the reset time above."),
            Some(429) => Some("The provider is limiting requests: wait a moment, or pick another model with /model."),
            _ => None,
        },
    }
}

/// The hint for words in an error's text, without a classified provider behind it.
fn error_hint_text(error: &str) -> Option<&'static str> {
    let error = error.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| error.contains(w));
    if has(&["unauthorized", "invalid api key", "invalid_api_key", "forbidden"]) {
        Some("Check api_key or api_key_env for this model's provider in ~/.killi/config.toml.")
    } else if has(&["context length", "context_length", "too long", "maximum context", "prompt is too long"]) {
        Some("The conversation no longer fits the model: /compact summarizes it, /new starts over.")
    } else if has(&["usage limit", "quota", "reset at", "exceeded your current"]) {
        Some("This provider's usage limit is spent: pick another model with /model, or wait until the reset time above.")
    } else if has(&["too many requests", "rate limit"]) {
        Some("The provider is limiting requests: wait a moment, or pick another model with /model.")
    } else if has(&["model not found", "unknown model", "does not exist"]) {
        Some("The provider doesn't know this model or URL: check model and base_url in ~/.killi/config.toml.")
    } else if has(&["error sending request", "connection", "dns", "timed out", "timeout"]) {
        Some("Can't reach the provider: check your network, proxy and base_url.")
    } else {
        None
    }
}

/// An error reason with a JSON body cut down to its status and message: `HTTP 529: busy`. What
/// follows the body, such as `(after 3 attempts)`, stays. A proxy's HTML error page shows its
/// title or first line instead of kilobytes of markup.
pub(super) fn gist(reason: &str) -> String {
    let flat = reason.replace('\n', " ");
    if let Some(start) = flat.find('{') {
        let mut bodies = serde_json::Deserializer::from_str(&flat[start..]).into_iter::<serde_json::Value>();
        if let Some(Ok(body)) = bodies.next()
            && let Some(message) = body.pointer("/error/message").or(body.get("message")).and_then(|m| m.as_str())
        {
            let after = flat[start + bodies.byte_offset()..].trim();
            let after = if after.is_empty() { String::new() } else { format!(" {after}") };
            let status: Vec<&str> = flat[..start].split_whitespace().take(2).collect();
            return match status.as_slice() {
                ["HTTP", code] => format!("HTTP {}: {message}{after}", code.trim_end_matches(':')),
                _ => format!("{message}{after}"),
            };
        }
    }
    html_gist(reason).unwrap_or(flat)
}

/// A proxy's HTML error page cut to its title or first line, tags stripped; None when the
/// reason isn't an HTML page.
fn html_gist(reason: &str) -> Option<String> {
    let lower = reason.to_ascii_lowercase();
    if !lower.contains("<html") && !lower.contains("<!doctype") && !lower.contains("<title") {
        return None;
    }
    let markup = reason.find('<').map(|at| &reason[at..]).unwrap_or(reason);
    let lower_markup = markup.to_ascii_lowercase();
    let title = lower_markup.find("<title").and_then(|start| {
        let content = markup[start..].find('>')? + start + 1;
        let end = lower_markup[content..].find("</title>")? + content;
        Some(markup[content..end].to_string())
    });
    let line = title.or_else(|| markup.lines().find(|line| !line.trim().is_empty()).map(str::to_string))?;
    let short = strip_tags(&line).split_whitespace().collect::<Vec<_>>().join(" ");
    if short.is_empty() {
        return None;
    }
    let prefix = match reason.replace('\n', " ").split_whitespace().take(2).collect::<Vec<_>>().as_slice() {
        ["HTTP", code] => format!("HTTP {}: ", code.trim_end_matches(':')),
        _ => String::new(),
    };
    Some(format!("{prefix}{}", first_line(&short, 200)))
}

/// An HTML fragment with its tags cut out, so `<h1>Bad Gateway</h1>` reads as its words.
fn strip_tags(fragment: &str) -> String {
    let mut out = String::with_capacity(fragment.len());
    let mut tag = false;
    for c in fragment.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            _ if !tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// A duration in whole seconds that moves up a unit as it grows: `45s`, `1m05s`, `1h02m`.
pub(super) fn clock(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f32().round() as u64;
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3_600, secs % 3_600 / 60),
    }
}

/// Like `clock`, with a tenth of a second below a minute: `2.4s`, `1m05s`.
pub(super) fn precise_clock(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0..60 => format!("{:.1}s", elapsed.as_secs_f32()),
        _ => clock(elapsed),
    }
}

pub(super) fn short(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3).replace(".0k", "k"),
        _ => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
    }
}

pub(super) fn ago(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    match s {
        0..60 => "just now".into(),
        60..3_600 => format!("{}m ago", s / 60),
        3_600..86_400 => format!("{}h ago", s / 3_600),
        _ => format!("{}d ago", s / 86_400),
    }
}

/// Bytes for the eye: `312 KB`, `4.2 MB`.
pub(crate) fn megabytes(bytes: u64) -> String {
    match bytes {
        0..1_000_000 => format!("{} KB", bytes / 1_000),
        _ => format!("{:.1} MB", bytes as f64 / 1e6),
    }
}
