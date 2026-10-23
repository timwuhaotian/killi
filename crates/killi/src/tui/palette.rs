//! Colors that follow the terminal's own ground. A terminal that answers the background (OSC 11)
//! and foreground (OSC 10) queries lets killi pick its grays, tints and stripes by their
//! contrast with that ground, instead of assuming `#0b0f14`: a fixed teal band is a foreign slab
//! on Kil's Ember (warm brown) or Dusk (violet), and SGR dim, which Kil and Alacritty apply as
//! two thirds of each channel, makes secondary text darker than body text on a light theme.
//!
//! Without an answer nothing here applies and the fixed shades in `text.rs` stay.

use std::fmt;
use std::sync::OnceLock;

/// A color as the terminal shows it.
pub type Rgb = (u8, u8, u8);

/// What the terminal said its ground is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ground {
    pub background: Rgb,
    /// Its foreground, when it answered that query too.
    pub foreground: Option<Rgb>,
}

/// The roles whose shade is derived from the ground.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Hairlines: the rules around the prompt.
    Rule,
    /// Marks the eye may skip: a tick, hints, the empty part of a meter.
    Faint,
    /// Secondary text: results, command output, notes.
    Muted,
    /// The band behind your own messages.
    Tint,
    /// The selected row of a list.
    Select,
    /// The stripe behind an added line.
    Add,
    /// The stripe behind a removed line.
    Del,
    /// The chip behind inline code.
    Chip,
}

const ROLES: usize = 8;

impl Role {
    /// Whether the color is painted behind text rather than on it.
    fn is_ground(self) -> bool {
        matches!(self, Role::Tint | Role::Select | Role::Add | Role::Del | Role::Chip)
    }
}

/// The derived shade of every role.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    colors: [Rgb; ROLES],
}

static PALETTE: OnceLock<Option<Palette>> = OnceLock::new();

/// Fixes the palette for the session; `None` keeps the fixed shades.
pub fn init(ground: Option<Ground>) {
    let _ = PALETTE.set(ground.map(derive));
}

/// The session's palette, if the terminal described its ground.
pub fn get() -> Option<&'static Palette> {
    PALETTE.get().and_then(Option::as_ref)
}

impl Palette {
    /// The escape for `role`: a foreground, or a background for the grounds. In a terminal
    /// without 24-bit color only text roles have a shade (the nearest of the 256), since a
    /// tint that subtle would land on the ground's own color; None for the rest.
    pub fn write(&self, f: &mut fmt::Formatter<'_>, role: Role, truecolor: bool) -> Option<fmt::Result> {
        let color = self.colors[role as usize];
        let (r, g, b) = color;
        Some(match (truecolor, role.is_ground()) {
            (true, false) => write!(f, "\x1b[38;2;{r};{g};{b}m"),
            (true, true) => write!(f, "\x1b[48;2;{r};{g};{b}m"),
            (false, false) => write!(f, "\x1b[38;5;{}m", nearest_256(color)),
            (false, true) => return None,
        })
    }

    #[cfg(test)]
    pub(super) fn rgb(&self, role: Role) -> Rgb {
        self.colors[role as usize]
    }
}

/// The palette for a ground. Text roles are steps from the background toward the foreground
/// that reach a contrast target, scaled down when the theme's own foreground has little
/// contrast, so secondary text is never brighter than body text. The grounds are small lifts of
/// the background with a cast of lagoon, the stripes with a cast of green and red.
pub fn derive(ground: Ground) -> Palette {
    let background = to_lab(ground.background);
    let light = background[0] > 0.6;
    let foreground = ground.foreground.unwrap_or(if light { (0, 0, 0) } else { (255, 255, 255) });
    let toward = to_lab(foreground);
    let full = contrast(foreground, ground.background);
    let target = |share: f64, floor: f64, ceiling: f64| (1.0 + share * (full - 1.0)).clamp(floor, ceiling);
    let step = |goal: f64| from_lab(reach(background, toward, ground.background, goal));
    let lift = |amount: f64| mix(background, toward, amount);
    let lagoon = to_lab(if light { (10, 110, 101) } else { (79, 209, 197) });
    let cast = |amount: f64, of: Lab| mix(background, of, amount);
    let mut colors = [(0, 0, 0); ROLES];
    colors[Role::Rule as usize] = step(target(0.04, 1.25, 1.42));
    colors[Role::Faint as usize] = step(target(0.2, 2.2, 3.0));
    colors[Role::Muted as usize] = step(target(0.5, 3.2, 5.8));
    colors[Role::Tint as usize] = from_lab(mix(lift(0.075), cast(0.12, lagoon), 0.35));
    colors[Role::Select as usize] = from_lab(mix(lift(0.10), cast(0.20, lagoon), 0.55));
    colors[Role::Add as usize] = from_lab(cast(0.14, lch(0.72, 0.11, 172.0)));
    colors[Role::Del as usize] = from_lab(cast(0.15, lch(0.72, 0.15, 18.0)));
    colors[Role::Chip as usize] = from_lab(lift(0.11));
    Palette { colors }
}

/// The step from `from` toward `toward` that first reaches `goal` contrast against `ground`,
/// or `toward` itself when it can't.
fn reach(from: Lab, toward: Lab, ground: Rgb, goal: f64) -> Lab {
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..24 {
        let middle = (low + high) / 2.0;
        if contrast(from_lab(mix(from, toward, middle)), ground) < goal {
            low = middle;
        } else {
            high = middle;
        }
    }
    mix(from, toward, high)
}

/// OKLab: `[lightness, a, b]`.
type Lab = [f64; 3];

fn linear(channel: u8) -> f64 {
    let c = f64::from(channel) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn gamma(linear: f64) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let encoded = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (encoded * 255.0).round() as u8
}

fn to_lab((r, g, b): Rgb) -> Lab {
    let (r, g, b) = (linear(r), linear(g), linear(b));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
    ]
}

fn from_lab([lightness, a, b]: Lab) -> Rgb {
    let l = (lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m = (lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s = (lightness - 0.089_484_177_5 * a - 1.291_485_548 * b).powi(3);
    (
        gamma(4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s),
        gamma(-1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s),
        gamma(-0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s),
    )
}

/// A color from lightness, chroma and hue in degrees.
fn lch(lightness: f64, chroma: f64, hue: f64) -> Lab {
    let radians = hue.to_radians();
    [lightness, chroma * radians.cos(), chroma * radians.sin()]
}

fn mix(a: Lab, b: Lab, amount: f64) -> Lab {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * amount)
}

fn luminance((r, g, b): Rgb) -> f64 {
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/// WCAG contrast ratio of two colors, from 1 to 21.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The index of the nearest color in the 256-color palette's cube or gray ramp.
fn nearest_256((r, g, b): Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |c: u8| (0..6).min_by_key(|&i| LEVELS[i].abs_diff(c)).unwrap_or(0);
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let distance = |p: Rgb| {
        let d = |x: u8, y: u8| i32::from(x.abs_diff(y)).pow(2);
        d(p.0, r) + d(p.1, g) + d(p.2, b)
    };
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let gray_of = |i: u8| (8 + 10 * i, 8 + 10 * i, 8 + 10 * i);
    let gray = (0..24u8).min_by_key(|&i| distance(gray_of(i))).unwrap_or(0);
    if distance(gray_of(gray)) < distance(cube) {
        232 + gray
    } else {
        (16 + 36 * ri + 6 * gi + bi) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NIGHT: Ground = Ground { background: (0x11, 0x13, 0x16), foreground: Some((0xd7, 0xda, 0xe0)) };
    const EMBER: Ground = Ground { background: (0x1d, 0x19, 0x15), foreground: Some((0xe8, 0xdc, 0xc6)) };
    const PAPER: Ground = Ground { background: (0xfb, 0xfa, 0xf7), foreground: Some((0x2e, 0x31, 0x38)) };
    /// Solarized dark: a foreground with little contrast.
    const SOLARIZED: Ground = Ground { background: (0x00, 0x2b, 0x36), foreground: Some((0x83, 0x94, 0x96)) };

    fn near(value: f64, goal: f64) {
        assert!((value - goal).abs() < 0.08, "{value} is not {goal}");
    }

    #[test]
    fn text_roles_reach_their_contrast_on_dark_and_light_grounds() {
        for ground in [NIGHT, EMBER, PAPER] {
            let palette = derive(ground);
            near(contrast(palette.rgb(Role::Rule), ground.background), 1.42);
            near(contrast(palette.rgb(Role::Faint), ground.background), 3.0);
            near(contrast(palette.rgb(Role::Muted), ground.background), 5.8);
        }
    }

    #[test]
    fn muted_text_is_never_brighter_than_the_foreground_it_is_muted_from() {
        for ground in [NIGHT, EMBER, PAPER, SOLARIZED] {
            let palette = derive(ground);
            let foreground = ground.foreground.unwrap();
            let [rule, faint, muted] = [Role::Rule, Role::Faint, Role::Muted].map(|role| contrast(palette.rgb(role), ground.background));
            let body = contrast(foreground, ground.background);
            assert!(1.0 < rule && rule < faint && faint < muted && muted < body, "{ground:?}: {rule} {faint} {muted} {body}");
        }
    }

    #[test]
    fn a_light_ground_gets_darker_grays_not_lighter_ones() {
        let palette = derive(PAPER);
        let lightness = |role| to_lab(palette.rgb(role))[0];
        assert!(lightness(Role::Rule) > lightness(Role::Faint) && lightness(Role::Faint) > lightness(Role::Muted));
        assert!(lightness(Role::Tint) < to_lab(PAPER.background)[0], "a tint on paper is a little darker");
    }

    #[test]
    fn the_grays_carry_the_grounds_own_cast() {
        let hue = |rgb: Rgb| {
            let [_, a, b] = to_lab(rgb);
            b.atan2(a).to_degrees().rem_euclid(360.0)
        };
        let ember = derive(EMBER);
        let (warm, tint) = (hue(ember.rgb(Role::Muted)), hue(ember.rgb(Role::Tint)));
        assert!((30.0..110.0).contains(&warm), "muted on Ember is warm, not {warm}");
        assert!((30.0..200.0).contains(&tint), "the band on Ember is not the fixed blue-teal: {tint}");
    }

    #[test]
    fn the_grounds_are_soft_lifts_of_the_background() {
        for ground in [NIGHT, EMBER, PAPER] {
            let palette = derive(ground);
            for role in [Role::Tint, Role::Select, Role::Add, Role::Del, Role::Chip] {
                let against = contrast(palette.rgb(role), ground.background);
                assert!((1.02..1.45).contains(&against), "{role:?} on {ground:?}: {against}");
            }
            let foreground = ground.foreground.unwrap();
            for role in [Role::Tint, Role::Select, Role::Add, Role::Del, Role::Chip] {
                assert!(contrast(foreground, palette.rgb(role)) > 7.0, "body text reads on {role:?}");
            }
            assert!(contrast(palette.rgb(Role::Muted), palette.rgb(Role::Tint)) > 4.5, "muted text reads on the band");
        }
    }

    #[test]
    fn without_a_foreground_the_ground_alone_gives_the_same_kind_of_ramp() {
        let night = Ground { foreground: None, ..NIGHT };
        let palette = derive(night);
        near(contrast(palette.rgb(Role::Muted), night.background), 5.8);
        let paper = derive(Ground { foreground: None, ..PAPER });
        assert!(to_lab(paper.rgb(Role::Muted))[0] < to_lab(PAPER.background)[0]);
    }

    #[test]
    fn colors_survive_the_trip_through_oklab() {
        for rgb in [(0, 0, 0), (255, 255, 255), (79, 209, 197), (255, 140, 90), (17, 19, 22)] {
            let back = from_lab(to_lab(rgb));
            assert!([(back.0, rgb.0), (back.1, rgb.1), (back.2, rgb.2)].iter().all(|(a, b)| a.abs_diff(*b) <= 1), "{rgb:?} came back as {back:?}");
        }
        near(contrast((255, 255, 255), (0, 0, 0)), 21.0);
    }

    #[test]
    fn the_256_color_shade_is_the_nearest_of_the_cube_and_the_gray_ramp() {
        assert_eq!(nearest_256((0, 0, 0)), 16);
        assert_eq!(nearest_256((255, 255, 255)), 231);
        assert_eq!(nearest_256((95, 175, 255)), 75);
        assert_eq!(nearest_256((108, 108, 108)), 242);
        assert_eq!(nearest_256((18, 18, 18)), 233);
    }

    #[test]
    fn grounds_have_no_shade_without_24_bit_color() {
        struct Show(Palette, Role, bool);
        impl fmt::Display for Show {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.write(f, self.1, self.2).unwrap_or(Ok(()))
            }
        }
        let palette = derive(NIGHT);
        assert!(Show(palette.clone(), Role::Muted, true).to_string().starts_with("\x1b[38;2;"));
        assert!(Show(palette.clone(), Role::Muted, false).to_string().starts_with("\x1b[38;5;"));
        assert!(Show(palette.clone(), Role::Tint, true).to_string().starts_with("\x1b[48;2;"));
        assert_eq!(Show(palette, Role::Tint, false).to_string(), "");
    }
}
