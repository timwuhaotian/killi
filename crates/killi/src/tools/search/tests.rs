use super::*;
use serde_json::json;

/// A small tree of files, removed when it goes out of scope.
fn tree() -> crate::test_temp::TempDir {
    let dir = crate::test_temp::TempDir::new("killi-search");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join("src/a.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
    std::fs::write(dir.join("notes.txt"), "alpha notes\n").unwrap();
    std::fs::write(dir.join(".git/config"), "alpha\n").unwrap();
    dir
}

#[test]
fn grep_finds_matches_with_line_numbers_and_skips_dot_git() {
    let dir = tree();
    let out = grep_blocking(&dir, &dir, &json!({"pattern": "alpha"}));
    assert_eq!(
        out.text,
        "notes.txt:1: alpha notes\nsrc/a.rs:1: fn alpha() {}"
    );
    let out = grep_blocking(
        &dir,
        &dir,
        &json!({"pattern": "ALPHA", "ignore_case": true, "glob": "*.rs"}),
    );
    assert_eq!(out.text, "src/a.rs:1: fn alpha() {}");
}

#[test]
fn grep_shows_context_and_other_output_modes() {
    let dir = tree();
    std::fs::write(
        dir.join("long.txt"),
        "a\nb\nneedle\nc\nd\ne\nf\ng\nneedle\nh\n",
    )
    .unwrap();
    let out = grep_blocking(
        &dir,
        &dir,
        &json!({"pattern": "needle", "context": 1, "glob": "long.txt"}),
    );
    assert_eq!(
        out.text,
        "long.txt-2- b\nlong.txt:3: needle\nlong.txt-4- c\n--\nlong.txt-8- g\nlong.txt:9: needle\nlong.txt-10- h"
    );
    let out = grep_blocking(&dir, &dir, &json!({"pattern": "alpha", "output": "files"}));
    assert_eq!(out.text, "notes.txt\nsrc/a.rs");
    let out = grep_blocking(&dir, &dir, &json!({"pattern": "needle", "output": "count"}));
    assert_eq!(out.text, "long.txt:2");
}

#[test]
fn streaming_grep_keeps_every_match_and_overlapping_context() {
    let dir = tree();
    let contents = b"before\nneedle one\nneedle two\nafter\nafter two\n";
    let mut reader = std::io::Cursor::new(contents.as_slice());
    let regex = Regex::new("needle").unwrap();
    let mut out = String::new();
    let mut matches = 0;
    stream_content(&mut reader, &regex, 0, 10, "x.txt", &mut out, &mut matches).unwrap();
    assert_eq!(matches, 2);
    assert_eq!(out, "x.txt:2: needle one\nx.txt:3: needle two\n");

    let mut reader = std::io::Cursor::new(contents.as_slice());
    let mut out = String::new();
    let mut matches = 0;
    stream_content(&mut reader, &regex, 2, 10, "x.txt", &mut out, &mut matches).unwrap();
    assert_eq!(
        out,
        "x.txt-1- before\nx.txt:2: needle one\nx.txt:3: needle two\nx.txt-4- after\nx.txt-5- after two\n"
    );
    let _ = dir;
}

#[test]
fn grep_matches_across_lines() {
    let dir = tree();
    let out = grep_blocking(
        &dir,
        &dir,
        &json!({"pattern": r"alpha\(\) \{\}\nfn beta", "multiline": true}),
    );
    assert_eq!(
        out.text,
        "src/a.rs:1: fn alpha() {}\nsrc/a.rs:2: fn beta() {}"
    );
    let out = grep_blocking(
        &dir,
        &dir,
        &json!({"pattern": "$", "multiline": true, "glob": "*.txt"}),
    );
    assert!(!out.is_error, "{}", out.text);
}

#[test]
fn grep_reports_traversal_errors_instead_of_no_matches() {
    let dir = tree();
    let missing = dir.join("missing");
    let out = grep_blocking(&dir, &missing, &json!({"pattern": "alpha"}));
    assert!(out.is_error, "{}", out.text);
    assert!(out.text.contains("No matches found."));
    assert!(
        out.text.contains("Search incomplete: 1 error"),
        "{}",
        out.text
    );
    assert!(out.text.contains("missing"), "{}", out.text);
    let out = glob_blocking(&dir, &missing, &json!({"pattern": "**/*.rs"}));
    assert!(out.is_error, "{}", out.text);
    assert!(
        out.text.contains("Search incomplete: 1 error"),
        "{}",
        out.text
    );
}

#[cfg(unix)]
#[test]
fn grep_marks_partial_results_when_a_file_cannot_be_read() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tree();
    let denied = dir.join("denied.txt");
    std::fs::write(&denied, "alpha\n").unwrap();
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o0)).unwrap();
    let out = grep_blocking(&dir, &dir, &json!({"pattern": "alpha"}));
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!out.is_error, "{}", out.text);
    assert!(out.text.contains("notes.txt:1: alpha notes"));
    assert!(
        out.text.contains("Search incomplete: 1 error"),
        "{}",
        out.text
    );
    assert!(out.text.contains("denied.txt"), "{}", out.text);
}

#[test]
fn streaming_grep_preserves_lossy_utf8_and_binary_detection() {
    let dir = tree();
    std::fs::write(dir.join("lossy.txt"), b"alpha \xff\n").unwrap();
    std::fs::write(dir.join("binary.txt"), b"alpha\0hidden\n").unwrap();
    let out = grep_blocking(&dir, &dir, &json!({"pattern": "alpha", "glob": "*.txt"}));
    assert!(out.text.contains("lossy.txt:1: alpha �"), "{}", out.text);
    assert!(!out.text.contains("binary.txt"));
}

#[test]
fn grep_names_files_it_skipped_for_size() {
    let dir = tree();
    use std::io::Write;
    let big = dir.join("big.log");
    let mut file = File::create(&big).unwrap();
    file.write_all(b"needle\n").unwrap();
    file.set_len(MAX_FILE_SIZE + 1).unwrap();
    let out = grep_blocking(&dir, &dir, &json!({"pattern": "needle"}));
    assert!(out.is_error, "{}", out.text);
    assert!(out.text.contains("No matches found."), "{}", out.text);
    assert!(
        out.text
            .contains("Skipped 1 file over 10 MB: big.log — search them with bash (`rg`/`grep`)."),
        "{}",
        out.text
    );
    let out = grep_blocking(&dir, &dir, &json!({"pattern": "alpha"}));
    assert!(!out.is_error, "{}", out.text);
    assert!(
        out.text.contains("notes.txt:1: alpha notes"),
        "{}",
        out.text
    );
    assert!(
        out.text.ends_with(
            "[Skipped 1 file over 10 MB: big.log — search them with bash (`rg`/`grep`).]"
        ),
        "{}",
        out.text
    );
}

#[test]
fn glob_matches_relative_paths() {
    let dir = tree();
    assert_eq!(
        glob_blocking(&dir, &dir, &json!({"pattern": "**/*.rs"})).text,
        "src/a.rs"
    );
    assert!(
        glob_blocking(&dir, &dir, &json!({"pattern": "*.rs"}))
            .text
            .starts_with("No files match")
    );
}
