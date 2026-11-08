//! Styled terminal text: display width, wrapping, and a line-at-a-time Markdown styler that
//! works while text is still streaming in.

use std::borrow::Cow;
use std::fmt;
use std::sync::{OnceLock, RwLock};

use super::palette::{self, Ground, Role};

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const ITALIC: &str = "\x1b[3m";
pub const UNDERLINE: &str = "\x1b[4m";
pub const STRIKE: &str = "\x1b[9m";
/// Reverse video on and off, around selected text.
pub const REVERSE: &str = "\x1b[7m";
pub const NOT_REVERSE: &str = "\x1b[27m";

/// Which shades each color takes: the default theme on a dark or light terminal, or one of
/// the named skins `/theme` offers, each made for a dark terminal.
#[derive(Clone, Copy, PartialEq)]
enum Theme {
    Dark,
    Light,
    Batman,
    IcedCoffee,
    Creamery,
    Pastel,
    Cappuccino,
    Gray,
    OffWhite,
    RedOrange,
    SnowIce,
}

impl Theme {
    /// The skin a (normalized) theme name picks, when it picks one.
    fn skin_named(name: &str) -> Option<Theme> {
        match name {
            "batman" => Some(Theme::Batman),
            "iced-coffee" => Some(Theme::IcedCoffee),
            "creamery-station" => Some(Theme::Creamery),
            "pastel-tweencore" => Some(Theme::Pastel),
            "cappuccino" => Some(Theme::Cappuccino),
            "shades-of-gray" => Some(Theme::Gray),
            "off-white" => Some(Theme::OffWhite),
            "red-orange" => Some(Theme::RedOrange),
            "snow-and-ice" => Some(Theme::SnowIce),
            _ => None,
        }
    }

    /// This theme's row in an `Sgr`'s skin shades, when it is a skin.
    fn skin(self) -> Option<usize> {
        match self {
            Theme::Batman => Some(0),
            Theme::IcedCoffee => Some(1),
            Theme::Creamery => Some(2),
            Theme::Pastel => Some(3),
            Theme::Cappuccino => Some(4),
            Theme::Gray => Some(5),
            Theme::OffWhite => Some(6),
            Theme::RedOrange => Some(7),
            Theme::SnowIce => Some(8),
            Theme::Dark | Theme::Light => None,
        }
    }
}

/// The color themes `/theme` offers, each with what it looks like. All but the default are
/// made for a dark terminal; the default follows the terminal's own dark or light ground.
pub const THEMES: &[(&str, &str)] = &[
    ("default", "killi's colors, dark or light with the terminal"),
    ("batman", "signal yellow and olive on slate and black"),
    ("iced-coffee", "sky blue, coral and amber on dark"),
    ("creamery-station", "cream, butter and tan on dark"),
    ("pastel-tweencore", "pastel pink, lavender and baby blue on dark"),
    ("cappuccino", "cream, tan and rust over coffee browns"),
    ("shades-of-gray", "grays only, light to dark"),
    ("off-white", "warm whites and ivory on dark"),
    ("red-orange", "amber through orange to red on dark"),
    ("snow-and-ice", "white and ice cyan on dark"),
];

/// A theme name as the lookup takes it: lowercase, with spaces and underscores as dashes,
/// so "iced coffee" names `iced-coffee`.
fn normalized(name: &str) -> String {
    name.trim().to_lowercase().replace([' ', '_'], "-")
}

/// Whether `setting` leaves the terminal's ground to decide the shades: anything but a
/// pinned `"dark"`/`"light"` or a skin's name.
pub fn reads_ground(setting: Option<&str>) -> bool {
    let Some(name) = setting.map(normalized) else {
        return true;
    };
    !matches!(name.as_str(), "dark" | "light") && Theme::skin_named(&name).is_none()
}

static THEME: RwLock<Option<Theme>> = RwLock::new(None);
/// The default theme this session resolved at its start, which `/theme default` goes back to.
static BASE: OnceLock<Theme> = OnceLock::new();

fn current_theme() -> Theme {
    if let Some(theme) = *THEME.read().expect("theme lock") {
        return theme;
    }
    let theme = *BASE.get_or_init(detect_theme);
    *THEME.write().expect("theme lock") = Some(theme);
    theme
}

/// Fixes the theme for the session: `"dark"` or `"light"` pins the default's shades, a
/// skin's name takes that skin, anything else goes by the terminal's `ground` when it answered
/// the color queries, else by `COLORFGBG`, which most terminals that have a light background
/// set. A ground also gives the grays, tints and stripes their shades (see `palette`); a
/// pinned theme has none, and keeps the fixed shades.
pub fn init_theme(setting: Option<&str>, ground: Option<Ground>) {
    let ground = ground.filter(|_| reads_ground(setting));
    let name = setting.map(normalized);
    let skin = name.as_deref().and_then(Theme::skin_named);
    let base = match name.as_deref() {
        Some("dark") => Theme::Dark,
        Some("light") => Theme::Light,
        _ if skin.is_some() => Theme::Dark,
        _ => ground.map_or_else(detect_theme, |ground| theme_for(ground.background)),
    };
    let _ = BASE.set(base);
    *THEME.write().expect("theme lock") = Some(skin.unwrap_or(base));
    palette::init(ground);
}

/// The name of the theme in use, as `/theme` writes it.
pub fn skin_name() -> &'static str {
    match current_theme() {
        Theme::Batman => "batman",
        Theme::IcedCoffee => "iced-coffee",
        Theme::Creamery => "creamery-station",
        Theme::Pastel => "pastel-tweencore",
        Theme::Cappuccino => "cappuccino",
        Theme::Gray => "shades-of-gray",
        Theme::OffWhite => "off-white",
        Theme::RedOrange => "red-orange",
        Theme::SnowIce => "snow-and-ice",
        Theme::Dark | Theme::Light => "default",
    }
}

/// Switches to the theme `name` picks from `THEMES`, for everything printed from here on.
/// False when no theme goes by that name.
pub fn set_theme(name: &str) -> bool {
    let name = normalized(name);
    let theme = match name.as_str() {
        "default" => *BASE.get_or_init(detect_theme),
        _ => match Theme::skin_named(&name) {
            Some(skin) => skin,
            None => return false,
        },
    };
    *THEME.write().expect("theme lock") = Some(theme);
    true
}

/// The theme for a background color: light when it reflects more than half the light.
fn theme_for((r, g, b): (u8, u8, u8)) -> Theme {
    let luminance = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b);
    if luminance > 127.5 {
        Theme::Light
    } else {
        Theme::Dark
    }
}

fn detect_theme() -> Theme {
    let background = std::env::var("COLORFGBG")
        .ok()
        .and_then(|v| v.rsplit(';').next().and_then(|bg| bg.parse::<u8>().ok()));
    match background {
        Some(7 | 9..=15) => Theme::Light,
        _ => Theme::Dark,
    }
}

/// Whether the session runs on a light terminal.
pub fn is_light() -> bool {
    current_theme() == Theme::Light
}

/// Whether the terminal takes 24-bit colors, as `COLORTERM` announces.
pub fn truecolor() -> bool {
    static TRUECOLOR: OnceLock<bool> = OnceLock::new();
    *TRUECOLOR
        .get_or_init(|| std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit"))
}

/// The shades one skin gives a token: its 256-color escape and its truecolor one.
type SkinShade = (&'static str, &'static str);

/// A color escape that comes out in the shade for the current theme when it is printed: the
/// design token itself on a truecolor terminal, else its nearest 256-color shade. A derived
/// role takes the shade `palette` made from the terminal's ground when there is one, under
/// the default theme; a skin always paints its own fixed shades.
#[derive(Clone, Copy)]
pub struct Sgr {
    dark: &'static str,
    light: &'static str,
    dark_true: &'static str,
    light_true: &'static str,
    /// Every named skin's shade, in `Theme::skin`'s order.
    skins: [SkinShade; 9],
    derived: Option<Role>,
}

impl Sgr {
    const fn fixed(
        dark: &'static str,
        light: &'static str,
        dark_true: &'static str,
        light_true: &'static str,
        skins: [SkinShade; 9],
    ) -> Sgr {
        Sgr {
            dark,
            light,
            dark_true,
            light_true,
            skins,
            derived: None,
        }
    }

    /// A role that follows the ground; the fixed shades are for when the terminal gave none.
    const fn derived(
        role: Role,
        dark: &'static str,
        light: &'static str,
        dark_true: &'static str,
        light_true: &'static str,
        skins: [SkinShade; 9],
    ) -> Sgr {
        Sgr {
            dark,
            light,
            dark_true,
            light_true,
            skins,
            derived: Some(role),
        }
    }

    fn shade(&self, theme: Theme, truecolor: bool) -> &'static str {
        if let Some(skin) = theme.skin() {
            let (plain, exact) = self.skins[skin];
            return if truecolor { exact } else { plain };
        }
        match (theme, truecolor) {
            (Theme::Light, true) => self.light_true,
            (Theme::Light, false) => self.light,
            (_, true) => self.dark_true,
            (_, false) => self.dark,
        }
    }
}

impl fmt::Display for Sgr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let theme = current_theme();
        match self.derived {
            // Colors are about to be stripped; what recedes keeps receding, as dim.
            Some(Role::Faint | Role::Rule | Role::Muted) if no_color() => {
                return f.write_str("\x1b[2m");
            }
            Some(role) if !no_color() && theme.skin().is_none() => {
                if let Some(palette) = palette::get()
                    && let Some(written) = palette.write(f, role, truecolor())
                {
                    return written;
                }
            }
            _ => {}
        }
        f.write_str(self.shade(theme, truecolor()))
    }
}

// The roles of the design system (`docs/killi-design-system.html`): each token's exact color
// where the terminal shows 24-bit color, else its nearest 256-color shade. The skins' shades
// are their palettes' colors — batman #fdff00/#988829/#242424/#282e3c/#505c7c, iced coffee
// #cfe2f3/#6fa8dc/#ef6868/#f4cccc/#ce7e00, creamery station
// #f8f2e7/#fff9cf/#fce9b2/#dec092/#cab297, pastel tweencore
// #ff9cd2/#fffbee/#ffcffc/#cbbeff/#d9ecff, cappuccino
// #4b3832/#854442/#fff4e6/#3c2f2f/#be9b7b, shades of gray
// #999999/#777777/#555555/#333333/#111111, off white
// #fffff2/#f9f9f9/#fffff4/#fbf7f5/#f9f1f1, red-orange
// #ffc100/#ff9a00/#ff7400/#ff4d00/#ff0000 and snow and ice
// #ffffff/#b9e8ea/#86d6d8/#3fd0d4/#20c3d0 — lightened or darkened a role at a time so each
// keeps the contrast its default shade has on a dark ground. Skins that are only shades of
// one hue (the grays, the whites) take the lighter and darker shades their scale points to.

/// Killi: running work, a finished call's tick, the active mode, commands, links and the
/// selected row's mark.
pub const LAGOON: Sgr = Sgr::fixed(
    "\x1b[38;5;80m",
    "\x1b[38;5;23m",
    "\x1b[38;2;79;209;197m",
    "\x1b[38;2;10;110;101m",
    [
        ("\x1b[38;5;226m", "\x1b[38;2;253;255;0m"),
        ("\x1b[38;5;74m", "\x1b[38;2;111;168;220m"),
        ("\x1b[38;5;223m", "\x1b[38;2;252;233;178m"),
        ("\x1b[38;5;218m", "\x1b[38;2;255;156;210m"),
        ("\x1b[38;5;186m", "\x1b[38;2;230;196;155m"),
        ("\x1b[38;5;255m", "\x1b[38;2;238;238;238m"),
        ("\x1b[38;5;231m", "\x1b[38;2;255;255;242m"),
        ("\x1b[38;5;208m", "\x1b[38;2;255;154;0m"),
        ("\x1b[38;5;80m", "\x1b[38;2;63;208;212m"),
    ],
);
/// The human: your messages, the prompt, questions and anything else waiting on you.
pub const FIN: Sgr = Sgr::fixed(
    "\x1b[38;5;209m",
    "\x1b[38;5;130m",
    "\x1b[38;2;255;140;90m",
    "\x1b[38;2;173;63;20m",
    [
        ("\x1b[38;5;146m", "\x1b[38;2;159;176;212m"),
        ("\x1b[38;5;224m", "\x1b[38;2;244;204;204m"),
        ("\x1b[38;5;180m", "\x1b[38;2;222;192;146m"),
        ("\x1b[38;5;195m", "\x1b[38;2;217;236;255m"),
        ("\x1b[38;5;173m", "\x1b[38;2;201;120;110m"),
        ("\x1b[38;5;252m", "\x1b[38;2;204;204;204m"),
        ("\x1b[38;5;255m", "\x1b[38;2;249;241;241m"),
        ("\x1b[38;5;208m", "\x1b[38;2;255;116;0m"),
        ("\x1b[38;5;116m", "\x1b[38;2;134;214;216m"),
    ],
);
/// Retries, a context window nearly full, an interrupted turn.
pub const WARN: Sgr = Sgr::fixed(
    "\x1b[38;5;221m",
    "\x1b[38;5;94m",
    "\x1b[38;2;242;193;78m",
    "\x1b[38;2;138;90;0m",
    [
        ("\x1b[38;5;185m", "\x1b[38;2;230;200;50m"),
        ("\x1b[38;5;178m", "\x1b[38;2;224;156;38m"),
        ("\x1b[38;5;179m", "\x1b[38;2;217;168;79m"),
        ("\x1b[38;5;225m", "\x1b[38;2;255;207;252m"),
        ("\x1b[38;5;215m", "\x1b[38;2;238;190;110m"),
        ("\x1b[38;5;253m", "\x1b[38;2;221;221;221m"),
        ("\x1b[38;5;230m", "\x1b[38;2;255;244;214m"),
        ("\x1b[38;5;214m", "\x1b[38;2;255;193;0m"),
        ("\x1b[38;5;231m", "\x1b[38;2;255;255;255m"),
    ],
);
/// Failed tool calls, errors and removed lines. Skins with no red of their own keep the
/// default's, so a failure reads as one in every theme.
pub const DANGER: Sgr = Sgr::fixed(
    "\x1b[38;5;204m",
    "\x1b[38;5;124m",
    "\x1b[38;2;255;111;125m",
    "\x1b[38;2;179;38;30m",
    [
        ("\x1b[38;5;204m", "\x1b[38;2;255;111;125m"),
        ("\x1b[38;5;203m", "\x1b[38;2;239;104;104m"),
        ("\x1b[38;5;204m", "\x1b[38;2;255;111;125m"),
        ("\x1b[38;5;204m", "\x1b[38;2;255;111;125m"),
        ("\x1b[38;5;167m", "\x1b[38;2;230;95;80m"),
        ("\x1b[38;5;204m", "\x1b[38;2;255;111;125m"),
        ("\x1b[38;5;204m", "\x1b[38;2;255;111;125m"),
        ("\x1b[38;5;203m", "\x1b[38;2;255;77;60m"),
        ("\x1b[38;5;204m", "\x1b[38;2;255;111;125m"),
    ],
);
/// Marks the eye may skip: gutters, hints, the empty part of a meter.
pub const FAINT: Sgr = Sgr::derived(
    Role::Faint,
    "\x1b[38;5;242m",
    "\x1b[38;5;245m",
    "\x1b[38;2;92;107;122m",
    "\x1b[38;2;117;131;138m",
    [
        ("\x1b[38;5;60m", "\x1b[38;2;80;92;124m"),
        ("\x1b[38;5;60m", "\x1b[38;2;91;113;134m"),
        ("\x1b[38;5;101m", "\x1b[38;2;138;122;95m"),
        ("\x1b[38;5;96m", "\x1b[38;2;122;114;153m"),
        ("\x1b[38;5;101m", "\x1b[38;2;140;118;104m"),
        ("\x1b[38;5;243m", "\x1b[38;2;119;119;119m"),
        ("\x1b[38;5;246m", "\x1b[38;2;150;145;138m"),
        ("\x1b[38;5;95m", "\x1b[38;2;153;92;51m"),
        ("\x1b[38;5;66m", "\x1b[38;2;84;134;136m"),
    ],
);
/// Hairlines: the rules around the prompt. As faint as a mark until the ground is known.
pub const RULE: Sgr = Sgr::derived(
    Role::Rule,
    "\x1b[38;5;242m",
    "\x1b[38;5;245m",
    "\x1b[38;2;92;107;122m",
    "\x1b[38;2;117;131;138m",
    [
        ("\x1b[38;5;59m", "\x1b[38;2;58;67;92m"),
        ("\x1b[38;5;59m", "\x1b[38;2;61;76;92m"),
        ("\x1b[38;5;59m", "\x1b[38;2;95;83;64m"),
        ("\x1b[38;5;240m", "\x1b[38;2;81;76;102m"),
        ("\x1b[38;5;239m", "\x1b[38;2;92;74;64m"),
        ("\x1b[38;5;240m", "\x1b[38;2;85;85;85m"),
        ("\x1b[38;5;241m", "\x1b[38;2;105;100;95m"),
        ("\x1b[38;5;238m", "\x1b[38;2;102;58;34m"),
        ("\x1b[38;5;239m", "\x1b[38;2;50;88;92m"),
    ],
);
/// Secondary text. SGR dim is the fallback, not the design: Kil and Alacritty scale each
/// channel to two thirds, which makes dim text darker than body text on a light theme.
pub const DIM: Sgr = Sgr::derived(
    Role::Muted,
    "\x1b[2m",
    "\x1b[2m",
    "\x1b[2m",
    "\x1b[2m",
    [
        ("\x1b[38;5;103m", "\x1b[38;2;139;150;179m"),
        ("\x1b[38;5;146m", "\x1b[38;2;157;180;201m"),
        ("\x1b[38;5;144m", "\x1b[38;2;179;160;134m"),
        ("\x1b[38;5;146m", "\x1b[38;2;173;162;217m"),
        ("\x1b[38;5;138m", "\x1b[38;2;190;155;123m"),
        ("\x1b[38;5;246m", "\x1b[38;2;153;153;153m"),
        ("\x1b[38;5;251m", "\x1b[38;2;200;194;186m"),
        ("\x1b[38;5;173m", "\x1b[38;2;230;140;64m"),
        ("\x1b[38;5;73m", "\x1b[38;2;110;175;180m"),
    ],
);
/// The thinking effort in the status line and the banner.
pub const EFFORT: Sgr = Sgr::fixed(
    "\x1b[38;5;147m",
    "\x1b[38;5;60m",
    "\x1b[38;2;170;169;235m",
    "\x1b[38;2;94;90;154m",
    [
        ("\x1b[38;5;100m", "\x1b[38;2;152;136;41m"),
        ("\x1b[38;5;189m", "\x1b[38;2;207;226;243m"),
        ("\x1b[38;5;230m", "\x1b[38;2;255;249;207m"),
        ("\x1b[38;5;183m", "\x1b[38;2;203;190;255m"),
        ("\x1b[38;5;230m", "\x1b[38;2;255;244;230m"),
        ("\x1b[38;5;254m", "\x1b[38;2;230;230;230m"),
        ("\x1b[38;5;231m", "\x1b[38;2;249;249;249m"),
        ("\x1b[38;5;221m", "\x1b[38;2;255;210;80m"),
        ("\x1b[38;5;152m", "\x1b[38;2;185;232;234m"),
    ],
);
/// Token counts and the cache hit rate in the status line.
pub const TOKENS: Sgr = Sgr::fixed(
    "\x1b[38;5;110m",
    "\x1b[38;5;24m",
    "\x1b[38;2;129;188;229m",
    "\x1b[38;2;43;105;145m",
    [
        ("\x1b[38;5;103m", "\x1b[38;2;126;147;189m"),
        ("\x1b[38;5;110m", "\x1b[38;2;143;192;232m"),
        ("\x1b[38;5;180m", "\x1b[38;2;214;189;143m"),
        ("\x1b[38;5;153m", "\x1b[38;2;185;216;250m"),
        ("\x1b[38;5;180m", "\x1b[38;2;205;170;140m"),
        ("\x1b[38;5;248m", "\x1b[38;2;170;170;170m"),
        ("\x1b[38;5;253m", "\x1b[38;2;220;220;214m"),
        ("\x1b[38;5;215m", "\x1b[38;2;255;174;77m"),
        ("\x1b[38;5;80m", "\x1b[38;2;100;205;215m"),
    ],
);
/// The git branch in the status line.
pub const BRANCH: Sgr = Sgr::fixed(
    "\x1b[38;5;182m",
    "\x1b[38;5;96m",
    "\x1b[38;2;215;160;202m",
    "\x1b[38;2;134;76;122m",
    [
        ("\x1b[38;5;188m", "\x1b[38;2;199;207;223m"),
        ("\x1b[38;5;217m", "\x1b[38;2;240;154;154m"),
        ("\x1b[38;5;255m", "\x1b[38;2;248;242;231m"),
        ("\x1b[38;5;231m", "\x1b[38;2;255;251;238m"),
        ("\x1b[38;5;180m", "\x1b[38;2;232;160;140m"),
        ("\x1b[38;5;255m", "\x1b[38;2;245;245;245m"),
        ("\x1b[38;5;255m", "\x1b[38;2;243;230;230m"),
        ("\x1b[38;5;203m", "\x1b[38;2;255;90;60m"),
        ("\x1b[38;5;195m", "\x1b[38;2;210;244;245m"),
    ],
);
/// A background a step off the terminal's own, for your messages.
pub const TINT: Sgr = Sgr::derived(
    Role::Tint,
    "\x1b[48;5;237m",
    "\x1b[48;5;254m",
    "\x1b[48;2;23;30;35m",
    "\x1b[48;2;223;229;226m",
    [
        ("\x1b[48;5;236m", "\x1b[48;2;40;46;60m"),
        ("\x1b[48;5;236m", "\x1b[48;2;34;48;62m"),
        ("\x1b[48;5;236m", "\x1b[48;2;58;50;34m"),
        ("\x1b[48;5;236m", "\x1b[48;2;64;39;53m"),
        ("\x1b[48;5;236m", "\x1b[48;2;60;47;47m"),
        ("\x1b[48;5;235m", "\x1b[48;2;38;38;38m"),
        ("\x1b[48;5;236m", "\x1b[48;2;52;50;46m"),
        ("\x1b[48;5;235m", "\x1b[48;2;64;32;16m"),
        ("\x1b[48;5;235m", "\x1b[48;2;18;52;56m"),
    ],
);
/// The selected row of a list: a little more than a tint, with more lagoon in it.
pub const SELECT: Sgr = Sgr::derived(
    Role::Select,
    "\x1b[48;5;237m",
    "\x1b[48;5;254m",
    "\x1b[48;2;27;41;45m",
    "\x1b[48;2;210;220;217m",
    [
        ("\x1b[48;5;60m", "\x1b[48;2;57;68;95m"),
        ("\x1b[48;5;238m", "\x1b[48;2;47;74;99m"),
        ("\x1b[48;5;238m", "\x1b[48;2;85;72;44m"),
        ("\x1b[48;5;239m", "\x1b[48;2;89;61;84m"),
        ("\x1b[48;5;237m", "\x1b[48;2;75;56;50m"),
        ("\x1b[48;5;238m", "\x1b[48;2;64;64;64m"),
        ("\x1b[48;5;238m", "\x1b[48;2;74;70;64m"),
        ("\x1b[48;5;58m", "\x1b[48;2;92;48;20m"),
        ("\x1b[48;5;23m", "\x1b[48;2;24;78;84m"),
    ],
);
/// Behind an added line of a diff. Only drawn in 24-bit color, where it can be this soft.
pub const ADD: Sgr = Sgr::derived(
    Role::Add,
    "\x1b[49m",
    "\x1b[49m",
    "\x1b[48;2;30;57;54m",
    "\x1b[48;2;203;230;219m",
    [
        ("\x1b[49m", "\x1b[48;2;63;61;23m"),
        ("\x1b[49m", "\x1b[48;2;46;74;92m"),
        ("\x1b[49m", "\x1b[48;2;77;69;32m"),
        ("\x1b[49m", "\x1b[48;2;52;62;84m"),
        ("\x1b[49m", "\x1b[48;2;70;58;40m"),
        ("\x1b[49m", "\x1b[48;2;52;52;52m"),
        ("\x1b[49m", "\x1b[48;2;60;60;50m"),
        ("\x1b[49m", "\x1b[48;2;76;54;14m"),
        ("\x1b[49m", "\x1b[48;2;20;84;80m"),
    ],
);
/// Behind a removed line of a diff.
pub const DEL: Sgr = Sgr::derived(
    Role::Del,
    "\x1b[49m",
    "\x1b[49m",
    "\x1b[48;2;72;45;50m",
    "\x1b[48;2;249;211;208m",
    [
        ("\x1b[49m", "\x1b[48;2;72;45;50m"),
        ("\x1b[49m", "\x1b[48;2;95;47;44m"),
        ("\x1b[49m", "\x1b[48;2;72;45;50m"),
        ("\x1b[49m", "\x1b[48;2;77;47;63m"),
        ("\x1b[49m", "\x1b[48;2;88;48;44m"),
        ("\x1b[49m", "\x1b[48;2;72;45;50m"),
        ("\x1b[49m", "\x1b[48;2;72;45;50m"),
        ("\x1b[49m", "\x1b[48;2;92;32;20m"),
        ("\x1b[49m", "\x1b[48;2;72;45;50m"),
    ],
);
/// Behind inline code, in 24-bit color.
pub const CHIP: Sgr = Sgr::derived(
    Role::Chip,
    "\x1b[49m",
    "\x1b[49m",
    "\x1b[48;2;30;35;40m",
    "\x1b[48;2;217;219;218m",
    [
        ("\x1b[49m", "\x1b[48;2;46;52;68m"),
        ("\x1b[49m", "\x1b[48;2;43;56;68m"),
        ("\x1b[49m", "\x1b[48;2;69;59;38m"),
        ("\x1b[49m", "\x1b[48;2;56;52;77m"),
        ("\x1b[49m", "\x1b[48;2;68;54;48m"),
        ("\x1b[49m", "\x1b[48;2;44;44;44m"),
        ("\x1b[49m", "\x1b[48;2;58;55;50m"),
        ("\x1b[49m", "\x1b[48;2;70;40;22m"),
        ("\x1b[49m", "\x1b[48;2;22;62;66m"),
    ],
);

// Syntax colors for code blocks only; they carry no meaning in the rest of the UI.
const STRING: Sgr = Sgr::fixed(
    "\x1b[32m",
    "\x1b[38;5;28m",
    "\x1b[32m",
    "\x1b[38;5;28m",
    [
        ("\x1b[38;5;143m", "\x1b[38;2;169;184;107m"),
        ("\x1b[38;5;217m", "\x1b[38;2;238;179;179m"),
        ("\x1b[38;5;230m", "\x1b[38;2;255;249;207m"),
        ("\x1b[38;5;231m", "\x1b[38;2;255;251;238m"),
        ("\x1b[38;5;230m", "\x1b[38;2;255;244;230m"),
        ("\x1b[38;5;252m", "\x1b[38;2;204;204;204m"),
        ("\x1b[38;5;231m", "\x1b[38;2;251;247;245m"),
        ("\x1b[38;5;214m", "\x1b[38;2;255;193;0m"),
        ("\x1b[38;5;152m", "\x1b[38;2;185;232;234m"),
    ],
);
const NUMBER: Sgr = Sgr::fixed(
    "\x1b[33m",
    "\x1b[38;5;130m",
    "\x1b[33m",
    "\x1b[38;5;130m",
    [
        ("\x1b[38;5;185m", "\x1b[38;2;232;227;60m"),
        ("\x1b[38;5;178m", "\x1b[38;2;224;156;38m"),
        ("\x1b[38;5;180m", "\x1b[38;2;222;192;146m"),
        ("\x1b[38;5;225m", "\x1b[38;2;255;207;252m"),
        ("\x1b[38;5;215m", "\x1b[38;2;238;190;110m"),
        ("\x1b[38;5;255m", "\x1b[38;2;238;238;238m"),
        ("\x1b[38;5;230m", "\x1b[38;2;255;240;200m"),
        ("\x1b[38;5;222m", "\x1b[38;2;255;224;130m"),
        ("\x1b[38;5;231m", "\x1b[38;2;255;255;255m"),
    ],
);
const KEYWORD: Sgr = Sgr::fixed(
    "\x1b[35m",
    "\x1b[38;5;90m",
    "\x1b[35m",
    "\x1b[38;5;90m",
    [
        ("\x1b[38;5;103m", "\x1b[38;2;126;147;189m"),
        ("\x1b[38;5;74m", "\x1b[38;2;111;168;220m"),
        ("\x1b[38;5;223m", "\x1b[38;2;252;233;178m"),
        ("\x1b[38;5;218m", "\x1b[38;2;255;156;210m"),
        ("\x1b[38;5;174m", "\x1b[38;2;222;150;120m"),
        ("\x1b[38;5;231m", "\x1b[38;2;250;250;250m"),
        ("\x1b[38;5;231m", "\x1b[38;2;255;255;244m"),
        ("\x1b[38;5;208m", "\x1b[38;2;255;116;0m"),
        ("\x1b[38;5;38m", "\x1b[38;2;32;195;208m"),
    ],
);

/// Closes an OSC 8 hyperlink.
pub const LINK_CLOSE: &str = "\x1b]8;;\x1b\\";

/// Opens an OSC 8 hyperlink to `url`: the label that follows is clickable in terminals
/// that take hyperlinks (Terminal, iTerm2, Ghostty, WezTerm, VS Code), plain underlined
/// text where they don't.
pub fn link_open(url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\")
}

/// Whether `url` may become a hyperlink: a web address without controls or spaces, so the
/// terminal never acts on anything but a plain link killi itself printed.
fn safe_link(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://"))
        && !url.bytes().any(|b| b <= 0x20 || b == 0x7f || b == 0x1b || b == 0x07)
}

/// `styled` as a clickable link to `url`; unchanged when `url` isn't a safe web address.
pub fn hyperlink(url: &str, styled: &str) -> String {
    if safe_link(url) {
        format!("{}{styled}{LINK_CLOSE}", link_open(url))
    } else {
        styled.to_string()
    }
}

/// Whether `seq` opens an OSC 8 hyperlink.
pub(crate) fn is_link_open(seq: &str) -> bool {
    seq.starts_with("\x1b]8;;") && seq != LINK_CLOSE
}

/// Whether `seq` closes one.
pub(crate) fn is_link_close(seq: &str) -> bool {
    seq == LINK_CLOSE
}

/// The bytes the escape sequence at the start of `text` takes: CSI through its final byte,
/// an OSC string through BEL or ST, anything else the escape and the character after it.
/// A sequence the text ends in the middle of runs to the text's end, so it still takes no
/// columns. Assumes `text` starts with ESC.
pub(crate) fn escape_len(text: &str) -> usize {
    debug_assert!(text.starts_with('\x1b'));
    let after = &text[1..];
    let Some(second) = after.chars().next() else {
        return 1;
    };
    match second {
        '[' => {
            let mut len = 1 + second.len_utf8();
            let mut rest = &after[second.len_utf8()..];
            loop {
                let Some(c) = rest.chars().next() else {
                    return text.len();
                };
                if ('\x30'..='\x3f').contains(&c) || ('\x20'..='\x2f').contains(&c) {
                    len += c.len_utf8();
                    rest = &rest[c.len_utf8()..];
                } else if ('\x40'..='\x7e').contains(&c) {
                    return len + c.len_utf8();
                } else {
                    return len;
                }
            }
        }
        ']' | 'P' | 'X' | '^' | '_' => {
            let mut len = 1 + second.len_utf8();
            let mut rest = &after[second.len_utf8()..];
            loop {
                let Some(c) = rest.chars().next() else {
                    return text.len();
                };
                if c == '\x07' {
                    return len + 1;
                }
                if c == '\x1b' && rest[1..].starts_with('\\') {
                    return len + 2;
                }
                if c.is_control() {
                    return len;
                }
                len += c.len_utf8();
                rest = &rest[c.len_utf8()..];
            }
        }
        _ => 1 + second.len_utf8(),
    }
}

/// Whether `NO_COLOR` asks for no colors.
pub fn no_color() -> bool {
    static NO_COLOR: OnceLock<bool> = OnceLock::new();
    *NO_COLOR.get_or_init(|| std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()))
}

/// `line` as it goes to the terminal: without its colors when `NO_COLOR` is set, keeping
/// bold, dim and the other text styles.
pub fn plain(line: &str) -> Cow<'_, str> {
    if no_color() {
        strip_colors(line)
    } else {
        Cow::Borrowed(line)
    }
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
            _ if matches!(
                part.parse::<u32>(),
                Ok(30..=39 | 40..=49 | 90..=97 | 100..=107)
            ) => {}
            _ => kept.push(part),
        }
    }
    kept.join(";")
}

mod markdown;
mod wrap;

pub use markdown::{Markdown, Part, render_table};
pub use wrap::{
    Sanitizer, ellipsize, hanging_indent, sanitize, sanitize_row, width, wrap, wrap_code,
    wrap_words,
};

#[cfg(test)]
use markdown::{code, inline};

#[cfg(test)]
mod tests;
