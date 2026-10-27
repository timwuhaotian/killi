//! The start-up banner: killi in block letters with a sky-to-violet gradient, and a few lines
//! about the session beside it.

use super::text::{BOLD, DIM, FAINT, RESET, is_light, truecolor, width};

const LOGO: [&str; 6] = [
    "██╗  ██╗██╗██╗     ██╗     ██╗",
    "██║ ██╔╝██║██║     ██║     ██║",
    "█████╔╝ ██║██║     ██║     ██║",
    "██╔═██╗ ██║██║     ██║     ██║",
    "██║  ██╗██║███████╗███████╗██║",
    "╚═╝  ╚═╝╚═╝╚══════╝╚══════╝╚═╝",
];
const LOGO_WIDTH: usize = 30;
const GAP: &str = "    ";
type Stops = [(f32, f32, f32); 3];
/// Lagoon, sky and periwinkle: the gradient starts where the brand's own color does, and its
/// other stops are the hues of the status line's figures.
const STOPS_DARK: Stops = [(79.0, 209.0, 197.0), (100.0, 188.0, 237.0), (157.0, 152.0, 242.0)];
/// The same hues, deeper, for a light terminal.
const STOPS_LIGHT: Stops = [(4.0, 121.0, 114.0), (1.0, 114.0, 159.0), (93.0, 83.0, 174.0)];

/// Rows a terminal needs before the block-letter logo is worth its six; with fewer the banner
/// is the wordmark and the setup lines.
pub const COMPACT_ROWS: usize = 30;
/// Columns below which the logo has no room beside the setup lines.
const COMPACT_COLUMNS: usize = 70;

/// Whether the banner is the wordmark alone: a narrow window, or one too short (`tall` false)
/// to give the logo its rows.
pub fn compact(columns: usize, tall: bool) -> bool {
    columns < COMPACT_COLUMNS || !tall
}

/// `killi` bold in the logo's gradient, with the version after it.
pub fn wordmark(version: &str) -> String {
    let truecolor = truecolor();
    let mut out = String::new();
    for (i, c) in WORD.chars().enumerate() {
        let (r, g, b) = gradient(i as f32 / (WORD.len() - 1) as f32);
        out.push_str(&format!("{BOLD}{}{c}", color(r, g, b, truecolor)));
    }
    format!("{out}{RESET} {DIM}{version}{RESET}")
}

/// Columns the spinner's second row indents by: the wave and the space after it, so the
/// elapsed time sits under the action word.
pub const INDENT: usize = WAVE + 1;

/// The wordmark that stands in for a spinner.
const WORD: &str = "killi";
/// Braille cells in the wave drawn before the word; each is two dots wide and four tall.
const WAVE: usize = 6;
/// Frames (a tenth of a second each) the wave takes to rise from a flat line.
const RISE: f32 = 8.0;

/// The top dot row of the wave's two-dot-thick ribbon in each dot column `frame` tenths of a
/// second in, 0 on top. A flat line at first, it rises into a wave that travels to the right a
/// dot column per frame, swelling and easing a little.
fn wave(frame: usize) -> [usize; WAVE * 2] {
    let t = frame as f32;
    let height = (t / RISE).min(1.0) * (0.86 + 0.14 * (t * 0.16).sin());
    std::array::from_fn(|x| {
        let phase = (x as f32 - t) / (WAVE * 2) as f32 * std::f32::consts::TAU;
        (1.0 - height * phase.sin()).round().clamp(0.0, 2.0) as usize
    })
}

/// The braille cell whose left column has the dots at `left` and `left + 1` lit, and whose
/// right column those at `right` and `right + 1`.
fn cell(left: usize, right: usize) -> char {
    const LEFT: [u32; 4] = [0x01, 0x02, 0x04, 0x40];
    const RIGHT: [u32; 4] = [0x08, 0x10, 0x20, 0x80];
    let bits = LEFT[left] | LEFT[left + 1] | RIGHT[right] | RIGHT[right + 1];
    char::from_u32(0x2800 + bits).expect("braille patterns are characters")
}

/// The spinner, `frame` tenths of a second after work began: a braille ribbon that rises from
/// a flat line and rolls on, then the `killi` wordmark fading in letter by letter, each with
/// the logo's gradient flowing through it. Always as wide as the wave, a space and the word.
pub fn working(frame: usize) -> String {
    action(frame, WORD)
}

/// The spinner with an action word (`thinking`, `writing`, …) in place of the wordmark: the
/// wave is unchanged, and the word fades in letter by letter with the logo's gradient
/// flowing through it, like the wordmark does. As wide as the wave, a space and the word.
pub fn action(frame: usize, word: &str) -> String {
    let truecolor = truecolor();
    let drift = frame as f32 * 0.035;
    // `across` is how far along its own piece a cell is, from 0 to 1; the word's gradient starts
    // half way round so the two never match.
    let paint = |across: f32, shift: f32, fade: f32| {
        let t = (across + shift + drift) % 2.0;
        let (r, g, b) = gradient(if t > 1.0 { 2.0 - t } else { t });
        color(r * fade, g * fade, b * fade, truecolor)
    };
    let dots = wave(frame);
    let rise = (frame as f32 / RISE).min(1.0);
    let mut out = String::new();
    for i in 0..WAVE {
        out.push_str(&paint(i as f32 / (WAVE - 1) as f32, 0.0, 0.5 + 0.5 * rise));
        out.push(cell(dots[2 * i], dots[2 * i + 1]));
    }
    out.push_str(RESET);
    out.push(' ');
    let chars: Vec<char> = word.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        let appeared = (frame as f32 - 2.0 * i as f32 + 1.0) / 3.0;
        if appeared <= 0.0 {
            out.push_str(&format!("{DIM}·{RESET}"));
            continue;
        }
        out.push_str(&paint(i as f32 / (chars.len() - 1).max(1) as f32, 0.5, appeared.clamp(0.0, 1.0) * 0.75 + 0.25));
        out.push(*c);
        out.push_str(RESET);
    }
    out
}

/// A progress bar `cells` wide with `fraction` of it filled: heavy strokes taking the logo's
/// gradient from the left, then a faint rule for what is left.
pub fn bar(fraction: f32, cells: usize) -> String {
    let truecolor = truecolor();
    let filled = (fraction.clamp(0.0, 1.0) * cells as f32).round() as usize;
    let mut out = String::new();
    for i in 0..filled {
        let (r, g, b) = gradient(i as f32 / (cells - 1).max(1) as f32);
        out.push_str(&color(r, g, b, truecolor));
        out.push('━');
    }
    if filled < cells {
        out.push_str(&format!("{FAINT}{}", "─".repeat(cells - filled)));
    }
    out.push_str(RESET);
    out
}

/// The banner's rows for a terminal `columns` wide: the logo with `side` beside it from its
/// second row, or `side` under it when there is no room, or only `side` when even the logo
/// doesn't fit.
pub fn render(columns: usize, side: &[String]) -> Vec<String> {
    if columns < LOGO_WIDTH {
        return side.to_vec();
    }
    let truecolor = truecolor();
    let logo: Vec<String> = LOGO.iter().enumerate().map(|(y, row)| paint(row, y, truecolor)).collect();
    let room = columns.saturating_sub(LOGO_WIDTH + GAP.len());
    if room < 24 || side.iter().any(|line| width(line) > room) && room < 40 {
        return logo.into_iter().chain(side.iter().cloned()).collect();
    }
    let mut rows = logo;
    for (i, line) in side.iter().enumerate() {
        match rows.get_mut(i + 1) {
            Some(row) => row.push_str(&format!("{GAP}{}", super::fit(line, room))),
            None => rows.push(format!("{}{GAP}{}", " ".repeat(LOGO_WIDTH), super::fit(line, room))),
        }
    }
    rows
}

/// One logo row in a diagonal gradient; the shadow strokes are a darker shade of the same color.
fn paint(row: &str, y: usize, truecolor: bool) -> String {
    let mut out = String::new();
    for (x, c) in row.chars().enumerate() {
        if c == ' ' {
            out.push(c);
            continue;
        }
        let (r, g, b) = gradient((x + 2 * y) as f32 / (LOGO_WIDTH + 2 * (LOGO.len() - 1)) as f32);
        let shade = if c == '█' { 1.0 } else { 0.5 };
        out.push_str(&color(r * shade, g * shade, b * shade, truecolor));
        out.push(c);
    }
    out.push_str(RESET);
    out
}

fn gradient(t: f32) -> (f32, f32, f32) {
    blend(if is_light() { &STOPS_LIGHT } else { &STOPS_DARK }, t)
}

/// The color `t` of the way, from 0 to 1, along `stops`.
fn blend(stops: &Stops, t: f32) -> (f32, f32, f32) {
    let t = t.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
    let i = (t.floor() as usize).min(stops.len() - 2);
    let f = t - i as f32;
    let (a, b) = (stops[i], stops[i + 1]);
    (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f, a.2 + (b.2 - a.2) * f)
}

/// A 24-bit color, or the nearest in the 256-color cube for terminals without it.
fn color(r: f32, g: f32, b: f32, truecolor: bool) -> String {
    if truecolor {
        return format!("\x1b[38;2;{};{};{}m", r as u8, g as u8, b as u8);
    }
    let level = |v: f32| (v / 255.0 * 5.0).round() as u8;
    format!("\x1b[38;5;{}m", 16 + 36 * level(r) + 6 * level(g) + level(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_logo_rows_are_all_one_width() {
        assert!(LOGO.iter().all(|row| width(row) == LOGO_WIDTH));
    }

    #[test]
    fn the_spinner_is_always_the_same_width() {
        for frame in 0..200 {
            assert_eq!(width(&working(frame)), WAVE + 1 + WORD.len(), "frame {frame}");
            assert_eq!(width(&action(frame, "thinking")), WAVE + 1 + 8, "frame {frame}");
        }
        assert!(working(0).contains('·'));
        assert!(!working(20).contains('·'));
        assert!(action(0, "writing").contains('·'));
        assert!(!action(20, "writing").contains('·'));
    }

    #[test]
    fn the_wave_rises_from_a_flat_line_and_rolls_on_as_one_unbroken_line() {
        assert!(wave(0).iter().all(|&row| row == wave(0)[0]));
        let mut rows = std::collections::HashSet::new();
        for frame in 8..120 {
            let dots = wave(frame);
            rows.extend(dots);
            assert_ne!(dots, wave(frame + 1), "frame {frame} stands still");
            assert!(dots.windows(2).all(|pair| pair[0].abs_diff(pair[1]) <= 1), "frame {frame} jumps: {dots:?}");
        }
        assert_eq!(rows.len(), 3, "the ribbon uses every height");
    }

    #[test]
    fn the_bar_fills_from_the_left_and_keeps_its_width() {
        for cells in [1, 8, 24] {
            for step in 0..=10 {
                assert_eq!(width(&bar(step as f32 / 10.0, cells)), cells);
            }
        }
        assert_eq!(bar(0.0, 4).matches('─').count(), 4);
        assert_eq!(bar(1.0, 4).matches('━').count(), 4);
        assert_eq!((bar(0.5, 4).matches('━').count(), bar(0.5, 4).matches('─').count()), (2, 2));
        assert_eq!(bar(7.0, 4), bar(1.0, 4), "past full is full");
    }

    #[test]
    fn the_logo_gives_way_to_the_wordmark_in_a_small_window() {
        assert!(!compact(118, true));
        assert!(compact(118, false), "too few rows for six of logo");
        assert!(compact(69, true), "too narrow for the setup lines beside it");
        assert!(!compact(70, true));
        let mark = wordmark("0.3.0");
        assert_eq!(width(&mark), WORD.len() + 1 + 5);
        assert!(mark.contains(BOLD) && mark.ends_with(RESET));
    }

    #[test]
    fn the_gradient_starts_at_lagoon_and_deepens_for_a_light_terminal() {
        assert_eq!(blend(&STOPS_DARK, 0.0), (79.0, 209.0, 197.0), "lagoon's own color");
        assert_eq!(blend(&STOPS_DARK, 1.0), STOPS_DARK[2]);
        assert_eq!(blend(&STOPS_DARK, 0.5), STOPS_DARK[1]);
        assert_eq!(blend(&STOPS_LIGHT, 7.0), STOPS_LIGHT[2], "past the end is the end");
        let brightness = |(r, g, b): (f32, f32, f32)| r + g + b;
        assert!(STOPS_LIGHT.iter().zip(STOPS_DARK).all(|(light, dark)| brightness(*light) < brightness(dark)));
    }

    #[test]
    fn side_lines_sit_beside_the_logo_or_under_it() {
        let side = vec!["killi 0.2.2".to_string(), "model  m".to_string()];
        let wide = render(100, &side);
        assert_eq!(wide.len(), 6);
        assert!(wide[1].ends_with("killi 0.2.2") && wide[2].ends_with("model  m"));
        assert!(wide.iter().all(|row| width(row) <= 100));
        let narrow = render(40, &side);
        assert_eq!(narrow.len(), 8);
        assert_eq!(narrow[6], "killi 0.2.2");
        assert_eq!(render(20, &side), side);
    }
}
