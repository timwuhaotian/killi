//! The ice cube that turns in the blank rows between the banner and the prompt while a session
//! has no prompt yet: six faces of shaded dots in the logo's gradient, its edges lit.

use super::{RESET, color, gradient, truecolor};

/// Tallest the cube is drawn, in rows; it is twice as many columns wide, since a cell is about
/// twice as tall as it is wide.
const MOST_ROWS: usize = 16;
/// Fewest rows worth drawing it in; a smaller window keeps its blank rows.
const LEAST_ROWS: usize = 7;
/// The faces' dots from shaded to lit.
const RAMP: [char; 5] = ['.', '·', ':', '=', '+'];
/// What an edge, where two faces meet or a face meets the background, is drawn in.
const EDGE: char = '*';

/// The cube `frame` tenths of a second into its turn, centered in a canvas `rows` tall and
/// `columns` wide: always `rows` rows, none wider than `columns`, all blank when it doesn't fit.
pub fn cube(frame: usize, rows: usize, columns: usize) -> Vec<String> {
    let mut out = vec![String::new(); rows];
    let size = rows.saturating_sub(2).min(MOST_ROWS);
    if size < LEAST_ROWS || columns < 2 * size + 2 {
        return out;
    }
    let (height, width) = (size, 2 * size);
    let shading = shade(frame, height, width);
    let left = " ".repeat((columns - width) / 2);
    let top = (rows - height) / 2;
    let truecolor = truecolor();
    let drift = frame as f32 * 0.015;
    for (y, line) in shading.chunks(width).enumerate() {
        let mut row = left.clone();
        let end = line.iter().rposition(Option::is_some).map_or(0, |x| x + 1);
        for (x, cell) in line[..end].iter().enumerate() {
            let Some((light, edge)) = *cell else {
                row.push(' ');
                continue;
            };
            let t = (x as f32 / width as f32 + y as f32 / height as f32) / 2.0 + drift;
            let t = t % 2.0;
            let (r, g, b) = gradient(if t > 1.0 { 2.0 - t } else { t });
            let fade = if edge { 1.0 } else { 0.35 + 0.6 * light };
            row.push_str(&color(r * fade, g * fade, b * fade, truecolor));
            row.push(match edge {
                true => EDGE,
                false => RAMP[((light * RAMP.len() as f32) as usize).min(RAMP.len() - 1)],
            });
        }
        if end > 0 {
            row.push_str(RESET);
            out[top + y] = row;
        }
    }
    out
}

/// Each cell of a `height` by `width` canvas: the light on the nearest face there (0 to 1) and
/// whether it is an edge, or None where the cube isn't. The cube turns about its upright axis
/// a full turn about every ten seconds, from a corner toward the viewer, tipped toward them
/// and rocking gently.
fn shade(frame: usize, height: usize, width: usize) -> Vec<Option<(f32, bool)>> {
    let seconds = frame as f32 / 10.0;
    let turn = 0.6 + seconds * 0.6;
    let tip = 0.55 + 0.15 * (seconds * 0.4).sin();
    let rotate = |[x, y, z]: [f32; 3]| {
        let (x, z) = (x * turn.cos() + z * turn.sin(), z * turn.cos() - x * turn.sin());
        let (y, z) = (y * tip.cos() - z * tip.sin(), y * tip.sin() + z * tip.cos());
        [x, y, z]
    };
    let light = normalize([-0.4, 0.6, 0.7]);
    // The cube's corners reach √3 from its middle; perspective from 6 away makes the nearest
    // 6/(6-√3) larger, and the scale keeps even those inside the canvas.
    const DISTANCE: f32 = 6.0;
    let scale = (height as f32 / 2.0 - 0.5) / (3f32.sqrt() * DISTANCE / (DISTANCE - 3f32.sqrt()));
    let (middle_x, middle_y) = (width as f32 / 2.0, height as f32 / 2.0);
    let mut depth = vec![f32::NEG_INFINITY; height * width];
    let mut faces: Vec<Option<(usize, f32)>> = vec![None; height * width];
    let steps = 4 * height;
    for axis in 0..3 {
        for sign in [-1.0f32, 1.0] {
            let mut normal = [0.0; 3];
            normal[axis] = sign;
            let normal = rotate(normal);
            if normal[2] <= 0.0 {
                continue;
            }
            let lit = dot(normal, light).max(0.0);
            for i in 0..=steps {
                for j in 0..=steps {
                    let u = -1.0 + 2.0 * i as f32 / steps as f32;
                    let v = -1.0 + 2.0 * j as f32 / steps as f32;
                    let mut point = [0.0; 3];
                    point[axis] = sign;
                    point[(axis + 1) % 3] = u;
                    point[(axis + 2) % 3] = v;
                    let [x, y, z] = rotate(point);
                    let near = DISTANCE / (DISTANCE - z);
                    let column = (middle_x + 2.0 * x * near * scale).floor();
                    let row = (middle_y - y * near * scale).floor();
                    if column < 0.0 || row < 0.0 {
                        continue;
                    }
                    let (column, row) = (column as usize, row as usize);
                    if column >= width || row >= height {
                        continue;
                    }
                    let at = row * width + column;
                    if z > depth[at] {
                        depth[at] = z;
                        faces[at] = Some((2 * axis + usize::from(sign > 0.0), lit));
                    }
                }
            }
        }
    }
    // An edge is a cell whose neighbor to the right or below shows another face or none, or
    // whose neighbor to the left or above shows none: one cell wide all round.
    let face = |row: usize, column: usize| faces[row * width + column].map(|(face, _)| face);
    (0..height * width)
        .map(|at| {
            let (row, column) = (at / width, at % width);
            let (own, lit) = faces[at]?;
            let differs = |other: Option<usize>| other != Some(own);
            let edge = column + 1 == width
                || row + 1 == height
                || column == 0
                || row == 0
                || differs(face(row, column + 1))
                || differs(face(row + 1, column))
                || face(row, column - 1).is_none()
                || face(row - 1, column).is_none();
            Some((lit, edge))
        })
        .collect()
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let length = dot(v, v).sqrt();
    [v[0] / length, v[1] / length, v[2] / length]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::text::width;

    #[test]
    fn the_cube_fills_its_canvas_and_never_runs_past_it() {
        for (rows, columns) in [(20, 118), (12, 80), (9, 30), (30, 60)] {
            for frame in [0, 7, 50, 333] {
                let canvas = cube(frame, rows, columns);
                assert_eq!(canvas.len(), rows);
                assert!(canvas.iter().all(|row| width(row) <= columns), "{rows}x{columns}");
                assert!(
                    canvas.iter().filter(|row| row.contains(EDGE)).count() >= LEAST_ROWS / 2,
                    "{rows}x{columns} frame {frame}: the cube is drawn with its edges"
                );
            }
        }
    }

    #[test]
    fn the_cube_turns_from_one_frame_to_the_next() {
        assert_ne!(cube(0, 20, 80), cube(1, 20, 80));
    }

    #[test]
    fn a_canvas_too_small_for_the_cube_stays_blank() {
        for (rows, columns) in [(0, 80), (LEAST_ROWS + 1, 80), (20, 15)] {
            let canvas = cube(3, rows, columns);
            assert_eq!(canvas.len(), rows);
            assert!(canvas.iter().all(String::is_empty), "{rows}x{columns}");
        }
    }
}
