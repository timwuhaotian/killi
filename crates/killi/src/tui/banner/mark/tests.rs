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
            let canvas = mark(seconds, None, rows, columns);
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
fn the_whole_mark_stays_inside_the_grid_as_it_turns_melts_and_is_pushed() {
    let mut moments = Vec::new();
    for tenth in 0..(2.0 * BREATH * 10.0) as usize {
        moments.push(pose(tenth as f32 / 10.0, None));
    }
    // Pushed on either side while it turns and while it rests.
    for since in [0.1, 0.15, 0.2, 0.4] {
        for side in [-1.0, 1.0] {
            for seconds in [0.3, BREATH * TURNING + 0.2] {
                moments.push(pose(seconds, Some(Click { since, side })));
            }
        }
    }
    for pose in moments {
        let dots = Dots::at(&pose, 64, 64);
        for i in 0..64 {
            for at in [i, 63 * 64 + i, i * 64, i * 64 + 63] {
                assert!(dots.dots[at].is_none(), "{pose:?}: touches the border");
            }
        }
    }
}

#[test]
fn each_turn_eases_rests_and_melts_between_the_k_and_the_cube() {
    assert_ne!(mark(1.0, None, 20, 80), mark(1.1, None, 20, 80), "turning");
    let resting = BREATH * TURNING + 0.1;
    for (a, b) in [
        (resting, BREATH - 0.05),
        (BREATH + resting, 2.0 * BREATH - 0.05),
    ] {
        let (at_a, at_b) = (pose(a, None), pose(b, None));
        assert_eq!((at_a.turn, at_a.melted), (at_b.turn, at_b.melted), "resting");
        // At rest it only breathes in, toward full.
        assert!(
            at_a.swell < at_b.swell && at_a.lit < at_b.lit && at_b.swell <= SWELL && at_b.lit <= 1.0,
            "{:?} then {:?}",
            at_a,
            at_b
        );
    }
    let melted = |seconds: f32| pose(seconds, None).melted;
    assert_eq!(melted(0.0), 0.0, "the K to start");
    assert_eq!(melted(BREATH + 0.1), 1.0, "the cube after one turn");
    assert_eq!(melted(2.0 * BREATH + 0.1), 0.0, "the K again after two");
    let mid = melted(BREATH * TURNING * 0.85);
    assert!(
        mid > 0.0 && mid < 1.0,
        "melting near the end of a turn: {mid}"
    );
    assert!(
        (pose(2.0 * BREATH, None).turn - 2.0 * TAU).abs() < 1e-4,
        "a loop is two whole turns"
    );
}

#[test]
fn the_k_shows_its_coral_dot_and_the_cube_none() {
    let shown = |seconds: f32| -> Vec<Dot> {
        Dots::at(&pose(seconds, None), 64, 64)
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
        if pose(seconds, None).melted == 1.0 {
            assert!(
                shown(seconds).iter().all(|d| !d.ball),
                "the cube has no dot at {seconds}s"
            );
        }
    }
    let studio = Studio::k();
    let brightness: Vec<f32> = k
        .iter()
        .map(|d| studio.shade(d.normal, d.z, 1.0).iter().sum())
        .collect();
    let (low, high) = brightness
        .iter()
        .fold((f32::MAX, 0.0f32), |(l, h), &b| (l.min(b), h.max(b)));
    assert!(high > 1.5 * low, "shaded and lit: {low}..{high}");
}

#[test]
fn a_canvas_too_small_for_the_mark_stays_blank() {
    for (rows, columns) in [(0, 80), (LEAST_ROWS + 1, 80), (20, 15)] {
        let canvas = mark(3.0, None, rows, columns);
        assert_eq!(canvas.len(), rows);
        assert!(canvas.iter().all(String::is_empty), "{rows}x{columns}");
    }
}

#[test]
fn the_mark_breathes_once_a_turn_full_and_bright_facing_the_viewer() {
    let at = |seconds: f32| pose(seconds, None);
    let (rest, mid) = (at(0.0), at(BREATH * 0.5));
    assert_eq!((rest.swell, rest.lit), (SWELL, 1.0));
    assert!(
        mid.swell.abs() < 1e-5 && (mid.lit - (1.0 - DIM)).abs() < 1e-5,
        "{:?}",
        mid
    );
    for tenth in 0..(BREATH * 10.0) as usize {
        let at = at(tenth as f32 / 10.0);
        assert!(at.swell.abs() < 1e-5 || (0.0..=SWELL).contains(&at.swell));
        assert!(at.lit >= 1.0 - DIM - 1e-5);
    }
    // The rows it takes stay the same however far it swells.
    assert_eq!(
        place(20, 80),
        Some(Place {
            row: 1,
            rows: MOST_ROWS,
            column: 22,
            columns: 2 * MOST_ROWS
        })
    );
    assert_eq!(place(LEAST_ROWS + 1, 80), None);
    assert_eq!(place(20, 2 * MOST_ROWS + 1), None);
}

#[test]
fn a_click_pushes_the_mark_and_the_push_eases_away() {
    assert_eq!(push(0.0), 0.0, "it lands at once, not before");
    assert!(
        push(0.15) > 0.6 && push(0.2) > push(0.4),
        "it peaks, then eases"
    );
    assert_eq!(push(PULSE), 0.0, "gone");
    assert_eq!(push(30.0), 0.0, "long past");

    let pushed = pose(
        0.0,
        Some(Click {
            since: 0.15,
            side: 1.0,
        }),
    );
    let (rest, back) = (
        pose(0.0, None),
        pose(
            0.0,
            Some(Click {
                since: 0.15,
                side: -1.0,
            }),
        ),
    );
    assert!(pushed.swell > rest.swell && pushed.lit > 1.0);
    assert!(
        pushed.turn > rest.turn && back.turn < rest.turn,
        "twists away from the click"
    );
    assert_eq!(back.swell, pushed.swell, "as far on either side");

    let idle = mark(3.0, None, 20, 80);
    assert_ne!(
        mark(
            3.0,
            Some(Click {
                since: 0.15,
                side: 1.0
            }),
            20,
            80
        ),
        idle,
        "the click shows"
    );
    assert_eq!(
        mark(
            3.0,
            Some(Click {
                since: PULSE,
                side: 1.0
            }),
            20,
            80
        ),
        idle,
        "and is gone with the push"
    );
    for rows in [20, 9] {
        assert_eq!(
            mark(
                3.0,
                Some(Click {
                    since: 0.15,
                    side: 1.0
                }),
                rows,
                80
            )
            .len(),
            rows
        );
    }
}
