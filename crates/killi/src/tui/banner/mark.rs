//! killi's icon turning in the blank rows between the banner and the prompt while a session has
//! no prompt yet: the icon's K, three rounded strokes given depth, and its coral dot as a ball.
//! Each turn eases in and out and rests a moment facing the viewer; while edge-on near the end
//! of a turn the K melts into an ice cube, and on the next turn back.
//!
//! Each cell is a braille character, a 2 by 4 grid of dots, every dot the mark covers drawn; the
//! cell's color is its dots' light averaged. The light is a studio's: a key light from above
//! left, a fill bounced from below right, a rim along the silhouette and a gloss, nearer parts
//! brighter, the surface swelling gently so the light shimmers as it turns.
//!
//! The timing, the pose, the studio light and the swell follow the empty-state animation of
//! OpenAI's Codex CLI (`codex-rs/tui/src/empty_state_animation`, Apache License 2.0), which
//! turns and morphs the Codex and OpenAI marks the same way; the shapes, their casting and the
//! colors here are killi's.

use std::f32::consts::TAU;

use super::{RESET, color, gradient, is_light, truecolor};

/// Tallest the mark is drawn, in rows; it is twice as many columns wide, since a cell is about
/// twice as tall as it is wide.
const MOST_ROWS: usize = 16;
/// Fewest rows worth drawing it in; a smaller window keeps its blank rows.
const LEAST_ROWS: usize = 7;
/// Seconds for one turn and the rest after it (two make a loop: K to cube, cube to K), and the
/// share of them the turn takes.
const BREATH: f32 = 3.6;
const TURNING: f32 = 0.82;
/// How far the mark tips toward the viewer at its most, with the turn, and rolls in the
/// screen's plane over a loop (radians).
const WOBBLE: f32 = 0.112;
const ROLL: f32 = 0.045;
/// The camera's distance from the mark's middle.
const DISTANCE: f32 = 5.3;
/// How far from its middle the mark reaches, its ball and stroke ends included.
const REACH: f32 = 1.55;
/// The K's strokes from docs/icon.svg (a 512 square, the K's stem at x 158), centered on the
/// mark and scaled down by 120: the stem, the arm and the leg.
const STROKES: [([f32; 2], [f32; 2]); 3] = [
    ([-0.84, 0.81], [-0.84, -0.99]),
    ([-0.84, -0.06], [0.28, 0.66]),
    ([-0.84, -0.06], [0.31, -0.86]),
];
/// Half the strokes' width, half their depth and how round their edges are.
const STROKE: f32 = 0.22;
const DEPTH: f32 = 0.2;
const ROUND: f32 = 0.09;
/// Half the ice cube's side and how round its edges are.
const CUBE: f32 = 0.78;
const CUBE_ROUND: f32 = 0.14;
/// The cube's own turn and tip (radians).
const CUBE_TURN: f32 = 0.62;
const CUBE_TIP: f32 = 0.5;
/// The icon's dot over the K: its middle and radius.
const BALL: [f32; 3] = [0.81, 0.96, 0.0];
const BALL_RADIUS: f32 = 0.25;
/// The dot's coral, as the icon has it.
const CORAL: Rgb = [239.0, 104.0, 104.0];
/// Each dot's bit in a braille character, by its (column, row) in the cell.
const BRAILLE: [[u32; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

type Rgb = [f32; 3];

/// The mark `seconds` into its breathing, centered in a canvas `rows` tall and `columns` wide:
/// always `rows` rows, none wider than `columns`, all blank when it doesn't fit.
pub fn mark(seconds: f32, rows: usize, columns: usize) -> Vec<String> {
    let mut out = vec![String::new(); rows];
    let size = rows.saturating_sub(2).min(MOST_ROWS);
    if size < LEAST_ROWS || columns < 2 * size + 2 {
        return out;
    }
    let (height, width) = (size, 2 * size);
    let dots = Dots::at(seconds, 4 * height, 2 * width);
    let lights = [Studio::k(), Studio::ball()];
    let left = " ".repeat((columns - width) / 2);
    let top = (rows - height) / 2;
    let truecolor = truecolor();
    for y in 0..height {
        let cells: Vec<Option<(char, Rgb)>> =
            (0..width).map(|x| dots.cell(x, y, &lights)).collect();
        let Some(end) = cells.iter().rposition(Option::is_some) else {
            continue;
        };
        let mut row = left.clone();
        for cell in &cells[..=end] {
            match cell {
                Some((glyph, [r, g, b])) => {
                    row.push_str(&color(*r, *g, *b, truecolor));
                    row.push(*glyph);
                }
                None => row.push(' '),
            }
        }
        row.push_str(RESET);
        out[top + y] = row;
    }
    out
}

/// The colors a surface takes under each light, and the ground it fades into with distance.
struct Studio {
    ground: Rgb,
    shadow: Rgb,
    key: Rgb,
    fill: Rgb,
    rim: Rgb,
    highlight: Rgb,
}

impl Studio {
    /// The K in the theme's gradient: its shade the first stop deepened, its light the middle
    /// stop toward white, the fill the first stop and the rim the last, as the icon's pale
    /// blues are lit. On a light ground everything leans to the ground instead.
    fn k() -> Self {
        let [first, middle, last] = [0.0, 0.5, 1.0].map(|t| {
            let (r, g, b) = gradient(t);
            [r, g, b]
        });
        Self::lit(first, middle, last)
    }

    /// The coral ball, lit the same way.
    fn ball() -> Self {
        Self::lit(CORAL, mix(CORAL, [255.0, 220.0, 210.0], 0.3), CORAL)
    }

    fn lit(first: Rgb, middle: Rgb, last: Rgb) -> Self {
        if is_light() {
            let ground = [250.0, 250.0, 248.0];
            return Self {
                ground,
                shadow: mix(ground, first, 0.45),
                key: middle,
                fill: first,
                rim: last,
                highlight: mix(middle, [0.0; 3], 0.25),
            };
        }
        Self {
            ground: [16.0, 18.0, 24.0],
            shadow: mix(first, [16.0, 22.0, 40.0], 0.55),
            key: mix(middle, [255.0; 3], 0.62),
            fill: first,
            rim: last,
            highlight: [255.0, 246.0, 225.0],
        }
    }

    /// The color of a surface facing `normal` (toward the viewer is +z, up is +y) at depth `z`
    /// (toward the viewer).
    fn shade(&self, [nx, ny, nz]: Rgb, z: f32) -> Rgb {
        let diffuse = (-0.41 * nx + 0.564 * ny + 0.718 * nz).clamp(0.0, 1.0);
        let bounce = (0.55 * nx - 0.2 * ny - 0.35 * nz).clamp(0.0, 1.0) * (1.0 - diffuse) * 0.38;
        let edge =
            (1.0 - nz.abs().min(1.0)).powf(2.4) * (0.85 * nx + 0.38 * ny).clamp(0.0, 1.0) * 0.82;
        let gloss = (-0.22 * nx + 0.302 * ny + 0.928 * nz)
            .clamp(0.0, 1.0)
            .powi(18)
            * 0.96;
        let base = (1.0 - bounce) * (1.0 - edge) * (1.0 - gloss);
        let gain = (0.9 + z * 0.15).clamp(0.68, 1.0);
        std::array::from_fn(|i| {
            let value = self.shadow[i] * (1.0 - diffuse) * base
                + self.key[i] * diffuse * base
                + self.fill[i] * bounce * (1.0 - edge) * (1.0 - gloss)
                + self.rim[i] * edge * (1.0 - gloss)
                + self.highlight[i] * gloss;
            self.ground[i] + (value - self.ground[i]) * gain
        })
    }
}

/// `a` moved `t` of the way to `b`.
fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// Eases 0 to 1 in and out, flat at both ends.
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// What a dot of the grid shows: the surface's normal and depth as seen, and whether it is the
/// ball.
#[derive(Clone, Copy)]
struct Dot {
    normal: Rgb,
    z: f32,
    ball: bool,
}

/// Where the mark is `seconds` into its loop: its turn about upright, its tip toward the
/// viewer and its roll (radians), and how far it has melted from the K to the cube (0 to 1).
fn pose(seconds: f32) -> (f32, f32, f32, f32) {
    let breaths = seconds / BREATH;
    let spin = smooth(breaths.rem_euclid(1.0) / TURNING);
    let turn = (breaths.floor() + spin) * TAU;
    // The melt runs over the last third of each turn, while the mark is edge-on and back.
    let melt = smooth((spin - 2.0 / 3.0) * 3.0);
    let to_cube = breaths.rem_euclid(2.0) < 1.0;
    let melted = if to_cube { melt } else { 1.0 - melt };
    let roll = (breaths / 2.0 * TAU).sin() * ROLL;
    (turn, turn.sin() * WOBBLE, roll, melted)
}

/// The mark cast onto a grid of dots `height` by `width`.
struct Dots {
    width: usize,
    dots: Vec<Option<Dot>>,
}

impl Dots {
    fn at(seconds: f32, height: usize, width: usize) -> Self {
        let (turn, tip, roll, melted) = pose(seconds);
        let shape = Shape { melted };
        let (st, ct, sp, cp, sr, cr) = (
            turn.sin(),
            turn.cos(),
            tip.sin(),
            tip.cos(),
            roll.sin(),
            roll.cos(),
        );
        // Out of the mark's frame: turn about upright, tip toward the viewer, roll in the
        // screen's plane; and back in, the same in reverse.
        let rotate = |[x, y, z]: Rgb| {
            let (x, z) = (x * ct + z * st, z * ct - x * st);
            let (y, z) = (y * cp - z * sp, y * sp + z * cp);
            [x * cr - y * sr, x * sr + y * cr, z]
        };
        let unrotate = |[x, y, z]: Rgb| {
            let (x, y) = (x * cr + y * sr, y * cr - x * sr);
            let (y, z) = (y * cp + z * sp, z * cp - y * sp);
            [x * ct - z * st, y, x * st + z * ct]
        };
        // The nearest of the mark is magnified D/(D-REACH); the scale (dots per unit) keeps
        // even that inside the grid.
        let scale = (height as f32 / 2.0 - 1.0) / (REACH * DISTANCE / (DISTANCE - REACH));
        let origin = unrotate([0.0, 0.0, DISTANCE]);
        let mut dots = vec![None; height * width];
        for row in 0..height {
            for column in 0..width {
                let x = (column as f32 + 0.5 - width as f32 / 2.0) / scale;
                let y = (height as f32 / 2.0 - row as f32 - 0.5) / scale;
                let direction = normalize(unrotate([x, y, -DISTANCE]));
                let Some(at) = shape.march(origin, direction) else {
                    continue;
                };
                dots[row * width + column] = Some(Dot {
                    normal: rotate(ripple(at, shape.normal(at))),
                    z: rotate(at)[2],
                    ball: shape.ball(at) < shape.body(at),
                });
            }
        }
        Self { width, dots }
    }

    /// The braille character for cell (`x`, `y`) and its dots' colors averaged, or None when
    /// the mark doesn't reach it.
    fn cell(&self, x: usize, y: usize, lights: &[Studio; 2]) -> Option<(char, Rgb)> {
        let (mut bits, mut sum, mut count) = (0, [0.0; 3], 0.0);
        for (dy, row) in BRAILLE.iter().enumerate() {
            for (dx, bit) in row.iter().enumerate() {
                let Some(dot) = self.dots[(4 * y + dy) * self.width + 2 * x + dx] else {
                    continue;
                };
                bits |= bit;
                let rgb = lights[usize::from(dot.ball)].shade(dot.normal, dot.z);
                sum = std::array::from_fn(|i| sum[i] + rgb[i]);
                count += 1.0;
            }
        }
        (bits > 0).then(|| {
            (
                char::from_u32(0x2800 + bits).expect("braille"),
                sum.map(|channel| channel / count),
            )
        })
    }
}

/// `normal` at `p` tilted by a gentle swell across the surface, so the light shimmers over
/// the faces as the mark turns.
fn ripple(p: Rgb, normal: Rgb) -> Rgb {
    let u = p[0] * 2.2 + p[1] * 0.7;
    let v = p[1] * 2.8 - p[0] * 0.4;
    let tilt = normal[2];
    normalize([
        normal[0] - tilt * (u.cos() * 0.165 - v.cos() * 0.016),
        normal[1] - tilt * (u.cos() * 0.0525 + v.cos() * 0.112),
        normal[2],
    ])
}

/// The mark part way through melting from the K (0) to the ice cube (1).
struct Shape {
    melted: f32,
}

impl Shape {
    /// Where a ray from `origin` along the unit `direction` first meets the mark, stepping by
    /// (most of) the distance to it; None when it passes by.
    fn march(&self, origin: Rgb, direction: Rgb) -> Option<Rgb> {
        // Skip rays that miss the sphere the mark sits in.
        let along = -dot(origin, direction);
        let closest = [0, 1, 2].map(|i| origin[i] + direction[i] * along);
        if dot(closest, closest) > REACH * REACH {
            return None;
        }
        let mut t = along - REACH;
        for _ in 0..64 {
            let at = [0, 1, 2].map(|i| origin[i] + direction[i] * t);
            let d = self.distance(at);
            if d < 0.003 {
                return Some(at);
            }
            // Mid-melt the distance is a blend of two, which can overstate it a little.
            t += d * 0.85;
            if t > along + REACH {
                return None;
            }
        }
        None
    }

    /// The surface normal at `p`, from how the distance to the mark changes around `p`.
    fn normal(&self, p: Rgb) -> Rgb {
        let e = 0.003;
        let along = |i: usize| {
            let (mut a, mut b) = (p, p);
            a[i] += e;
            b[i] -= e;
            self.distance(a) - self.distance(b)
        };
        normalize([along(0), along(1), along(2)])
    }

    /// How far `p` is from the mark: the nearer of its body and the ball.
    fn distance(&self, p: Rgb) -> f32 {
        self.body(p).min(self.ball(p))
    }

    /// How far `p` is from the K melting into the cube.
    fn body(&self, p: Rgb) -> f32 {
        match self.melted {
            0.0 => k(p),
            1.0 => cube(p),
            t => k(p) * (1.0 - t) + cube(p) * t,
        }
    }

    /// How far `p` is from the coral ball, which shrinks away as the K melts. Once it is
    /// smaller than a dot it is gone, not a point a ray can still catch.
    fn ball(&self, p: Rgb) -> f32 {
        let radius = BALL_RADIUS * (1.0 - self.melted);
        if radius < 0.01 {
            return f32::INFINITY;
        }
        let d = [p[0] - BALL[0], p[1] - BALL[1], p[2] - BALL[2]];
        dot(d, d).sqrt() - radius
    }
}

/// How far `p` is from the K: its strokes in the flat, given depth, their edges rounded.
fn k(p: Rgb) -> f32 {
    let flat = STROKES
        .iter()
        .map(|&(a, b)| segment([p[0], p[1]], a, b))
        .fold(f32::INFINITY, f32::min)
        - (STROKE - ROUND);
    let deep = p[2].abs() - (DEPTH - ROUND);
    let outside = (flat.max(0.0).powi(2) + deep.max(0.0).powi(2)).sqrt();
    outside + flat.max(deep).min(0.0) - ROUND
}

/// How far `p` is from the ice cube, its edges rounded. It sits turned and tipped on its own,
/// so facing the viewer it shows a corner and three faces rather than a flat square.
fn cube(p: Rgb) -> f32 {
    let (st, ct, sp, cp) = (
        CUBE_TURN.sin(),
        CUBE_TURN.cos(),
        CUBE_TIP.sin(),
        CUBE_TIP.cos(),
    );
    let (y, z) = (p[1] * cp + p[2] * sp, p[2] * cp - p[1] * sp);
    let p = [p[0] * ct - z * st, y, p[0] * st + z * ct];
    let q = p.map(|c| c.abs() - (CUBE - CUBE_ROUND));
    let outside = q.map(|c| c.max(0.0));
    dot(outside, outside).sqrt() + q[0].max(q[1]).max(q[2]).min(0.0) - CUBE_ROUND
}

/// How far the point `p` is from the segment from `a` to `b`.
fn segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (pa, ba) = ([p[0] - a[0], p[1] - a[1]], [b[0] - a[0], b[1] - a[1]]);
    let h = ((pa[0] * ba[0] + pa[1] * ba[1]) / (ba[0] * ba[0] + ba[1] * ba[1])).clamp(0.0, 1.0);
    let (x, y) = (pa[0] - ba[0] * h, pa[1] - ba[1] * h);
    (x * x + y * y).sqrt()
}

fn dot(a: Rgb, b: Rgb) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: Rgb) -> Rgb {
    let length = dot(v, v).sqrt().max(1e-9);
    [v[0] / length, v[1] / length, v[2] / length]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::text::width;

    /// Cells holding a drawn braille character.
    fn drawn(canvas: &[String]) -> usize {
        canvas
            .iter()
            .flat_map(|row| row.chars())
            .filter(|c| ('\u{2801}'..='\u{28ff}').contains(c))
            .count()
    }

    #[test]
    fn the_mark_fills_its_canvas_and_never_runs_past_it() {
        for (rows, columns) in [(20, 118), (12, 80), (9, 30), (30, 60)] {
            for seconds in [0.0, 0.7, 1.5, 2.4, 3.0, 5.5, 33.3] {
                let canvas = mark(seconds, rows, columns);
                assert_eq!(canvas.len(), rows);
                assert!(
                    canvas.iter().all(|row| width(row) <= columns),
                    "{rows}x{columns}"
                );
                let size = (rows - 2).min(MOST_ROWS);
                assert!(
                    drawn(&canvas) >= size,
                    "{rows}x{columns} at {seconds}s: the mark is drawn, edge-on too"
                );
            }
        }
    }

    #[test]
    fn the_whole_mark_stays_inside_the_grid_as_it_turns_and_melts() {
        for tenth in 0..(2.0 * BREATH * 10.0) as usize {
            let dots = Dots::at(tenth as f32 / 10.0, 64, 64);
            for i in 0..64 {
                for at in [i, 63 * 64 + i, i * 64, i * 64 + 63] {
                    assert!(
                        dots.dots[at].is_none(),
                        "{tenth} tenths: touches the border"
                    );
                }
            }
        }
    }

    #[test]
    fn each_turn_eases_rests_and_melts_between_the_k_and_the_cube() {
        assert_ne!(mark(1.0, 20, 80), mark(1.1, 20, 80), "turning");
        let resting = BREATH * TURNING + 0.1;
        for (a, b) in [
            (resting, BREATH - 0.05),
            (BREATH + resting, 2.0 * BREATH - 0.05),
        ] {
            let ((turn_a, _, _, melted_a), (turn_b, _, _, melted_b)) = (pose(a), pose(b));
            assert_eq!((turn_a, melted_a), (turn_b, melted_b), "resting");
        }
        let melted = |seconds: f32| pose(seconds).3;
        assert_eq!(melted(0.0), 0.0, "the K to start");
        assert_eq!(melted(BREATH + 0.1), 1.0, "the cube after one turn");
        assert_eq!(melted(2.0 * BREATH + 0.1), 0.0, "the K again after two");
        let mid = melted(BREATH * TURNING * 0.85);
        assert!(
            mid > 0.0 && mid < 1.0,
            "melting near the end of a turn: {mid}"
        );
        assert!(
            (pose(2.0 * BREATH).0 - 2.0 * TAU).abs() < 1e-4,
            "a loop is two whole turns"
        );
    }

    #[test]
    fn the_k_shows_its_coral_dot_and_the_cube_none() {
        let shown = |seconds: f32| -> Vec<Dot> {
            Dots::at(seconds, 64, 64)
                .dots
                .iter()
                .flatten()
                .copied()
                .collect()
        };
        let k = shown(0.0);
        let balls = k.iter().filter(|d| d.ball).count();
        assert!(balls > 0, "the dot");
        assert!(k.len() > 5 * balls, "the K is most of it");
        for tenth in 0..(2.0 * BREATH * 10.0) as usize {
            let seconds = tenth as f32 / 10.0;
            if pose(seconds).3 == 1.0 {
                assert!(
                    shown(seconds).iter().all(|d| !d.ball),
                    "the cube has no dot at {seconds}s"
                );
            }
        }
        let studio = Studio::k();
        let brightness: Vec<f32> = k
            .iter()
            .map(|d| studio.shade(d.normal, d.z).iter().sum())
            .collect();
        let (low, high) = brightness
            .iter()
            .fold((f32::MAX, 0.0f32), |(l, h), &b| (l.min(b), h.max(b)));
        assert!(high > 1.5 * low, "shaded and lit: {low}..{high}");
    }

    #[test]
    fn a_canvas_too_small_for_the_mark_stays_blank() {
        for (rows, columns) in [(0, 80), (LEAST_ROWS + 1, 80), (20, 15)] {
            let canvas = mark(3.0, rows, columns);
            assert_eq!(canvas.len(), rows);
            assert!(canvas.iter().all(String::is_empty), "{rows}x{columns}");
        }
    }
}
