//! The start-up banner: killi in block letters with a sky-to-violet gradient, and a few lines
//! about the session beside it.

use super::text::{RESET, width};

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

/// The banner's rows for a terminal `columns` wide: the logo with `side` beside it from its
/// second row, or `side` under it when there is no room, or only `side` when even the logo
/// doesn't fit.
pub fn render(columns: usize, side: &[String]) -> Vec<String> {
    if columns < LOGO_WIDTH {
        return side.to_vec();
    }
    let truecolor = std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit");
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
