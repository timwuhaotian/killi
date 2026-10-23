//! The start-up banner: killi in block letters with a sky-to-violet gradient, and a few lines
//! about the session beside it.

use super::text::{DIM, RESET, truecolor, width};

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
/// Sky, indigo and violet.
const STOPS: [(f32, f32, f32); 3] = [(56.0, 189.0, 248.0), (129.0, 140.0, 248.0), (192.0, 132.0, 252.0)];

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
    for (i, c) in WORD.chars().enumerate() {
        let appeared = (frame as f32 - 2.0 * i as f32 + 1.0) / 3.0;
        if appeared <= 0.0 {
            out.push_str(&format!("{DIM}·{RESET}"));
            continue;
        }
        out.push_str(&paint(i as f32 / (WORD.len() - 1) as f32, 0.5, appeared.clamp(0.0, 1.0) * 0.75 + 0.25));
        out.push(c);
        out.push_str(RESET);
    }
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
    let t = t.clamp(0.0, 1.0) * (STOPS.len() - 1) as f32;
    let i = (t.floor() as usize).min(STOPS.len() - 2);
    let f = t - i as f32;
    let (a, b) = (STOPS[i], STOPS[i + 1]);
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
        }
        assert!(working(0).contains('·'));
        assert!(!working(20).contains('·'));
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
