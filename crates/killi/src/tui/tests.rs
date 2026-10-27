use super::*;

mod app;
mod helpers;
mod rows;

    /// `line` without escape codes, whatever colors the theme picked.
    fn unstyled(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                chars.by_ref().find(char::is_ascii_alphabetic);
            } else {
                out.push(c);
            }
        }
        out
    }

    /// `row` without killi's own SGR (`ESC [ digits m`), so any other escape stays in plain sight.
    fn without_own_sgr(row: &str) -> String {
        let mut out = String::new();
        let mut rest = row;
        while let Some(at) = rest.find('\x1b') {
            out.push_str(&rest[..at]);
            let after = &rest[at + 1..];
            let params = after.strip_prefix('[');
            let end = params.map_or(0, |p| p.find(|c: char| !c.is_ascii_digit() && c != ';').unwrap_or(p.len()));
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
    fn test_app() -> App {
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
            search: None,
            next_input: None,
            effort_later: None,
            entries: Vec::new(),
            model_choices: Vec::new(),
            effort_choices: Vec::new(),
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
            controls: Controls::default(),
            spinner: 0,
            retry: None,
            snapshotting: false,
            exit_armed: None,
            queued: Vec::new(),
            send_now: false,
            aside_asked: None,
            later_noted: false,
            aside: None,
            held_asides: Vec::new(),
            auto_recap: true,
            recap: None,
            last_end: None,
            cwd: PathBuf::from("/test"),
            notify_after: Duration::from_secs(10),
            sees_images: false,
            since_banner: None,
            content_rows: 24,
            fullscreen: false,
            history: History::new(80),
            view_rows: 24,
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
