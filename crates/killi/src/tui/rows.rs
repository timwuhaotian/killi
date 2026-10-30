use super::*;

/// Rows for a list of (name, note) choices, `visible` at a time around `selected`, the names
/// in a column at most `column` wide and the notes dimmed beside them.
pub(super) fn choice_rows(options: &[(&str, &str)], selected: usize, visible: usize, width: usize, column: usize) -> Vec<String> {
    let start = selected.saturating_sub(visible / 2).min(options.len() - visible);
    let shown = &options[start..start + visible];
    let column = shown.iter().map(|(name, _)| text::width(name)).max().unwrap_or(0).clamp(12, column.max(12));
    shown
        .iter()
        .enumerate()
        .map(|(i, (name, note))| {
            let name = clip(name, column);
            let pad = " ".repeat(column.saturating_sub(text::width(&name)));
            let note = note.split_whitespace().collect::<Vec<_>>().join(" ");
            if start + i == selected {
                let line = fit(&format!("{SELECT}{LAGOON}❯ {RESET}{SELECT}{BOLD}{name}{RESET}{SELECT}{pad}  {note}"), width);
                let fill = " ".repeat(width.saturating_sub(text::width(&line)));
                format!("{line}{SELECT}{fill}{RESET}")
            } else {
                fit(&format!("  {name}{pad}  {DIM}{note}{RESET}"), width)
            }
        })
        .collect()
}

/// Rows for an `ask_user_question` call: one wrapped block per option, so a long option
/// takes the rows it needs instead of being cut with an ellipsis. The selected block sits
/// on the select stripe to the window's edge; continuation rows hang under the option's
/// text. `visible` options show at a time around `selected`, like `choice_rows`.
pub(super) fn ask_rows(names: &[String], selected: usize, visible: usize, width: usize) -> Vec<String> {
    if names.is_empty() || width == 0 {
        return Vec::new();
    }
    let visible = visible.clamp(1, names.len());
    let start = selected.saturating_sub(visible / 2).min(names.len() - visible);
    names[start..start + visible]
        .iter()
        .enumerate()
        .flat_map(|(i, name)| {
            if start + i == selected {
                wrap_words(&format!("{SELECT}{LAGOON}❯ {RESET}{SELECT}{BOLD}{name}{RESET}"), width, 5)
                    .into_iter()
                    .map(|row| {
                        let fill = " ".repeat(width.saturating_sub(text::width(&row)));
                        format!("{row}{SELECT}{fill}{RESET}")
                    })
                    .collect::<Vec<_>>()
            } else {
                wrap_words(&format!("  {name}"), width, 5)
            }
        })
        .collect()
}

/// What the user typed as their message block: tinted rows the width of the window.
pub(super) fn prompt_rows(prompt: &str, width: usize) -> Vec<String> {
    let prompt = text::sanitize(prompt);
    let mut rows = Vec::new();
    let mut first = true;
    // The band is what sets the message apart; with no colors the words are bold instead.
    let weight = if text::no_color() { BOLD } else { "" };
    for line in prompt.lines() {
        for row in text::wrap_words(line, width.saturating_sub(3), 0) {
            let lead = if std::mem::take(&mut first) { format!("{FIN}{BOLD}❯{RESET}{TINT}") } else { " ".to_string() };
            let pad = " ".repeat(width.saturating_sub(3 + text::width(&row)));
            rows.push(format!("{TINT} {lead} {weight}{row}{pad}{RESET}"));
        }
    }
    rows
}

/// A question the model asks, as rows: the `?` mark on its first line, each of its lines
/// wrapped under the mark, so a multi-line question reads as its lines and not as one row
/// the line breaks run over.
pub(super) fn question_rows(text: &str, width: usize) -> Vec<String> {
    text::sanitize(text)
        .lines()
        .enumerate()
        .flat_map(|(n, line)| {
            let mark = if n == 0 { format!("{BOLD}{FIN}?{RESET} ") } else { String::new() };
            wrap_words(&format!("{mark}{BOLD}{FIN}{line}{RESET}"), width, 2)
        })
        .collect()
}

/// A /btw question and its answer as one block: the question, the answer hung off the output
/// gutter like a tool's output, and `note` last, so it reads as set aside from the conversation.
pub(super) fn aside_rows(tag: &str, question: &str, answer: &str, note: &str, width: usize) -> Vec<String> {
    let question = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let head = if question.is_empty() { format!("{BOLD}{FIN}{tag}{RESET}") } else { format!("{BOLD}{FIN}{tag}{RESET} {BOLD}{question}{RESET}") };
    let mut rows = wrap_words(&head, width, 4);
    let gutter = format!("  {FAINT}│{RESET}");
    let inner = width.saturating_sub(4);
    let mut markdown = Markdown::new(inner);
    let mut styled: Vec<String> = answer.lines().flat_map(|line| markdown.line(line)).collect();
    styled.extend(markdown.finish());
    while styled.last().is_some_and(|row| text::width(row) == 0) {
        styled.pop();
    }
    for row in styled {
        if text::width(&row) == 0 {
            rows.push(gutter.clone());
            continue;
        }
        for wrapped in wrap_words(&row, inner, text::hanging_indent(&row)) {
            rows.push(format!("{gutter} {wrapped}"));
        }
    }
    rows.push(gutter.clone());
    for wrapped in wrap_words(note, inner, 0) {
        rows.push(format!("{gutter} {DIM}{wrapped}{RESET}"));
    }
    rows
}

/// `/help`: the commands, then the keys, each description wrapped under its own column.
pub(super) fn help_rows(width: usize, enhanced: bool) -> Vec<String> {
    let mut rows = vec![format!("{BOLD}Commands{RESET} {DIM}type / to search them and your skills{RESET}")];
    let commands: Vec<(String, String, &str)> = COMMANDS
        .iter()
        .map(|c| {
            let aliases: String = c.aliases.iter().map(|a| format!(", /{a}")).collect();
            let args = if c.args.is_empty() { String::new() } else { format!(" {}", c.args) };
            (format!("/{}", c.name), format!("{aliases}{args}"), c.about)
        })
        .chain([("/<skill>".to_string(), " [request]".to_string(), "Run a skill")])
        .collect();
    let column = commands.iter().map(|(name, rest, _)| text::width(name) + text::width(rest)).max().unwrap_or(0);
    for (name, rest, about) in commands {
        let pad = " ".repeat(column - text::width(&name) - text::width(&rest));
        rows.extend(wrap_words(&format!("  {LAGOON}{name}{RESET}{DIM}{rest}{RESET}{pad}  {about}"), width, column + 4));
    }
    rows.push(String::new());
    rows.push(format!("{BOLD}Keys{RESET}"));
    let column = KEYS.iter().map(|(key, _)| text::width(key)).max().unwrap_or(0);
    for (key, about) in KEYS {
        let (key, about) = match *key {
            "Shift+Enter" if !enhanced => ("Ctrl+J", "new line; also \\ then Enter"),
            _ => (*key, *about),
        };
        rows.extend(wrap_words(&format!("  {BOLD}{key:<column$}{RESET}  {DIM}{about}{RESET}"), width, column + 4));
    }
    rows.push(String::new());
    rows
}

/// A recap as one block: its label, the text wrapped under it, and what asking for it cost.
pub(super) fn recap_rows(text: &str, note: &str, width: usize) -> Vec<String> {
    wrap_words(&format!("{BOLD}{LAGOON}recap{RESET} {} {DIM}· {note}{RESET}", text::sanitize_row(text)), width, 6)
}

/// What the empty prompt says it takes, in fg-faint: nothing once something is typed or while
/// a picker or a question `waits`, since those say it themselves.
pub(super) fn placeholder(empty: bool, busy: bool, waits: bool) -> Option<&'static str> {
    if !empty || waits {
        return None;
    }
    Some(if busy { "a message for the next step · Ctrl+S sends it now · /btw asks on the side" } else { "ask, / for commands and skills, @ for files" })
}

/// A full-width line around the prompt, in lagoon with `label` on it in plan mode.
pub(super) fn rule(label: Option<&str>, width: usize) -> String {
    match label {
        None => format!("{RULE}{}{RESET}", "─".repeat(width)),
        Some("") => format!("{LAGOON}{}{RESET}", "─".repeat(width)),
        Some(label) => {
            let rest = width.saturating_sub(text::width(label) + 4);
            format!("{LAGOON}── {BOLD}{label}{RESET}{LAGOON} {}{RESET}", "─".repeat(rest))
        }
    }
}

/// A full-width line with `title` on it in lagoon, above a list of choices.
pub(super) fn title_rule(title: &str, width: usize) -> String {
    let rest = width.saturating_sub(text::width(title) + 4);
    format!("{LAGOON}── {BOLD}{title}{RESET}{LAGOON} {}{RESET}", "─".repeat(rest))
}

/// A row of key hints under a list: each `(rank, hint)` in its place, the lowest ranks left out
/// first when the window is too narrow, so what stays reads whole.
pub(super) fn hint_row(hints: &[(u8, String)], width: usize) -> String {
    let mut keep: Vec<&(u8, String)> = hints.iter().filter(|(_, hint)| !hint.is_empty()).collect();
    let line = |keep: &[&(u8, String)]| format!("  {DIM}{}{RESET}", keep.iter().map(|(_, hint)| hint.as_str()).collect::<Vec<_>>().join(&dot()));
    while keep.len() > 1 && text::width(&line(&keep)) > width {
        let lowest = keep.iter().map(|(rank, _)| *rank).min().unwrap_or(0);
        let at = keep.iter().position(|(rank, _)| *rank == lowest).unwrap_or(0);
        keep.remove(at);
    }
    fit(&line(&keep), width)
}

/// `items` in rows of at most `width` columns, a faint dot between them and never one split
/// across two rows.
pub(super) fn wrap_items(items: &[String], width: usize) -> Vec<String> {
    let dot = format!(" {FAINT}·{RESET} ");
    let mut rows = Vec::new();
    let mut row = String::new();
    for item in items {
        let joined = if row.is_empty() { item.clone() } else { format!("{row}{dot}{item}") };
        if row.is_empty() || text::width(&joined) <= width {
            row = joined;
        } else {
            rows.push(std::mem::replace(&mut row, item.clone()));
        }
    }
    rows.push(row);
    rows.iter().map(|row| fit(row, width)).collect()
}

/// The rows of an edit's diff behind the output gutter: the line number, the sign, the text.
pub(super) fn diff_rows(lines: &[String], width: usize) -> Vec<String> {
    lines.iter().map(|line| diff_row(line, width, text::truecolor())).collect()
}

/// One row of a diff. With `stripes` an added or removed line sits on a soft stripe that runs to
/// the window's edge, its sign in lagoon or danger and its text left as it is; without, the sign
/// and the text take those colors. Context lines are muted either way.
pub(super) fn diff_row(line: &str, width: usize, stripes: bool) -> String {
    let Some((head, text)) = line.split_once('\t').filter(|(head, _)| head.len() > 1 && head.is_char_boundary(1)) else {
        return fit(&format!("  {FAINT}│{RESET} {DIM}{}{RESET}", text::sanitize(line)), width);
    };
    let (sign, number) = head.split_at(1);
    let text = text::sanitize(text);
    let ink = match sign {
        "+" => LAGOON.to_string(),
        "-" => DANGER.to_string(),
        _ => DIM.to_string(),
    };
    let ground = match sign {
        "+" if stripes => Some(ADD.to_string()),
        "-" if stripes => Some(DEL.to_string()),
        _ => None,
    };
    let Some(ground) = ground else {
        return fit(&format!("  {FAINT}│{RESET} {DIM}{number:>4}{RESET} {ink}{sign} {text}{RESET}"), width);
    };
    let row = fit(&format!("  {FAINT}│{RESET}{ground} {DIM}{number:>4}{RESET}{ground} {ink}{sign}{RESET}{ground} {text}"), width);
    format!("{row}{ground}{}{RESET}", " ".repeat(width.saturating_sub(text::width(&row))))
}

/// The row under a call's output that counts what was left out, naming Ctrl+O when `teach`.
pub(super) fn hidden_row(count: usize, which: &str, teach: bool) -> String {
    let key = if teach { " · Ctrl+O" } else { "" };
    format!("  {FAINT}│ … {count} {which} line{}{key}{RESET}", if count == 1 { "" } else { "s" })
}

/// A call that only keeps the task list, which is on screen already, gets no row of its own
/// unless it failed.
pub(super) fn is_bookkeeping(summary: &str, is_error: bool) -> bool {
    !is_error && (summary == "todo_write" || summary.starts_with("todo_write "))
}

/// A tool call's summary as its row draws it. The shell is the `$` of a prompt, because most
/// rows are bash calls and would all start with the same word.
pub(super) fn tool_text(summary: &str) -> String {
    match summary.strip_prefix("bash") {
        Some(rest) if rest.is_empty() || rest.starts_with(' ') => format!("${rest}"),
        _ => summary.to_string(),
    }
}

/// `tool_text` with the tool's name in bold, `read src/lib.rs`, or the `$` faint.
pub(super) fn tool_summary(summary: &str) -> String {
    let (name, rest) = match summary.split_once(' ') {
        Some((name, args)) => (name, format!(" {args}")),
        None => (summary, String::new()),
    };
    match name {
        "$" => format!("{FAINT}${RESET}{rest}"),
        "read" | "write" | "edit" => format!("{BOLD}{name}{RESET}{}", path_style(&rest)),
        _ => format!("{BOLD}{name}{RESET}{rest}"),
    }
}

/// A path with its folders muted and its file name as it is, so the eye lands on the file.
fn path_style(rest: &str) -> String {
    let (space, path) = rest.split_at(rest.len() - rest.trim_start().len());
    match path.rsplit_once('/') {
        Some((dir, file)) if !file.is_empty() => format!("{space}{DIM}{dir}/{RESET}{file}"),
        _ => rest.to_string(),
    }
}

pub(super) fn first_sentence(text: &str) -> String {
    let text = text.trim().replace('\n', " ");
    let end = text.find(['.', '。', '!', '?']).map_or(text.len(), |i| i + text[i..].chars().next().map_or(1, char::len_utf8));
    crate::llm::truncate(&text[..end], 100).to_string()
}

/// A banner label, dimmed and padded so the values beside it line up.
pub(super) fn label(name: &str) -> String {
    format!("{DIM}{name:<6}{RESET} ")
}

/// The banner's rows under the model and folder, in the same column: the instruction files and
/// what they cost, skills, MCP servers, a custom prompt and what is noted for later.
pub(super) fn overview_rows(overview: &Overview) -> Vec<String> {
    let mut rows = Vec::new();
    if !overview.instructions.is_empty() {
        let names: Vec<String> = overview.instructions.iter().map(|path| tilde(path)).collect();
        rows.push(format!("{}{} {DIM}·{RESET} {TOKENS}~{}{RESET} {DIM}tokens{RESET}", label("rules"), names.join(", "), short(overview.instruction_tokens as u64)));
    }
    if overview.skills > 0 {
        rows.push(format!("{}{}", label("skills"), overview.skills));
    }
    if !overview.mcp.is_empty() {
        rows.push(format!("{}{}", label("mcp"), overview.mcp.join(", ")));
    }
    if overview.custom_prompt {
        rows.push(format!("{}custom {DIM}· [prompt] in config.toml{RESET}", label("prompt")));
    }
    if overview.later > 0 {
        rows.push(format!("{}{} noted {DIM}·{RESET} {LAGOON}/later{RESET}", label("later"), overview.later));
    }
    rows
}

/// The keys worth knowing, each in lagoon with what it does dimmed beside it. Shift+Enter
/// starts a new line only when the terminal speaks the kitty keyboard protocol; Ctrl+J does.
pub(super) fn key_hints(enhanced: bool) -> Vec<String> {
    let newline = if enhanced { "Shift+Enter" } else { "Ctrl+J" };
    [("/", "commands and skills"), ("@", "files"), (newline, "new line"), ("Shift+Tab", "plan mode"), ("Esc", "interrupts")]
        .iter()
        .map(|(key, what)| format!("{LAGOON}{key}{RESET} {DIM}{what}{RESET}"))
        .collect()
}

/// `path` cut from the left to fit `room` columns, at a folder boundary where there is one:
/// `…/code/killi`, not `…hnet.com.sg/code/killi`.
pub(super) fn shorten_path(path: &str, room: usize) -> String {
    if text::width(path) <= room {
        return path.to_string();
    }
    let tail: String = path.chars().rev().take(room.saturating_sub(1)).collect::<Vec<_>>().into_iter().rev().collect();
    match tail.find('/') {
        Some(at) => format!("…{}", &tail[at..]),
        None => format!("…{tail}"),
    }
}

/// `path` with the home folder as `~`.
pub(super) fn tilde(path: &Path) -> String {
    match path.strip_prefix(crate::config::home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// The text `/session` copies: enough for another agent to find the log and know how to
/// read it, with the session's numbers and prompts so the reader knows what it is looking at.
#[allow(clippy::too_many_arguments)]
pub(super) fn handoff(
    id: &str,
    log: &Path,
    cwd: &str,
    model: &str,
    effort: &str,
    prompts: &[String],
    stats: &crate::agent::Stats,
    later: Option<(PathBuf, usize)>,
) -> String {
    let usage = &stats.usage;
    let cached = usage.cached * 100 / usage.prompt.max(1);
    let plural = |n: usize, word: &str| crate::tools::count(n, word);
    let mut out = vec![
        format!("killi session {id} · {cwd}"),
        format!("log: {}", log.display()),
        format!(
            "{model} · effort {effort} · {} · {} · {} · {} in, {cached}% cached · {} out",
            plural(prompts.len(), "prompt"),
            plural(stats.requests as usize, "request"),
            plural(stats.tool_calls as usize, "tool call"),
            short(usage.prompt),
            short(usage.completion)
        ),
    ];
    if let Some((path, open)) = later {
        out.push(format!("later: {} ({open} open: what the agent noted to do later)", path.display()));
    }
    for (i, prompt) in prompts.iter().enumerate() {
        out.push(format!("{}. {}", i + 1, crate::llm::truncate(&prompt.split_whitespace().collect::<Vec<_>>().join(" "), 160)));
    }
    out.push(
        "The log is JSON lines, one record per `type`: `session` (cwd, model, effort, tools), every `message` exactly as sent \
         (user, assistant with tool_calls, tool results; a tool result's `tool` has is_error, brief and ms), `usage` after each \
         request (prompt, cached, completion and reasoning tokens, finish_reason, ms), `model` when the model or effort \
         changed, `backup` when this session's backup model was set or cleared, and `compaction`. Every record has a `ts` in ms. \
         `scripts/sessions.py` in the killi repo summarizes logs and flags what to fix."
            .to_string(),
    );
    out.join("\n")
}

/// The words shown while the turn's opening snapshot (the undo point taken on a blocking
/// thread) runs, so the internal word never shows: long Renaissance words, one per turn,
/// each held whole for a while before the next takes its place.
const SNAPSHOT_WORDS: [&str; 4] = ["chiaroscuro", "contrapposto", "illuminating", "perspective"];
/// Tenths of a second each snapshot word holds: a backstop for a snapshot outliving turns,
/// since the word normally advances once per snapshot, not on a timer.
const WORD_FRAMES: usize = 100;

/// The snapshot word `seq` snapshots in and `frame` tenths of a second into the turn: one
/// whole Renaissance word, advancing one per turn like Claude Code's spinner verbs.
pub(super) fn snapshot_word(seq: usize, frame: usize) -> String {
    SNAPSHOT_WORDS[(seq + frame / WORD_FRAMES) % SNAPSHOT_WORDS.len()].to_string()
}

/// The working rows while the snapshot runs: the wave and the whole snapshot word with the
/// logo's gradient flowing through it, then the elapsed time and the interrupt hint below.
pub(super) fn snapshot_rows(seq: usize, frame: usize, elapsed: Duration) -> Vec<String> {
    vec![
        banner::steady(frame, &snapshot_word(seq, frame)),
        format!("{BOLD}{}{RESET} {FAINT}·{RESET} {LAGOON}Esc{RESET} {DIM}to interrupt{RESET}", clock(elapsed)),
    ]
}

/// The working rows: the wave and the turn's action word (`thinking`, `writing`, …) with
/// what it is doing after it, plain; then the turn's elapsed time and the interrupt hint on
/// their own row, left-aligned with the wave. While a failed request waits to be
/// retried the first row counts that wait down instead, since nothing is working then.
pub(super) fn working_rows(frame: usize, elapsed: Duration, action: &str, detail: &str) -> Vec<String> {
    let first = match detail.is_empty() {
        true => banner::action(frame, action),
        false => format!("{} {detail}", banner::action(frame, action)),
    };
    let second = format!("{BOLD}{}{RESET} {FAINT}·{RESET} {LAGOON}Esc{RESET} {DIM}to interrupt{RESET}", clock(elapsed));
    vec![first, second]
}

/// A message queued while the agent works, saying Ctrl+S can send it now instead of
/// waiting for the next step.
pub(super) fn queued_row(queued: &str) -> String {
    format!("{FIN}❯{RESET} {DIM}{} (sent at the next step · Ctrl+S sends it now){RESET}", text::sanitize(queued).replace('\n', " "))
}
