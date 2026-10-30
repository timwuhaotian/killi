use super::*;

#[test]
fn wrap_words_breaks_at_spaces_and_keeps_styles() {
    assert_eq!(
        wrap_words("hello brave new world", 11, 0),
        ["hello brave", "new world"]
    );
    assert_eq!(
        wrap_words("• one two three four", 11, 2),
        ["• one two", "  three", "  four"]
    );
    assert_eq!(
        wrap_words("  indented words here", 12, 2),
        ["  indented", "  words here"]
    );
    let rows = wrap_words(&format!("a {BOLD}bold words{RESET} end"), 9, 0);
    assert_eq!(
        rows,
        [
            format!("a {BOLD}bold{RESET}"),
            format!("{BOLD}words{RESET} end")
        ]
    );
    assert_eq!(wrap_words("abcdefghij", 4, 0), ["abcd", "efgh", "ij"]);
}

#[test]
fn hanging_indent_follows_the_list_marker() {
    assert_eq!(hanging_indent("plain"), 0);
    assert_eq!(hanging_indent(&format!("  • {BOLD}item{RESET}")), 4);
    assert_eq!(hanging_indent("12. step"), 4);
}

#[test]
fn width_skips_escapes_and_counts_wide_characters() {
    assert_eq!(width(&format!("{BOLD}ab{RESET}")), 2);
    assert_eq!(width("中文"), 4);
}

#[test]
fn sanitize_takes_out_escape_sequences_and_controls() {
    assert_eq!(&*sanitize("plain, 中文 and ✓"), "plain, 中文 and ✓");
    assert_eq!(&*sanitize("a\x1b[31mred\x1b[0mb"), "aredb");
    assert_eq!(&*sanitize("a\x1b[2Jbc"), "abc");
    assert_eq!(&*sanitize("\x1b[3;7Hx"), "x");
    assert_eq!(&*sanitize("\x1b]0;title\x07rest"), "rest");
    assert_eq!(
        &*sanitize("\x1b]8;;http://a\x1b\\link\x1b]8;;\x1b\\!"),
        "link!"
    );
    assert_eq!(&*sanitize("a\x1bPq;1;2\x1b\\b"), "ab");
    assert_eq!(&*sanitize("a\x1bMb"), "ab");
    assert_eq!(&*sanitize("a\x1b"), "a", "a lone escape goes");
    assert_eq!(
        &*sanitize("a\x1b[31"),
        "a",
        "so does one the text ends in the middle of"
    );
    assert_eq!(
        &*sanitize("x\u{9b}31my"),
        "x31my",
        "a C1 control is no safer"
    );
    assert_eq!(
        &*sanitize("a\x07b\rc\x7fd"),
        "abcd",
        "other C0 controls go, line breaks stay"
    );
    assert_eq!(&*sanitize("a\nb"), "a\nb");
    // A tab that would eat the line break neither eats it nor the text after it.
    assert_eq!(&*sanitize("a\x1b[\nb"), "a\nb");
    assert_eq!(&*sanitize("\x1b]0;still open\nb"), "\nb");
}

#[test]
fn sanitize_expands_tabs_to_the_next_four_column_stop() {
    assert_eq!(&*sanitize("a\tb"), "a   b");
    assert_eq!(&*sanitize("ab\tc"), "ab  c");
    assert_eq!(&*sanitize("abc\td"), "abc d");
    assert_eq!(&*sanitize("\tx"), "    x");
    assert_eq!(
        &*sanitize("a\n\tb"),
        "a\n    b",
        "the column starts over on each line"
    );
    assert_eq!(&*sanitize("中文\t."), "中文    .");
    assert_eq!(&*sanitize_row("one\ntwo"), "one ⏎ two");
    assert_eq!(&*sanitize_row("a\tb"), "a   b");
    assert_eq!(&*sanitize_row("plain"), "plain");
}

#[test]
fn a_sequence_split_across_deltas_is_still_taken_out() {
    let mut stream = Sanitizer::default();
    assert_eq!(stream.push("ok \x1b["), "ok ");
    assert_eq!(stream.push("31;41mred"), "red");
    assert_eq!(stream.push("\x1b]0;ti"), "");
    assert_eq!(stream.push("tle\x07!"), "!");
    assert_eq!(stream.push("ab"), "ab");
    assert_eq!(
        stream.push("\tc"),
        "   c",
        "a tab counts every column before it, delta or not"
    );
    assert_eq!(stream.push("d\x1b"), "d");
    assert_eq!(stream.push("[2K"), "");
    assert_eq!(stream.finish(), "");
    let mut ending = Sanitizer::default();
    assert_eq!(ending.push("end\x1b[12"), "end");
    assert_eq!(
        ending.finish(),
        "",
        "a sequence still waiting at the end goes"
    );
}

#[test]
fn wrapped_code_rows_keep_the_gutter() {
    let rows = wrap_code(
        &format!("  {FAINT}▏{RESET} {}", "let x = 1;".repeat(20)),
        30,
    );
    assert!(rows.len() > 1);
    assert!(rows.iter().all(|row| width(row) <= 30), "{rows:?}");
    assert!(rows.iter().all(|row| row.contains('▏')), "{rows:?}");
}

#[test]
fn code_marks_a_part_that_wraps_with_its_gutter() {
    let mut md = Markdown::new(30);
    md.line("```rust");
    let parts = md.parts(&"let x = 1;".repeat(20));
    assert!(matches!(parts.as_slice(), [Part::Code(_)]));
    let rows = md.rows(parts);
    assert!(
        rows.len() > 1 && rows.iter().all(|row| width(row) <= 30 && row.contains('▏')),
        "{rows:?}"
    );
}

#[test]
fn the_256_color_light_shades_read_at_four_point_five_to_one() {
    for sgr in [LAGOON, WARN] {
        let index: u8 = sgr.light["\x1b[38;5;".len()..sgr.light.len() - 1]
            .parse()
            .unwrap();
        let ratio = contrast_256(index, (245, 247, 244));
        assert!(ratio >= 4.5, "{index}: {ratio}");
    }
}

/// The RGB of a 256-color palette index.
fn palette_256(index: u8) -> (u8, u8, u8) {
    match index {
        16..=231 => {
            let n = index - 16;
            let level = |k: u8| [0, 95, 135, 175, 215, 255][usize::from(k)];
            (level(n / 36), level((n % 36) / 6), level(n % 6))
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            (v, v, v)
        }
        _ => (0, 0, 0),
    }
}

/// The WCAG contrast ratio of a palette index on a background color.
fn contrast_256(index: u8, background: (u8, u8, u8)) -> f64 {
    let luminance = |(r, g, b): (u8, u8, u8)| {
        let linear = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    };
    let (a, b) = (luminance(palette_256(index)), luminance(background));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[test]
fn wrap_carries_styles_across_rows() {
    let rows = wrap(&format!("{DANGER}abcdef{RESET}"), 4);
    assert_eq!(rows.len(), 2);
    assert_eq!(width(&rows[0]), 4);
    assert!(rows[1].starts_with(&DANGER.to_string()));
    assert_eq!(wrap("中文中文", 5).len(), 2);
}

fn lines(md: &mut Markdown, text: &str) -> Vec<String> {
    let mut rows: Vec<String> = text.lines().flat_map(|l| md.line(l)).collect();
    rows.extend(md.finish());
    rows
}

#[test]
fn markdown_styles_lines() {
    let mut md = Markdown::default();
    assert_eq!(md.line("## Title"), [format!("{BOLD}{LAGOON}Title{RESET}")]);
    assert_eq!(md.line("### Small"), [format!("{BOLD}Small{RESET}")]);
    assert_eq!(
        md.line("- use `cargo` **now**"),
        [format!("• use {} {BOLD}now{RESET}", code("cargo"))]
    );
    assert_eq!(md.line("  - nested"), ["  ◦ nested"]);
    assert_eq!(md.line("- [x] done"), ["☑ done"]);
    assert_eq!(md.line("2. second"), [format!("{DIM}2.{RESET} second")]);
    assert!(
        md.line("```").is_empty(),
        "a fence with no language has nothing to say"
    );
    assert_eq!(
        md.line("let x = 1;"),
        [format!("  {FAINT}▏{RESET} let x = 1;")]
    );
    assert!(
        md.line("```").is_empty(),
        "nor does the one that closes the block"
    );
    assert_eq!(md.line("a * b"), ["a * b"]);
    assert_eq!(
        md.line("some *emphasis* here"),
        [format!("some {ITALIC}emphasis{RESET} here")]
    );
    assert_eq!(md.line("~~gone~~"), [format!("{STRIKE}gone{RESET}")]);
}

#[test]
fn markdown_links_show_their_address() {
    let mut md = Markdown::default();
    assert_eq!(
        md.line("see [docs](https://a.b/c)"),
        [format!(
            "see {LAGOON}{UNDERLINE}docs{RESET} {DIM}(https://a.b/c){RESET}"
        )]
    );
    assert_eq!(
        md.line("[https://a.b](https://a.b)"),
        [format!("{LAGOON}{UNDERLINE}https://a.b{RESET}")]
    );
    assert_eq!(md.line("[not a link] (x)"), ["[not a link] (x)"]);
}

#[test]
fn code_blocks_are_colored_by_language() {
    let mut md = Markdown::default();
    let rows = lines(&mut md, "```rust\nlet n = 42; // note\n```");
    assert_eq!(rows[0], format!("  {FAINT}▏{RESET} {DIM}rust{RESET}"));
    assert!(rows[1].contains(&format!("{KEYWORD}let{RESET}")));
    assert!(rows[1].contains(&format!("{NUMBER}42{RESET}")));
    assert!(rows[1].contains(&format!("{DIM}// note{RESET}")));
    let rows = lines(&mut md, "```diff\n+new\n-old\n```");
    assert!(
        rows[1].contains(&format!("{LAGOON}+new")) && rows[2].contains(&format!("{DANGER}-old"))
    );
}

#[test]
fn tables_line_up_and_shrink_to_the_width() {
    let mut md = Markdown::new(40);
    assert!(
        md.line("| a | bb |").is_empty(),
        "rows wait for the end of the table"
    );
    md.line("|---|---:|");
    md.line("| long cell | 1 |");
    let rows = md.line("after");
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[3], "after");
    assert!(rows[..3].iter().all(|r| width(r) == width(&rows[0])));
    assert!(rows[1].contains('┼'));
    let mut narrow = Markdown::new(14);
    narrow.line("| aaaaaaaaaa | bbbbbbbbbb |");
    let rows = narrow.finish();
    assert!(rows.iter().all(|r| width(r) <= 14), "{rows:?}");
}

#[test]
fn an_unclosed_fence_does_not_leak_into_the_next_message() {
    let mut md = Markdown::default();
    assert!(md.line("```rust").iter().any(|row| row.contains("rust")));
    assert!(md.line("let x = 1;").iter().all(|row| row.contains('▏')));
    let held = md.end_message();
    assert!(held.is_empty(), "no table was held");
    assert_eq!(md.line("plain"), ["plain"]);
    let reopened = md.line("```rust");
    assert!(
        reopened.iter().any(|row| row.contains("rust")),
        "a fence still opens after the reset: {reopened:?}"
    );
    assert!(md.line("let y = 2;").iter().all(|row| row.contains('▏')));
}

#[test]
fn a_background_picks_the_theme_by_its_brightness() {
    assert!(matches!(theme_for((11, 15, 20)), Theme::Dark));
    assert!(matches!(theme_for((245, 247, 244)), Theme::Light));
    assert!(matches!(theme_for((0, 43, 54)), Theme::Dark));
    assert!(matches!(theme_for((253, 246, 227)), Theme::Light));
}

#[test]
fn roles_print_their_tokens_exactly_on_truecolor_terminals() {
    assert_eq!(LAGOON.shade(Theme::Dark, true), "\x1b[38;2;79;209;197m");
    assert_eq!(LAGOON.shade(Theme::Dark, false), "\x1b[38;5;80m");
    assert_eq!(TINT.shade(Theme::Light, true), "\x1b[48;2;223;229;226m");
    assert_eq!(TINT.shade(Theme::Light, false), "\x1b[48;5;254m");
    assert_eq!(EFFORT.shade(Theme::Dark, true), "\x1b[38;2;170;169;235m");
    assert_eq!(TOKENS.shade(Theme::Dark, true), "\x1b[38;2;129;188;229m");
    assert_eq!(BRANCH.shade(Theme::Dark, true), "\x1b[38;2;215;160;202m");
    for role in [
        LAGOON, FIN, WARN, DANGER, FAINT, RULE, EFFORT, TOKENS, BRANCH,
    ] {
        assert!(
            role.dark_true.starts_with("\x1b[38;2;") && role.light_true.starts_with("\x1b[38;2;")
        );
        assert!(role.dark.starts_with("\x1b[38;5;") && role.light.starts_with("\x1b[38;5;"));
    }
    for ground in [TINT, SELECT, ADD, DEL, CHIP] {
        assert!(
            ground.dark_true.starts_with("\x1b[48;2;")
                && ground.light_true.starts_with("\x1b[48;2;")
        );
    }
    for ground in [ADD, DEL, CHIP] {
        assert_eq!(
            (ground.dark, ground.light),
            ("\x1b[49m", "\x1b[49m"),
            "no stripe or chip without 24-bit color"
        );
    }
    assert_eq!(
        (DIM.dark, DIM.light_true),
        ("\x1b[2m", "\x1b[2m"),
        "without a ground, dim is SGR dim"
    );
}

#[test]
fn the_fixed_grounds_are_what_the_palette_derives_for_the_design_systems_own_ground() {
    let digits = |shade: &str| -> Vec<u8> {
        shade
            .trim_start_matches("\x1b[48;2;")
            .trim_end_matches('m')
            .split(';')
            .map(|n| n.parse().unwrap())
            .collect()
    };
    let dark = palette::derive(Ground {
        background: (11, 15, 20),
        foreground: Some((232, 238, 242)),
    });
    let light = palette::derive(Ground {
        background: (245, 247, 244),
        foreground: Some((19, 26, 32)),
    });
    for (role, sgr) in [
        (Role::Tint, TINT),
        (Role::Select, SELECT),
        (Role::Add, ADD),
        (Role::Del, DEL),
        (Role::Chip, CHIP),
    ] {
        for (derived, shade) in [
            (dark.rgb(role), sgr.dark_true),
            (light.rgb(role), sgr.light_true),
        ] {
            let fixed = digits(shade);
            assert!(
                [derived.0, derived.1, derived.2]
                    .iter()
                    .zip(&fixed)
                    .all(|(a, b)| a.abs_diff(*b) <= 1),
                "{role:?}: {derived:?} against {fixed:?}"
            );
        }
    }
}

#[test]
fn an_ellipsis_ends_a_row_that_runs_out_in_the_style_it_was_in() {
    assert_eq!(ellipsize("short", 10), "short");
    assert_eq!(ellipsize("exactly ten", 11), "exactly ten");
    assert_eq!(ellipsize("one two three", 8), "one two…");
    assert_eq!(width(&ellipsize("one two three", 8)), 8);
    assert_eq!(ellipsize("anything", 0), "");
    assert_eq!(
        ellipsize("中文中文", 5),
        "中文…",
        "a wide character is never split"
    );
    let tinted = ellipsize(&format!("{TINT}{BOLD}a long selected row{RESET}"), 10);
    assert_eq!(
        tinted,
        format!("{TINT}{BOLD}a long se…{RESET}"),
        "the ellipsis keeps the band"
    );
    assert_eq!(width(&tinted), 10);
    let closed = ellipsize(&format!("{BOLD}bold{RESET} and plain text"), 12);
    assert_eq!(
        closed,
        format!("{BOLD}bold{RESET} and pl…"),
        "after a reset the ellipsis is plain"
    );
}

#[test]
fn inline_code_is_a_chip_in_24_bit_color_and_lagoon_elsewhere() {
    assert!(inline("a `b` c").contains(&code("b")));
    let expected = if truecolor() {
        format!("{CHIP}b{RESET}")
    } else {
        format!("{LAGOON}b{RESET}")
    };
    assert_eq!(code("b"), expected);
}

#[test]
fn a_code_block_starts_and_ends_on_its_code() {
    let mut md = Markdown::default();
    assert_eq!(lines(&mut md, "```\nx\n```").len(), 1);
    assert_eq!(
        lines(&mut md, "```rust\nlet a = 1;\nlet b = 2;\n```").len(),
        3,
        "the label and two lines"
    );
    assert_eq!(lines(&mut md, "after").len(), 1);
}

#[test]
fn styling_steps_bump_the_generation() {
    let mut md = Markdown::new(80);
    assert_eq!(md.generation(), 0);
    md.line("```rust");
    let styled = md.generation();
    assert!(styled > 0);
    md.line("let x = 1;");
    assert!(md.generation() > styled);
    md.finish();
    assert!(md.generation() > styled);
}

#[test]
fn no_color_drops_colors_and_keeps_styles() {
    let line = format!("{BOLD}{LAGOON}x{RESET}\x1b[38;2;1;2;3m\x1b[48;5;9my\x1b[1;32mz{DIM}");
    assert_eq!(strip_colors(&line), format!("{BOLD}x{RESET}y\x1b[1mz{DIM}"));
    assert_eq!(strip_colors("plain"), "plain");
}
