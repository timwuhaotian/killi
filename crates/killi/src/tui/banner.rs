//! The start-up banner: killi in block letters with a sky-to-violet gradient, and a few lines
//! about the session beside it.

use super::text::{self, BOLD, DIM, FAINT, RESET, SELECT, is_light, truecolor, width};

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
const STOPS_DARK: Stops = [
    (79.0, 209.0, 197.0),
    (100.0, 188.0, 237.0),
    (157.0, 152.0, 242.0),
];
/// The same hues, deeper, for a light terminal.
const STOPS_LIGHT: Stops = [
    (4.0, 121.0, 114.0),
    (1.0, 114.0, 159.0),
    (93.0, 83.0, 174.0),
];
/// Each skin's gradient, from its palette: batman's signal yellow through its olive to its
/// slate, iced coffee's blue through its pale blue to its coral, creamery station's butter
/// through its tans, and the rest each across the three shades of theirs that read brightest.
const STOPS_BATMAN: Stops = [
    (253.0, 255.0, 0.0),
    (191.0, 168.0, 63.0),
    (126.0, 147.0, 189.0),
];
const STOPS_ICED_COFFEE: Stops = [
    (111.0, 168.0, 220.0),
    (207.0, 226.0, 243.0),
    (239.0, 104.0, 104.0),
];
const STOPS_CREAMERY: Stops = [
    (252.0, 233.0, 178.0),
    (222.0, 192.0, 146.0),
    (202.0, 178.0, 151.0),
];
const STOPS_PASTEL: Stops = [
    (255.0, 156.0, 210.0),
    (203.0, 190.0, 255.0),
    (217.0, 236.0, 255.0),
];
const STOPS_CAPPUCCINO: Stops = [
    (255.0, 244.0, 230.0),
    (190.0, 155.0, 123.0),
    (201.0, 120.0, 110.0),
];
const STOPS_GRAY: Stops = [
    (240.0, 240.0, 240.0),
    (170.0, 170.0, 170.0),
    (119.0, 119.0, 119.0),
];
const STOPS_OFF_WHITE: Stops = [
    (255.0, 255.0, 242.0),
    (251.0, 247.0, 245.0),
    (249.0, 241.0, 241.0),
];
const STOPS_RED_ORANGE: Stops = [
    (255.0, 193.0, 0.0),
    (255.0, 116.0, 0.0),
    (255.0, 0.0, 0.0),
];
const STOPS_SNOW_ICE: Stops = [
    (255.0, 255.0, 255.0),
    (134.0, 214.0, 216.0),
    (32.0, 195.0, 208.0),
];

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

/// The wordmark's word.
const WORD: &str = "killi";

/// The logo's gradient at `across` (0 to 1 along its piece, `shift`ed half way round for
/// words so the two never match) with the spinner's drift at `frame`, dimmed by `fade`.
fn flow(frame: usize, across: f32, shift: f32, fade: f32) -> String {
    let drift = frame as f32 * 0.02;
    let t = (across + shift + drift) % 2.0;
    let (r, g, b) = gradient(if t > 1.0 { 2.0 - t } else { t });
    color(r * fade, g * fade, b * fade, truecolor())
}

/// The snowflake spinner's glyphs, in the order they pulse: light to heavy and back again,
/// the way kimi-code's moon turns through its phases. All from the snowflake block, so no
/// frame jumps in weight the way the asterisk did.
const SNOWFLAKES: [char; 4] = ['❄', '❅', '❆', '❅'];
/// Frames (a tenth of a second each) each snowflake glyph holds before the next turns in,
/// so the flake rotates slowly instead of flickering.
const SNOWFLAKE_HOLD: usize = 4;
/// Frames (a tenth of a second each) for the snowflake's full bright-to-bright cycle.
const WAX_CYCLE: f32 = 24.0;
/// The spinner `frame` tenths of a second in: one bold snowflake in a single column,
/// turning through its variants in the logo's gradient while its light waxes and wanes,
/// so the working line's icon changes shape in place instead of spreading out.
pub fn snowflake(frame: usize) -> String {
    let c = SNOWFLAKES[(frame / SNOWFLAKE_HOLD) % SNOWFLAKES.len()];
    let t = frame as f32 / WAX_CYCLE;
    let wax = (1.0 - (std::f32::consts::TAU * t).cos()) / 2.0;
    let fade = 0.55 + 0.45 * wax;
    let across = (0.5 + frame as f32 * 0.012) % 2.0;
    let (r, g, b) = gradient(if across > 1.0 { 2.0 - across } else { across });
    format!(
        "{BOLD}{}{c}{RESET}",
        color(r * fade, g * fade, b * fade, truecolor())
    )
}

/// The working line's island: `inner` on the selection ground with a space of it on each
/// side. The ground is reapplied after every reset inside, since a reset would end it.
fn island(inner: String) -> String {
    let ground = SELECT.to_string();
    format!(
        "{ground} {} {RESET}",
        inner.replace(RESET, &format!("{RESET}{ground}"))
    )
}

/// The spinner `frame` tenths of a second in, as the session's working line shows it and
/// `killi update` mirrors it: the island of the snowflake and the action word fading in
/// beside it. Always the snowflake, a space and the word, with a space of ground each side.
pub fn working_island(frame: usize, word: &str) -> String {
    island(format!("{} {}", snowflake(frame), action_word(frame, word)))
}

/// The same island with the word shown whole at once, for work too brief to fade in.
pub fn steady_island(frame: usize, word: &str) -> String {
    island(format!("{} {}", snowflake(frame), steady_word(frame, word)))
}

/// `word` fading in letter by letter beside the snowflake, each letter taking the logo's
/// gradient as it flows with `frame`; letters not yet there are dim dots. Two sparkles
/// travel across the letters against each other, so the word shimmers while it works.
pub fn action_word(frame: usize, word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let lit = sparkles(frame, chars.len());
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        let appeared = (frame as f32 - 4.0 * i as f32 + 2.0) / 6.0;
        if appeared <= 0.0 {
            out.push_str(&format!("{DIM}·{RESET}"));
            continue;
        }
        let (bold, fade) = match lit.contains(&i) {
            true => (BOLD, 1.0),
            false => ("", appeared.clamp(0.0, 1.0) * 0.75 + 0.25),
        };
        out.push_str(bold);
        out.push_str(&flow(
            frame,
            i as f32 / (chars.len() - 1).max(1) as f32,
            0.5,
            fade,
        ));
        out.push(*c);
        out.push_str(RESET);
    }
    out
}

/// `word` shown whole at once beside the snowflake, in the logo's gradient flowing with
/// `frame`, with the same two sparkles traveling across it as `action_word`.
pub fn steady_word(frame: usize, word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let lit = sparkles(frame, chars.len());
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        let bold = match lit.contains(&i) {
            true => BOLD,
            false => "",
        };
        out.push_str(bold);
        out.push_str(&flow(
            frame,
            i as f32 / (chars.len() - 1).max(1) as f32,
            0.5,
            1.0,
        ));
        out.push(*c);
        out.push_str(RESET);
    }
    out
}

/// The letters the two sparkles sit on `frame` tenths of a second in: one crosses left to
/// right a letter per frame, the other right to left a letter every two frames, so the
/// pair keeps falling in and out of step — and lands on one letter when they meet.
fn sparkles(frame: usize, len: usize) -> [usize; 2] {
    let len = len.max(1);
    [frame % len, len - 1 - (frame / 2) % len]
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
    let logo: Vec<String> = LOGO
        .iter()
        .enumerate()
        .map(|(y, row)| paint(row, y, truecolor))
        .collect();
    let room = columns.saturating_sub(LOGO_WIDTH + GAP.len());
    if room < 24 || side.iter().any(|line| width(line) > room) && room < 40 {
        return logo.into_iter().chain(side.iter().cloned()).collect();
    }
    let mut rows = logo;
    for (i, line) in side.iter().enumerate() {
        match rows.get_mut(i + 1) {
            Some(row) => row.push_str(&format!("{GAP}{}", super::fit(line, room))),
            None => rows.push(format!(
                "{}{GAP}{}",
                " ".repeat(LOGO_WIDTH),
                super::fit(line, room)
            )),
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
    let stops = match text::skin_name() {
        "batman" => &STOPS_BATMAN,
        "iced-coffee" => &STOPS_ICED_COFFEE,
        "creamery-station" => &STOPS_CREAMERY,
        "pastel-tweencore" => &STOPS_PASTEL,
        "cappuccino" => &STOPS_CAPPUCCINO,
        "shades-of-gray" => &STOPS_GRAY,
        "off-white" => &STOPS_OFF_WHITE,
        "red-orange" => &STOPS_RED_ORANGE,
        "snow-and-ice" => &STOPS_SNOW_ICE,
        _ if is_light() => &STOPS_LIGHT,
        _ => &STOPS_DARK,
    };
    blend(stops, t)
}

/// The color `t` of the way, from 0 to 1, along `stops`.
fn blend(stops: &Stops, t: f32) -> (f32, f32, f32) {
    let t = t.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
    let i = (t.floor() as usize).min(stops.len() - 2);
    let f = t - i as f32;
    let (a, b) = (stops[i], stops[i + 1]);
    (
        a.0 + (b.0 - a.0) * f,
        a.1 + (b.1 - a.1) * f,
        a.2 + (b.2 - a.2) * f,
    )
}

/// A 24-bit color, or the nearest in the 256-color cube for terminals without it.
fn color(r: f32, g: f32, b: f32, truecolor: bool) -> String {
    if truecolor {
        return format!("\x1b[38;2;{};{};{}m", r as u8, g as u8, b as u8);
    }
    let level = |v: f32| (v / 255.0 * 5.0).round() as u8;
    format!(
        "\x1b[38;5;{}m",
        16 + 36 * level(r) + 6 * level(g) + level(b)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_logo_rows_are_all_one_width() {
        assert!(LOGO.iter().all(|row| width(row) == LOGO_WIDTH));
    }

    #[test]
    fn the_islands_are_always_the_same_width_and_carry_their_ground() {
        for frame in 0..200 {
            assert_eq!(
                width(&working_island(frame, "downloading")),
                1 + 1 + 1 + 11 + 1,
                "frame {frame}"
            );
            assert_eq!(
                width(&steady_island(frame, "contrapposto")),
                1 + 1 + 1 + 12 + 1,
                "frame {frame}"
            );
        }
        let ground = SELECT.to_string();
        let island = working_island(30, "thinking");
        assert!(island.starts_with(&ground) && island.ends_with(RESET), "{island:?}");
        assert!(
            island.matches(&ground).count() > 1,
            "the ground is reapplied inside: {island:?}"
        );
        assert_eq!(unstyled(&working_island(40, "checking")), " ❆ checking ");
        assert_eq!(unstyled(&steady_island(0, "checking")), " ❄ checking ");
    }

    #[test]
    fn the_words_fade_in_or_show_whole_and_keep_their_width() {
        for frame in 0..60 {
            assert_eq!(width(&action_word(frame, "thinking")), 8, "frame {frame}");
            assert_eq!(
                width(&steady_word(frame, "chiaroscuro")),
                "chiaroscuro".len(),
                "frame {frame}"
            );
        }
        assert!(action_word(0, "thinking").contains('·'));
        assert!(!action_word(30, "thinking").contains('·'));
        assert!(
            !steady_word(0, "chiaroscuro").contains('·'),
            "the whole word shows at once"
        );
    }

    #[test]
    fn the_snowflake_is_one_column_and_turns_slowly() {
        for frame in 0..120 {
            assert_eq!(width(&snowflake(frame)), 1, "frame {frame}");
        }
        let glyph = |frame| unstyled(&snowflake(frame));
        assert!(
            (1..SNOWFLAKE_HOLD).all(|frame| glyph(frame) == glyph(0)),
            "the flake flickers"
        );
        assert_ne!(glyph(0), glyph(SNOWFLAKE_HOLD), "the flake stands still");
        let mut glyphs = std::collections::HashSet::new();
        for frame in (0..SNOWFLAKES.len() * SNOWFLAKE_HOLD).step_by(SNOWFLAKE_HOLD) {
            glyphs.insert(unstyled(&snowflake(frame)));
        }
        let shown: String = glyphs.iter().cloned().collect();
        assert_eq!(glyphs.len(), 3, "one glyph at a time, in place: {glyphs:?}");
        for c in SNOWFLAKES {
            assert!(shown.contains(c), "missing {c}: {glyphs:?}");
        }
    }

    #[test]
    fn the_snowflake_is_bold() {
        for frame in 0..40 {
            let flake = snowflake(frame);
            let parts: Vec<&str> = flake.split(RESET).collect();
            assert_eq!(parts.len(), 2, "frame {frame}: the flake alone");
            assert!(parts[0].contains(BOLD), "frame {frame}: the flake is bold");
        }
    }

    #[test]
    fn two_sparkles_cross_the_action_and_snapshot_words() {
        for frame in 60..84 {
            for styled in [
                action_word(frame, "thinking"),
                steady_word(frame, "thinking"),
            ] {
                assert_eq!(
                    sweep_letters(&styled),
                    lit_letters("thinking", frame),
                    "frame {frame}: two sparkles, one where they meet"
                );
            }
            assert_eq!(
                sweep_letters(&steady_word(frame, "chiaroscuro")),
                lit_letters("chiaroscuro", frame),
                "frame {frame}"
            );
        }
        // The pair is apart more often than together: two highlights, not one in disguise.
        let apart = (60..84)
            .filter(|&frame| sweep_letters(&steady_word(frame, "thinking")).len() == 2)
            .count();
        assert!(apart > 12, "the sparkles rarely part: {apart}/24");
    }

    /// The letters of `word` the sparkles light at `frame`, in word order.
    fn lit_letters(word: &str, frame: usize) -> Vec<char> {
        let lit = sparkles(frame, word.chars().count());
        word.chars()
            .enumerate()
            .filter(|(i, _)| lit.contains(i))
            .map(|(_, c)| c)
            .collect()
    }

    /// The letters the sweeps highlight: the first character after each BOLD past its color.
    fn sweep_letters(styled: &str) -> Vec<char> {
        styled
            .match_indices(BOLD)
            .map(|(at, _)| {
                let mut chars = styled[at + BOLD.len()..].chars();
                while let Some(c) = chars.next() {
                    if c == '\x1b' {
                        chars.by_ref().find(char::is_ascii_alphabetic);
                    } else {
                        return c;
                    }
                }
                panic!("no letter after the highlight");
            })
            .collect()
    }

    fn unstyled(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                chars.by_ref().find(char::is_ascii_alphabetic);
            } else {
                out.push(c);
            }
        }
        out
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
        assert_eq!(
            (
                bar(0.5, 4).matches('━').count(),
                bar(0.5, 4).matches('─').count()
            ),
            (2, 2)
        );
        assert_eq!(bar(7.0, 4), bar(1.0, 4), "past full is full");
    }

    #[test]
    fn the_logo_gives_way_to_the_wordmark_in_a_small_window() {
        assert!(!compact(118, true));
        assert!(compact(118, false), "too few rows for six of logo");
        assert!(
            compact(69, true),
            "too narrow for the setup lines beside it"
        );
        assert!(!compact(70, true));
        let mark = wordmark("0.3.0");
        assert_eq!(width(&mark), WORD.len() + 1 + 5);
        assert!(mark.contains(BOLD) && mark.ends_with(RESET));
    }

    #[test]
    fn the_gradient_starts_at_lagoon_and_deepens_for_a_light_terminal() {
        assert_eq!(
            blend(&STOPS_DARK, 0.0),
            (79.0, 209.0, 197.0),
            "lagoon's own color"
        );
        assert_eq!(blend(&STOPS_DARK, 1.0), STOPS_DARK[2]);
        assert_eq!(blend(&STOPS_DARK, 0.5), STOPS_DARK[1]);
        assert_eq!(
            blend(&STOPS_LIGHT, 7.0),
            STOPS_LIGHT[2],
            "past the end is the end"
        );
        let brightness = |(r, g, b): (f32, f32, f32)| r + g + b;
        assert!(
            STOPS_LIGHT
                .iter()
                .zip(STOPS_DARK)
                .all(|(light, dark)| brightness(*light) < brightness(dark))
        );
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
