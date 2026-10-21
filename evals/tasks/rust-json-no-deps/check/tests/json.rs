use report::Report;

fn report(failures: Vec<&str>, note: Option<&str>) -> Report {
    Report {
        suite: "unit".into(),
        passed: 41,
        failed: failures.len() as u32,
        duration_ms: 1234,
        flaky: false,
        failures: failures.into_iter().map(String::from).collect(),
        note: note.map(String::from),
    }
}

#[test]
fn plain_report() {
    assert_eq!(
        report(vec![], None).to_json(),
        r#"{"suite":"unit","passed":41,"failed":0,"duration_ms":1234,"flaky":false,"failures":[],"note":null}"#
    );
}

#[test]
fn escapes_strings() {
    let json = report(vec!["say \"hi\"", "path\\to\\test", "line1\nline2\ttab"], Some("ünïcödé ✓ 中文")).to_json();
    assert_eq!(
        json,
        r#"{"suite":"unit","passed":41,"failed":3,"duration_ms":1234,"flaky":false,"failures":["say \"hi\"","path\\to\\test","line1\nline2\ttab"],"note":"ünïcödé ✓ 中文"}"#
    );
}

#[test]
fn escapes_control_characters() {
    let json = report(vec!["bell\u{7}", "nul\u{0}", "esc\u{1b}[0m", "cr\r", "del\u{7f}"], None).to_json();
    assert!(json.contains(r#""bell\u0007""#), "{json}");
    assert!(json.contains(r#""nul\u0000""#), "{json}");
    assert!(json.contains(r#""esc\u001b[0m""#), "{json}");
    assert!(json.contains(r#""cr\r""#), "{json}");
    assert!(!json.chars().any(|c| (c as u32) < 0x20), "raw control characters in {json:?}");
}
