use super::*;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result};

/// The first-launch guide: on the first interactive start, offers to add another command
/// beside the `killi` binary (a symlink such as `dw` -> `killi`). Pressing Enter, or typing
/// `killi`, keeps the default. Invalid or already-taken names are explained and asked again;
/// choosing one records the choice in `~/.killi/command-guide.json` so it is asked only once.
/// Nothing is overwritten: an existing command with that name is left alone.
pub fn command_guide_if_first() -> Result<()> {
    let path = killi_home().join("command-guide.json");
    if path.exists() || !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(());
    }
    let invoked = std::env::args().next().and_then(|arg| {
        PathBuf::from(arg)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    if invoked.as_deref() != Some("killi") {
        return Ok(());
    }
    let exe = fs::canonicalize(std::env::current_exe()?)?;
    if exe.file_name().is_none_or(|name| name != "killi") {
        return Ok(());
    }
    let chosen = choose_command(&exe)?;
    fs::create_dir_all(killi_home())?;
    fs::write(
        &path,
        format!("{}\n", serde_json::json!({ "command": chosen })),
    )
    .with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

fn choose_command(exe: &Path) -> Result<String> {
    eprintln!(
        "Welcome to killi. You can start it as `killi`, or pick a shorter command such as `dw`."
    );
    loop {
        eprint!("Start command [killi]: ");
        std::io::stderr().flush()?;
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer)? == 0 {
            return Ok("killi".to_string());
        }
        let name = answer.trim();
        if name.is_empty() || name == "killi" {
            return Ok("killi".to_string());
        }
        if !valid_command_name(name) {
            eprintln!(
                "`{name}` can't be a command: use 1-32 letters, digits, `-` or `_`, starting with a letter, digit or `_`. Try another, or press Enter to keep `killi`."
            );
            continue;
        }
        match create_command_link(exe, name) {
            Ok(link) => {
                eprintln!(
                    "Created {} -> killi. Start with `{name}` or `killi`.",
                    link.display()
                );
                return Ok(name.to_string());
            }
            Err(reason) => eprintln!("{reason} Try another name, or press Enter to keep `killi`."),
        }
    }
}

pub(super) fn valid_command_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric() || first == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Links `name`, beside `exe`, to the binary's file name. A link that already points at `exe`
/// counts as done; any other existing file or command is reported, never replaced.
pub(super) fn create_command_link(exe: &Path, name: &str) -> std::result::Result<PathBuf, String> {
    let dir = exe
        .parent()
        .ok_or_else(|| format!("{} has no folder", exe.display()))?;
    let target = exe
        .file_name()
        .ok_or_else(|| format!("{} has no file name", exe.display()))?;
    let link = dir.join(name);
    match fs::symlink_metadata(&link) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                && let Ok(existing) = fs::read_link(&link)
            {
                let resolved = if existing.is_absolute() {
                    existing
                } else {
                    dir.join(existing)
                };
                if fs::canonicalize(&resolved).is_ok_and(|path| path == exe) {
                    return Ok(link);
                }
            }
            return Err(format!(
                "{} already exists and was not changed.",
                link.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not check {}: {error}.", link.display())),
    }
    if let Some(existing) = command_on_path(name, dir) {
        return Err(format!(
            "`{name}` already runs {}; it was not changed.",
            existing.display()
        ));
    }
    std::os::unix::fs::symlink(target, &link)
        .map_err(|error| format!("could not create {}: {error}.", link.display()))?;
    Ok(link)
}

fn command_on_path(name: &str, except: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .filter(|dir| dir != except)
            .map(|dir| dir.join(name))
            .find(|candidate| {
                fs::metadata(candidate).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
    })
}

/// Asks a yes/no question on the terminal; Enter means yes, end of input means no, so a
/// guide the user closes never writes anything.
pub(crate) fn confirm(question: &str) -> Result<bool> {
    Ok(agree(&read_line_opt(&format!("{question} [Y/n] "))?))
}

/// Enter or yes; anything else, including end of input, is no.
pub(super) fn agree(answer: &Option<String>) -> bool {
    matches!(
        answer.as_deref().map(str::to_ascii_lowercase).as_deref(),
        Some("" | "y" | "yes")
    )
}

/// Prints `prompt` and reads one line from the terminal, trimmed; end of input reads as no
/// answer.
pub(crate) fn read_line(prompt: &str) -> Result<String> {
    Ok(read_line_opt(prompt)?.unwrap_or_default())
}

/// Prints `prompt` and reads one line from the terminal, trimmed; None on end of input.
fn read_line_opt(prompt: &str) -> Result<Option<String>> {
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer)? == 0 {
        return Ok(None);
    }
    Ok(Some(answer.trim().to_string()))
}

/// Reads one line with the terminal's echo off, so a pasted key stays out of the scrollback.
/// Ctrl+C cancels the paste: with signals off for the read it arrives as a byte, and reads as
/// no answer rather than ending killi with the echo still off.
pub(crate) fn read_secret(prompt: &str) -> Result<String> {
    let mut settings = unsafe { std::mem::zeroed::<libc::termios>() };
    // SAFETY: stdin is a terminal here and `settings` is initialized before it is read into.
    let terminal = unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut settings) } == 0;
    let saved = settings;
    if terminal {
        settings.c_lflag &= !(libc::ECHO | libc::ISIG);
        // SAFETY: the settings were just read from this terminal.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &settings) };
    }
    let answer = read_line(prompt);
    if terminal {
        // SAFETY: `saved` is this terminal's own settings, read before the change.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved) };
        eprintln!();
    }
    let answer = answer?;
    Ok(if answer.contains('\u{3}') {
        String::new()
    } else {
        answer
    })
}
