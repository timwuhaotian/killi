//! Styled terminal text: display width, wrapping, and a line-at-a-time Markdown styler that
//! works while text is still streaming in.

use std::borrow::Cow;
use std::fmt;
use std::sync::OnceLock;

use super::palette::{self, Ground, Role};

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const ITALIC: &str = "\x1b[3m";
pub const UNDERLINE: &str = "\x1b[4m";
pub const STRIKE: &str = "\x1b[9m";

/// The terminal's background: it decides which shade each color takes.
#[derive(Clone, Copy, PartialEq)]
enum Theme {
    Dark,
    Light,
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// Fixes the theme for the session: `"dark"` or `"light"`, anything else goes by the
/// terminal's `ground` when it answered the color queries, else by `COLORFGBG`, which most
/// terminals that have a light background set. A ground also gives the grays, tints and stripes
/// their shades (see `palette`); a pinned theme has none, and keeps the fixed shades.
pub fn init_theme(setting: Option<&str>, ground: Option<Ground>) {
    let pinned = matches!(setting, Some("dark" | "light"));
    let ground = ground.filter(|_| !pinned);
    let theme = match setting {
        Some("dark") => Theme::Dark,
        Some("light") => Theme::Light,
        _ => ground.map_or_else(detect_theme, |ground| theme_for(ground.background)),
    };
    let _ = THEME.set(theme);
    palette::init(ground);
}

/// The theme for a background color: light when it reflects more than half the light.
fn theme_for((r, g, b): (u8, u8, u8)) -> Theme {
    let luminance = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b);
    if luminance > 127.5 { Theme::Light } else { Theme::Dark }
}

fn detect_theme() -> Theme {
    let background = std::env::var("COLORFGBG").ok().and_then(|v| v.rsplit(';').next().and_then(|bg| bg.parse::<u8>().ok()));
    match background {
        Some(7 | 9..=15) => Theme::Light,
        _ => Theme::Dark,
    }
}

/// Whether the session runs on a light terminal.
pub fn is_light() -> bool {
    *THEME.get_or_init(detect_theme) == Theme::Light
}

/// Whether the terminal takes 24-bit colors, as `COLORTERM` announces.
pub fn truecolor() -> bool {
    static TRUECOLOR: OnceLock<bool> = OnceLock::new();
    *TRUECOLOR.get_or_init(|| std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit"))
}

/// A color escape that comes out in the shade for the current theme when it is printed: the
/// design token itself on a truecolor terminal, else its nearest 256-color shade. A derived
/// role takes the shade `palette` made from the terminal's ground when there is one.
#[derive(Clone, Copy)]
pub struct Sgr {
    dark: &'static str,
    light: &'static str,
    dark_true: &'static str,
    light_true: &'static str,
    derived: Option<Role>,
}

impl Sgr {
    const fn fixed(dark: &'static str, light: &'static str, dark_true: &'static str, light_true: &'static str) -> Sgr {
        Sgr { dark, light, dark_true, light_true, derived: None }
    }

    /// A role that follows the ground; the fixed shades are for when the terminal gave none.
    const fn derived(role: Role, dark: &'static str, light: &'static str, dark_true: &'static str, light_true: &'static str) -> Sgr {
        Sgr { dark, light, dark_true, light_true, derived: Some(role) }
    }

    fn shade(&self, theme: Theme, truecolor: bool) -> &'static str {
        match (theme, truecolor) {
            (Theme::Dark, true) => self.dark_true,
            (Theme::Dark, false) => self.dark,
            (Theme::Light, true) => self.light_true,
            (Theme::Light, false) => self.light,
        }
    }
}

impl fmt::Display for Sgr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.derived {
            // Colors are about to be stripped; what recedes keeps receding, as dim.
            Some(Role::Faint | Role::Rule | Role::Muted) if no_color() => return f.write_str("\x1b[2m"),
            Some(role) if !no_color() => {
                if let Some(palette) = palette::get()
                    && let Some(written) = palette.write(f, role, truecolor())
                {
                    return written;
                }
            }
            _ => {}
        }
        f.write_str(self.shade(*THEME.get_or_init(detect_theme), truecolor()))
    }
}

// The roles of the design system (`docs/killi-design-system.html`): each token's exact color
// where the terminal shows 24-bit color, else its nearest 256-color shade.

/// Killi: running work, the active mode, commands, links and the selected row's mark.
pub const LAGOON: Sgr = Sgr::fixed("\x1b[38;5;80m", "\x1b[38;5;23m", "\x1b[38;2;79;209;197m", "\x1b[38;2;10;110;101m");
/// The human: your messages, the prompt, questions and anything else waiting on you.
pub const FIN: Sgr = Sgr::fixed("\x1b[38;5;209m", "\x1b[38;5;130m", "\x1b[38;2;255;140;90m", "\x1b[38;2;173;63;20m");
/// Retries, a context window nearly full, an interrupted turn.
pub const WARN: Sgr = Sgr::fixed("\x1b[38;5;221m", "\x1b[38;5;94m", "\x1b[38;2;242;193;78m", "\x1b[38;2;138;90;0m");
/// Failed tool calls, errors and removed lines.
pub const DANGER: Sgr = Sgr::fixed("\x1b[38;5;204m", "\x1b[38;5;124m", "\x1b[38;2;255;111;125m", "\x1b[38;2;179;38;30m");
/// Marks the eye may skip: a finished call's tick, gutters, hints, the empty part of a meter.
pub const FAINT: Sgr = Sgr::derived(Role::Faint, "\x1b[38;5;242m", "\x1b[38;5;245m", "\x1b[38;2;92;107;122m", "\x1b[38;2;117;131;138m");
/// Hairlines: the rules around the prompt. As faint as a mark until the ground is known.
pub const RULE: Sgr = Sgr::derived(Role::Rule, "\x1b[38;5;242m", "\x1b[38;5;245m", "\x1b[38;2;92;107;122m", "\x1b[38;2;117;131;138m");
/// Secondary text. SGR dim is the fallback, not the design: Kil and Alacritty scale each
/// channel to two thirds, which makes dim text darker than body text on a light theme.
pub const DIM: Sgr = Sgr::derived(Role::Muted, "\x1b[2m", "\x1b[2m", "\x1b[2m", "\x1b[2m");
/// The thinking effort in the status line and the banner.
pub const EFFORT: Sgr = Sgr::fixed("\x1b[38;5;147m", "\x1b[38;5;60m", "\x1b[38;2;170;169;235m", "\x1b[38;2;94;90;154m");
/// Token counts and the cache hit rate in the status line.
pub const TOKENS: Sgr = Sgr::fixed("\x1b[38;5;110m", "\x1b[38;5;24m", "\x1b[38;2;129;188;229m", "\x1b[38;2;43;105;145m");
/// The git branch in the status line.
pub const BRANCH: Sgr = Sgr::fixed("\x1b[38;5;182m", "\x1b[38;5;96m", "\x1b[38;2;215;160;202m", "\x1b[38;2;134;76;122m");
/// A background a step off the terminal's own, for your messages.
pub const TINT: Sgr = Sgr::derived(Role::Tint, "\x1b[48;5;237m", "\x1b[48;5;254m", "\x1b[48;2;23;30;35m", "\x1b[48;2;223;229;226m");
/// The selected row of a list: a little more than a tint, with more lagoon in it.
pub const SELECT: Sgr = Sgr::derived(Role::Select, "\x1b[48;5;237m", "\x1b[48;5;254m", "\x1b[48;2;27;41;45m", "\x1b[48;2;210;220;217m");
/// Behind an added line of a diff. Only drawn in 24-bit color, where it can be this soft.
pub const ADD: Sgr = Sgr::derived(Role::Add, "\x1b[49m", "\x1b[49m", "\x1b[48;2;21;35;37m", "\x1b[48;2;224;239;231m");
/// Behind a removed line of a diff.
pub const DEL: Sgr = Sgr::derived(Role::Del, "\x1b[49m", "\x1b[49m", "\x1b[48;2;41;30;35m", "\x1b[48;2;248;229;226m");
/// Behind inline code, in 24-bit color.
pub const CHIP: Sgr = Sgr::derived(Role::Chip, "\x1b[49m", "\x1b[49m", "\x1b[48;2;30;35;40m", "\x1b[48;2;217;219;218m");

// Syntax colors for code blocks only; they carry no meaning in the rest of the UI.
const STRING: Sgr = Sgr::fixed("\x1b[32m", "\x1b[38;5;28m", "\x1b[32m", "\x1b[38;5;28m");
const NUMBER: Sgr = Sgr::fixed("\x1b[33m", "\x1b[38;5;130m", "\x1b[33m", "\x1b[38;5;130m");
const KEYWORD: Sgr = Sgr::fixed("\x1b[35m", "\x1b[38;5;90m", "\x1b[35m", "\x1b[38;5;90m");

/// Whether `NO_COLOR` asks for no colors.
pub fn no_color() -> bool {
    static NO_COLOR: OnceLock<bool> = OnceLock::new();
    *NO_COLOR.get_or_init(|| std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()))
}

/// `line` as it goes to the terminal: without its colors when `NO_COLOR` is set, keeping
/// bold, dim and the other text styles.
pub fn plain(line: &str) -> Cow<'_, str> {
    if no_color() { strip_colors(line) } else { Cow::Borrowed(line) }
}

fn strip_colors(line: &str) -> Cow<'_, str> {
    if !line.contains("\x1b[") {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("\x1b[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find(|c: char| c.is_ascii_alphabetic()) else {
            out.push_str(&rest[start..]);
            return Cow::Owned(out);
        };
        let (params, kind) = (&after[..end], after.as_bytes()[end] as char);
        rest = &after[end + 1..];
        if kind != 'm' {
            out.push_str(&format!("\x1b[{params}{kind}"));
            continue;
        }
        let kept = without_colors(params);
        if !kept.is_empty() {
            out.push_str(&format!("\x1b[{kept}m"));
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// The SGR parameters left once every foreground and background color is dropped.
fn without_colors(params: &str) -> String {
    if params.is_empty() {
        return "0".into();
    }
    let mut kept = Vec::new();
    let mut parts = params.split(';');
    while let Some(part) = parts.next() {
        match part {
            "38" | "48" => match parts.next() {
                Some("5") => {
                    parts.next();
                }
                Some("2") => {
                    parts.by_ref().take(3).for_each(drop);
                }
                _ => {}
            },
            _ if matches!(part.parse::<u32>(), Ok(30..=39 | 40..=49 | 90..=97 | 100..=107)) => {}
            _ => kept.push(part),
        }
    }
    kept.join(";")
}

mod markdown;
mod wrap;

pub use markdown::{Markdown, Part, render_table};
pub use wrap::{Sanitizer, ellipsize, hanging_indent, sanitize, sanitize_row, width, wrap, wrap_code, wrap_words};

#[cfg(test)]
use markdown::{code, inline};

#[cfg(test)]
mod tests;
