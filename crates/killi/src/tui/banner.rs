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
/// Frames (a tenth of a second each) from the start of one glint to the start of the next,
/// and how many of them the glint spends crossing the word. The rest is a pause.
const GLINT_EVERY: usize = 40;
const GLINT_CROSSING: usize = 18;
/// How far the glint reaches ahead of its head and behind it, in letters: a sharp front and a
/// long tail, like a comet.
const GLINT_AHEAD: f32 = 0.9;
const GLINT_BEHIND: f32 = 2.4;
/// The cool white the glint brightens the letters toward.
const GLINT_WHITE: (f32, f32, f32) = (236.0, 246.0, 255.0);

/// How much of the glint is on `letter` `frame` tenths of a second in, from 0 to 1. The head
/// starts and ends out of reach of every letter, so the pause between glints is still.
fn glow(letter: usize, frame: usize) -> f32 {
    let cross = ((frame % GLINT_EVERY) as f32 / GLINT_CROSSING as f32).min(1.0);
    let (from, to) = (-GLINT_AHEAD, WORD.len() as f32 - 1.0 + GLINT_BEHIND);
    let ahead = letter as f32 - (from + (to - from) * cross);
    let reach = if ahead > 0.0 { GLINT_AHEAD } else { GLINT_BEHIND };
    ((ahead.abs() / reach).min(1.0) * std::f32::consts::PI).cos() * 0.5 + 0.5
}

/// The wordmark as a spinner, `frame` tenths of a second after work began. The letters fade in
/// one after another, then the logo's whole gradient flows through them and, every few
/// seconds, a soft glint crosses from left to right. Always `WORD.len()` columns wide.
pub fn working(frame: usize) -> String {
    let truecolor = truecolor();
    let letters = WORD.len();
    let drift = frame as f32 * 0.035;
    let mut out = String::new();
    for (i, c) in WORD.chars().enumerate() {
        let appeared = (frame as f32 - 2.0 * i as f32 + 1.0) / 3.0;
        if appeared <= 0.0 {
            out.push_str(&format!("{DIM}·{RESET}"));
            continue;
        }
        let wave = (i as f32 / (letters - 1) as f32 + drift) % 2.0;
        let (r, g, b) = gradient(if wave > 1.0 { 2.0 - wave } else { wave });
        let glow = glow(i, frame);
        let fade = appeared.clamp(0.0, 1.0) * 0.75 + 0.25;
        let mix = |v: f32, white: f32| (v + (white - v) * glow * 0.9) * fade;
        out.push_str(&color(mix(r, GLINT_WHITE.0), mix(g, GLINT_WHITE.1), mix(b, GLINT_WHITE.2), truecolor));
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
    fn the_spinner_is_always_as_wide_as_the_word() {
        for frame in 0..200 {
            assert_eq!(width(&working(frame)), WORD.len(), "frame {frame}");
        }
        assert!(working(0).contains('·'));
        assert!(!working(20).contains('·'));
    }

    #[test]
    fn the_glint_crosses_the_word_left_to_right_then_rests() {
        let peak = |letter: usize| (0..GLINT_EVERY).max_by(|&a, &b| glow(letter, a).total_cmp(&glow(letter, b))).unwrap();
        for letter in 0..WORD.len() {
            assert!(glow(letter, peak(letter)) > 0.9, "letter {letter}");
        }
        assert!((1..WORD.len()).all(|letter| peak(letter) > peak(letter - 1)));
        for frame in [0, GLINT_CROSSING, GLINT_EVERY - 1, GLINT_EVERY, GLINT_EVERY + GLINT_CROSSING + 3] {
            assert!((0..WORD.len()).all(|letter| glow(letter, frame) < 0.01), "frame {frame}");
        }
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
