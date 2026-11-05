use super::start::{ENHANCED, RESUMED, restore_terminal, take_terminal};
use super::*;
use crate::tools::{MAX_BYTES, MAX_LINES, OwnSession};

/// The shell command in `text`, when it starts with `!`: the rest of the line, or empty
/// for a bare `!`, which opens an interactive shell instead of running one command.
pub(super) fn shell_arg(text: &str) -> Option<String> {
    let rest = text.trim().strip_prefix('!')?;
    Some(rest.trim_start().to_string())
}

impl App {
    /// `!cmd` while the agent works: runs now without interrupting the turn. True when
    /// the prompt was one. A bare `!` waits for the turn to end, as an interactive
    /// shell needs the terminal the running turn draws on.
    pub(super) fn shell_while_busy(&mut self) -> bool {
        let Some(cmd) = shell_arg(self.editor.text()) else {
            return false;
        };
        let taken = self.editor.take();
        debug_assert!(shell_arg(&taken).is_some());
        if cmd.trim().is_empty() {
            self.notice("! alone opens a shell once the agent is idle; !cmd runs now.");
            return true;
        }
        run_shell_once(self, &cmd);
        true
    }
}

/// Runs `cmd` in the working directory and prints its output like a bash tool call:
/// one `$` row, a short preview, the rest behind Ctrl+O. Never sent to the model.
pub(super) fn run_shell_once(app: &mut App, cmd: &str) {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return;
    }
    let started = Instant::now();
    let out = std::process::Command::new("/bin/bash")
        .arg("-c")
        .arg(format!("exec 2>&1\n{cmd}"))
        .current_dir(&app.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env(crate::live::PID_VAR, std::process::id().to_string())
        .own_session()
        .output();
    let out = match out {
        Ok(out) => out,
        Err(e) => return app.error(&format!("Couldn't run `{cmd}`: {e}")),
    };
    let first = cmd.lines().next().unwrap_or(cmd);
    let summary = format!("bash {}", crate::llm::truncate(first, 120));
    let output = truncate_shell_output(&String::from_utf8_lossy(&out.stdout));
    let lines = match output.lines().count() {
        _ if output == NO_OUTPUT => "no output".to_string(),
        1 => "1 line".to_string(),
        n => format!("{n} lines"),
    };
    let ok = out.status.success();
    let brief = match out.status.code() {
        Some(0) => lines,
        Some(code) => format!("{lines} · exit {code}"),
        None => format!("{lines} · killed"),
    };
    let output = text::sanitize(&output).into_owned();
    app.tool_line(None, &summary, !ok, &brief, Some(started.elapsed()));
    app.output_preview(&output, !ok);
    app.push_detail(summary, output);
}

/// What a command that printed nothing shows in the transcript.
const NO_OUTPUT: &str = "(no output)";

/// The output as shown: all of it within budgets, else its last lines, so a huge
/// command can't grow memory beyond the transcript's share of it.
fn truncate_shell_output(output: &str) -> String {
    let output = output.trim_end_matches('\n');
    if output.is_empty() {
        return NO_OUTPUT.into();
    }
    let total = output.bytes().filter(|&b| b == b'\n').count() + 1;
    if total <= MAX_LINES && output.len() <= MAX_BYTES {
        return output.to_string();
    }
    let (mut kept, mut bytes) = (0, 0);
    for line in output.rsplit('\n') {
        let next = line.len() + 1;
        if kept == MAX_LINES || (bytes + next > MAX_BYTES && kept > 0) {
            break;
        }
        bytes += next;
        kept += 1;
    }
    let shown = &output[output.len() - (bytes - 1)..];
    format!(
        "[Showing lines {}-{total} of {total}; truncated at {MAX_LINES} lines / 50KB]\n{shown}",
        total - kept + 1
    )
}

/// Opens the user's shell in the working directory: typing `exit` there returns to
/// killi. The terminal goes back first and is taken again after, as for Ctrl+Z.
pub(super) fn run_interactive_shell(app: &mut App) -> Result<()> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let (fullscreen, enhanced) = (
        app.fullscreen,
        ENHANCED.load(Ordering::Relaxed),
    );
    restore_terminal();
    println!(
        "Opened {shell} in {}. Type `exit` to return to killi.",
        app.cwd.display()
    );
    let status = std::process::Command::new(&shell)
        .current_dir(&app.cwd)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status();
    take_terminal(fullscreen, enhanced)?;
    // The shell wrote below the erased live area; draw afresh where the cursor is.
    RESUMED.store(true, Ordering::Relaxed);
    app.shown.clear();
    match status {
        Ok(status) if status.success() => app.notice(&format!("Back from {shell}.")),
        Ok(status) => app.notice(&format!(
            "Back from {shell} ({}).",
            status
                .code()
                .map_or_else(|| "killed".to_string(), |code| format!("exit {code}"))
        )),
        Err(e) => app.error(&format!("Couldn't open {shell}: {e}")),
    }
    Ok(())
}
