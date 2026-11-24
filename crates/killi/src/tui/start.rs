use super::*;
pub(super) use crate::crash::crash_line;

/// Whether to take the whole screen: `[ui] fullscreen` when set, else in Kil, which sends the
/// wheel to the program and leaves selecting text to itself.
pub(super) fn wants_fullscreen(setting: Option<bool>, terminal: &str) -> bool {
    setting.unwrap_or(terminal == "kil")
}

/// Asks the terminal for its background and foreground colors (OSC 11 and OSC 10) and waits up
/// to 200 ms for the answers, so a light terminal gets the light palette and every theme gets
/// grays, tints and stripes that match it, without `COLORFGBG` or `[ui] theme`. Nothing when
/// stdin isn't a terminal or it doesn't answer for the background; `[ui] theme` skips the
/// question. A terminal that answers the background but not the foreground costs 40 ms more.
///
/// A sentinel question after these, such as the primary device attributes, would end the wait
/// sooner on a terminal that knows no colors, but not every terminal answers in order: Kil
/// writes its answer to that one first and the colors after, which would leave them to be typed
/// into the prompt.
pub(super) fn query_ground() -> Option<Ground> {
    use std::io::{IsTerminal, Read};
    if !std::io::stdin().is_terminal() || !stdout().is_terminal() {
        return None;
    }
    terminal::enable_raw_mode().ok()?;
    let answer = (|| {
        let mut out = stdout();
        write!(out, "\x1b]11;?\x1b\\\x1b]10;?\x1b\\").ok()?;
        out.flush().ok()?;
        let mut reply = Vec::new();
        let mut limit = Instant::now() + Duration::from_millis(200);
        let mut waited_for_foreground = false;
        loop {
            let left = limit.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let mut poll = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `poll` points at one pollfd that lives for the whole call.
            if unsafe { libc::poll(&mut poll, 1, left.as_millis() as i32) } <= 0 {
                break;
            }
            let mut bytes = [0u8; 64];
            let n = std::io::stdin().lock().read(&mut bytes).ok()?;
            if n == 0 {
                break;
            }
            reply.extend_from_slice(&bytes[..n]);
            let seen = String::from_utf8_lossy(&reply);
            if parse_background(&seen).is_some() {
                if parse_foreground(&seen).is_some() {
                    break;
                }
                if !std::mem::replace(&mut waited_for_foreground, true) {
                    limit = limit.min(Instant::now() + Duration::from_millis(40));
                }
            }
        }
        parse_ground(&String::from_utf8_lossy(&reply))
    })();
    let _ = terminal::disable_raw_mode();
    answer
}

/// The terminal's answers to the color queries, the background being the one that must be there.
pub(super) fn parse_ground(reply: &str) -> Option<Ground> {
    Some(Ground {
        background: parse_background(reply)?,
        foreground: parse_foreground(reply),
    })
}

/// The color in a terminal's OSC 11 answer, `rgb:rrrr/gggg/bbbb` with 1 to 4 hex digits per
/// channel, scaled to 8 bits; None until its terminator has arrived.
pub(super) fn parse_background(reply: &str) -> Option<(u8, u8, u8)> {
    parse_osc_color(reply, 11)
}

/// The color in a terminal's OSC 10 answer, in the same form.
pub(super) fn parse_foreground(reply: &str) -> Option<(u8, u8, u8)> {
    parse_osc_color(reply, 10)
}

fn parse_osc_color(reply: &str, code: u8) -> Option<(u8, u8, u8)> {
    let marker = format!("\x1b]{code};");
    let start = reply.find(&marker)? + marker.len();
    let body = reply[start..].strip_prefix("rgb:")?;
    let end = body.find(['\x07', '\x1b'])?;
    let channel = |hex: &str| {
        let value = u32::from_str_radix(hex, 16).ok()?;
        Some(match hex.len() {
            1 => (value * 17) as u8,
            n @ 2..=4 => (value >> (4 * (n - 2))) as u8,
            _ => return None,
        })
    };
    let mut channels = body[..end].split('/').map(channel);
    let (r, g, b) = (channels.next()??, channels.next()??, channels.next()??);
    Some((r, g, b))
}

/// The terminal's title as last set, so an unchanged repaint writes nothing.
static TITLE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Sets the terminal's title (OSC 0) for hosts that read it: an agent manager's tabs and
/// status dots. Control bytes are dropped so a model name or path can't close the sequence.
/// The title of a title-less host is its own business; the escape is harmless elsewhere.
pub(super) fn set_title(text: &str) {
    let mut last = TITLE.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_deref() == Some(text) {
        return;
    }
    *last = Some(text.to_string());
    let text: String = text
        .chars()
        .filter(|c| !matches!(c, '\u{1b}' | '\u{7}' | '\n' | '\r'))
        .collect();
    let mut out = stdout();
    let _ = write!(out, "\x1b]0;{text}\x07");
    let _ = out.flush();
}

/// Whether the terminal is already gone: hung up, or a read gives back nothing or fails. A
/// pty closed during startup would otherwise leave crossterm's reader spinning on EOF at 100%
/// CPU, ignoring SIGHUP and SIGTERM while a blocking read holds the runtime's thread.
pub(super) fn terminal_gone(fd: libc::c_int) -> bool {
    if fd < 0 {
        return true;
    }
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLHUP,
        revents: 0,
    };
    // SAFETY: `poll` points at one pollfd that lives for the whole call.
    if unsafe { libc::poll(&mut poll, 1, 0) } < 0 {
        return true;
    }
    // A hangup is gone even when input is still buffered: probing with a read would eat a
    // byte of it, and the reader would spin on EOF once the buffer drains anyway.
    (poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0
}

/// The command that reopens the session logged at `log`, quoted for the shell when needed.
pub(super) fn resume_command(log: &Path) -> String {
    let path = log.to_string_lossy();
    let plain = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-@+:,=".contains(c));
    if plain {
        format!("killi -r {path}")
    } else {
        format!("killi -r '{}'", path.replace('\'', r"'\''"))
    }
}

/// Raw mode and bracketed paste while the UI runs, and the alternate screen with the mouse in
/// fullscreen mode, restored when it ends.
pub(super) struct TerminalGuard;

/// Whether the UI pushed keyboard enhancement flags, which `restore_terminal` pops.
pub(super) static ENHANCED: AtomicBool = AtomicBool::new(false);

/// Whether the UI is on the alternate screen with mouse reports on, which `restore_terminal`
/// ends.
static FULLSCREEN: AtomicBool = AtomicBool::new(false);

/// The inline live area's cursor row, so `suspend_cleanly` can erase the area.
pub(super) static CURSOR_ROW: AtomicUsize = AtomicUsize::new(0);

/// Set once killi was stopped and continued: the next resize draws the inline live area where
/// the cursor is, since the shell wrote below the erased one meanwhile.
pub(super) static RESUMED: AtomicBool = AtomicBool::new(false);

/// The write end of the pipe a stop signal's handler pokes.
static SUSPEND_PIPE: AtomicI32 = AtomicI32::new(-1);

/// Woken once killi is continued, so an idle prompt repaints without waiting for a key:
/// crossterm stops surfacing SIGWINCH after a stop, so the resume can't lean on a resize event.
static RESUMED_NOTIFY: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Awaits the next continue. A free function so it borrows nothing of `App` and can share a
/// `select!` with the futures that do.
pub(super) async fn resumed() {
    RESUMED_NOTIFY.notified().await;
}

impl TerminalGuard {
    pub(super) fn enter(fullscreen: bool) -> Result<Self> {
        terminal::enable_raw_mode()?;
        let enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false);
        take_terminal(fullscreen, enhanced)?;
        install_crash_hook();
        suspend_cleanly();
        Ok(Self)
    }
}

/// Puts the terminal in the state the UI needs: raw mode, bracketed paste, disambiguated
/// escape codes when the terminal knows them, and in fullscreen mode the alternate screen with
/// button, wheel and drag reports (1002, which reports the pointer only while a button is held)
/// in the SGR form (1006); crossterm's own `EnableMouseCapture` also asks for 1003, a report of
/// every pointer move.
pub(super) fn take_terminal(fullscreen: bool, enhanced: bool) -> Result<()> {
    terminal::enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnableBracketedPaste)?;
    if enhanced {
        execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        ENHANCED.store(true, Ordering::Relaxed);
    }
    if fullscreen {
        execute!(
            out,
            EnterAlternateScreen,
            Clear(ClearType::All),
            MoveTo(0, 0)
        )?;
        write!(out, "\x1b[?1002h\x1b[?1006h")?;
        out.flush()?;
        FULLSCREEN.store(true, Ordering::Relaxed);
    }
    Ok(())
}

extern "C" fn on_suspend_signal(_: libc::c_int) {
    let fd = SUSPEND_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: a one-byte write to a pipe this process opened; async-signal-safe.
        unsafe { libc::write(fd, [0u8].as_ptr().cast(), 1) };
    }
}

/// Being stopped gives the terminal back first, and takes it again when killi is continued, so
/// a suspended killi never leaves the shell in raw mode with the mouse reporting, where the
/// wheel would type `65;48;33M` into the prompt. The reported SIGTTIN came from a child that
/// took the terminal, which `OwnSession` now prevents; this covers the remaining path, an
/// explicit `kill -TSTP` (Ctrl+Z is a key in raw mode, not a signal). SIGTTOU is ignored so
/// taking the terminal again never stops killi should the shell not have handed it back yet.
/// A handler only pokes a pipe; a thread does the work, since restoring the terminal takes a
/// lock a signal handler must not wait on. It erases the inline live area, restores the
/// terminal, stops the whole process with SIGSTOP, and once continued takes the terminal again
/// and raises SIGWINCH, which reaches the UI as a resize and paints everything afresh.
fn suspend_cleanly() {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe fills the two descriptors; fcntl acts on one this function owns.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    let [reader, writer] = fds;
    // SAFETY: as above.
    unsafe { libc::fcntl(writer, libc::F_SETFL, libc::O_NONBLOCK) };
    SUSPEND_PIPE.store(writer, Ordering::Relaxed);
    // SAFETY: a zeroed sigaction is valid; the handler only writes a byte to the pipe, and
    // ignoring SIGTTOU keeps a terminal write off the background from stopping killi.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_suspend_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGTSTP, &action, std::ptr::null_mut());

        let mut ignore: libc::sigaction = std::mem::zeroed();
        ignore.sa_sigaction = libc::SIG_IGN;
        libc::sigemptyset(&mut ignore.sa_mask);
        libc::sigaction(libc::SIGTTOU, &ignore, std::ptr::null_mut());
    }
    std::thread::spawn(move || {
        // SAFETY: the set holds only SIGTSTP, so its handler runs on the main thread, not here.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTSTP);
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        }
        let mut bytes = [0u8; 16];
        // SAFETY: reads into a buffer of the given length.
        let mut read = || unsafe { libc::read(reader, bytes.as_mut_ptr().cast(), bytes.len()) };
        loop {
            let n = read();
            if n == 0
                || (n < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted)
            {
                return;
            }
            if n < 0 {
                continue;
            }
            let taken = terminal::is_raw_mode_enabled().unwrap_or(false);
            let (fullscreen, enhanced) = (
                FULLSCREEN.load(Ordering::Relaxed),
                ENHANCED.load(Ordering::Relaxed),
            );
            if taken && !fullscreen {
                let mut out = stdout();
                let row = CURSOR_ROW.load(Ordering::Relaxed);
                if row > 0 {
                    let _ = queue!(out, MoveUp(row as u16));
                }
                let _ = execute!(out, MoveToColumn(0), Clear(ClearType::FromCursorDown));
            }
            restore_terminal();
            // SAFETY: stops this process; the shell continues it with `fg`.
            unsafe { libc::kill(libc::getpid(), libc::SIGSTOP) };
            // Continued. A poke that arrived before the stop is stale.
            loop {
                let mut poll = libc::pollfd {
                    fd: reader,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: `poll` points at one pollfd that lives for the whole call.
                if unsafe { libc::poll(&mut poll, 1, 0) } <= 0 || read() <= 0 {
                    break;
                }
            }
            if taken {
                let _ = take_terminal(fullscreen, enhanced);
                RESUMED.store(true, Ordering::Relaxed);
                // A tick repaints a turn in flight; this wakes an idle prompt to repaint now.
                RESUMED_NOTIFY.notify_one();
            }
        }
    });
}

/// Ends mouse reports and leaves the alternate screen, if the UI is on it.
pub(super) fn leave_fullscreen(out: &mut impl Write) {
    if FULLSCREEN.swap(false, Ordering::Relaxed) {
        let _ = write!(out, "\x1b[?1006l\x1b[?1002l");
        let _ = execute!(out, LeaveAlternateScreen);
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Ends what killi started, leaves presence, notes the crash in the session log, then gives
/// the terminal back, before the previous hook reports: with `panic = "abort"` no destructor
/// runs, so a panic would otherwise leave children running and log nothing.
pub fn install_crash_hook() {
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crate::tools::kill_all();
        crate::live::leave();
        crate::crash::append_record(|ts| crash_line(&panic_message(info), ts));
        restore_terminal();
        report(info);
    }));
}

/// What the panic says, for the crash record, and where in the source it happened: the
/// location survives a stripped binary.
fn panic_message(info: &std::panic::PanicHookInfo) -> String {
    let message = if let Some(text) = info.payload().downcast_ref::<&str>() {
        text.to_string()
    } else if let Some(text) = info.payload().downcast_ref::<String>() {
        text.clone()
    } else {
        return info.to_string();
    };
    located(message, info.location())
}

/// `message` with the source location it came from, when there is one.
pub(super) fn located(message: String, at: Option<&std::panic::Location>) -> String {
    match at {
        Some(at) => format!("{message} (at {}:{}:{})", at.file(), at.line(), at.column()),
        None => message,
    }
}

/// Gives the terminal back as it was before the UI took it; also on quitting from a signal,
/// when the guard is never dropped.
pub fn restore_terminal() {
    if !terminal::is_raw_mode_enabled().unwrap_or(false) {
        return;
    }
    let mut out = stdout();
    leave_fullscreen(&mut out);
    if ENHANCED.swap(false, Ordering::Relaxed) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableBracketedPaste, Show);
    let _ = terminal::disable_raw_mode();
    let _ = writeln!(out);
}
