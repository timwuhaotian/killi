use super::*;

mod app;
mod helpers;
mod rows;

/// `line` without escape codes, whatever colors the theme picked.
fn unstyled(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        if c == '\x1b' {
            rest = &rest[text::escape_len(rest)..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// `row` without killi's own SGR (`ESC [ digits m`) and its own hyperlinks, so any other
/// escape stays in plain sight.
fn without_own_sgr(row: &str) -> String {
    let mut out = String::new();
    let mut rest = row;
    while let Some(at) = rest.find('\x1b') {
        out.push_str(&rest[..at]);
        let seq = &rest[at..];
        // Killi's own hyperlinks are OSC 8 to a web address, closed the same way.
        if let Some(stripped) = seq.strip_prefix(text::LINK_CLOSE) {
            rest = stripped;
            continue;
        }
        if seq.starts_with("\x1b]8;;") {
            let len = text::escape_len(seq);
            let mut inner = seq["\x1b]8;;".len()..len.min(seq.len())].to_string();
            for suffix in ["\x1b\\", "\x07"] {
                if let Some(stripped) = inner.strip_suffix(suffix) {
                    inner = stripped.to_string();
                    break;
                }
            }
            if inner.starts_with("http://") || inner.starts_with("https://") {
                rest = &seq[len..];
                continue;
            }
        }
        let after = &rest[at + 1..];
        let params = after.strip_prefix('[');
        let end = params.map_or(0, |p| {
            p.find(|c: char| !c.is_ascii_digit() && c != ';')
                .unwrap_or(p.len())
        });
        match params.filter(|p| p[end..].starts_with('m')) {
            Some(params) => rest = &params[end + 1..],
            None => {
                out.push('\x1b');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// An `App` that never touched a terminal: events go to `pending`, which the tests read.
pub(super) fn test_app() -> App {
    App {
        out: stdout(),
        width: 80,
        height: 24,
        cursor_row: 0,
        shown: Vec::new(),
        shown_col: 0,
        pending: Vec::new(),
        editor: Editor::with_history(Vec::new()),
        markdown: Markdown::new(80),
        partial: String::new(),
        preview_cache: None,
        stream: text::Sanitizer::default(),
        reasoning: text::Sanitizer::default(),
        held_blank: false,
        last_blank: false,
        last_kind: Kind::None,
        thinking: None,
        running: Vec::new(),
        swarm: None,
        todos: Vec::new(),
        asking: None,
        ask_past: Vec::new(),
        ask_view: None,
        ask_corrections: Vec::new(),
        picker: None,
        theme_before_preview: None,
        search: None,
        provider: None,
        searxng: None,
        next_input: None,
        effort_later: None,
        entries: Vec::new(),
        skills: crate::skills::Skills::default(),
        model_choices: Vec::new(),
        effort_choices: Vec::new(),
        theme_choices: Vec::new(),
        menu_selected: 0,
        menu_text: String::new(),
        menu_closed: None,
        files: None,
        files_gen: 0,
        file_menu_cache: None,
        busy: None,
        details: Vec::new(),
        detail_back: 0,
        ctrl_o_taught: false,
        status: Status::default(),
        turn_out_base: 0,
        streamed_bytes: 0,
        controls: Controls::default(),
        spinner: 0,
        retry: None,
        snapshotting: false,
        snapshot_seq: 0,
        exit_armed: None,
        queued: Vec::new(),
        send_now: false,
        swarm_send_armed: false,
        aside_asked: None,
        later_noted: false,
        aside: None,
        held_asides: Vec::new(),
        held_prompt: None,
        auto_recap: true,
        recap: None,
        recap_unsaved: None,
        suggesting: None,
        suggestion: None,
        complete_at: None,
        last_end: None,
        cwd: PathBuf::from("/test"),
        notify_after: Duration::from_secs(10),
        sees_images: false,
        since_banner: None,
        content_rows: 24,
        fullscreen: false,
        history: History::new(80),
        view_rows: 24,
        selection: None,
        drag: None,
        pressed: None,
        input_first: 0,
        input_rows: InputRows::default(),
        banner_resumed: false,
        branch_at: None,
        live: None,
        peers: None,
        peer_asks: FuturesUnordered::new(),
        peer_asking: Vec::new(),
        peer_ask_id: 0,
        peer_tells: 0,
        undo_warned: false,
    }
}
