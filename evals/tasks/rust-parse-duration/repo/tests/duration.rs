use std::time::Duration;

use duration::{ParseError, parse_duration};

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn single_units() {
    assert_eq!(parse_duration("90s"), Ok(secs(90)));
    assert_eq!(parse_duration("2d"), Ok(secs(2 * 86_400)));
    assert_eq!(parse_duration("250ms"), Ok(Duration::from_millis(250)));
    assert_eq!(parse_duration("0m"), Ok(secs(0)));
}

#[test]
fn combined_parts_add_up() {
    assert_eq!(parse_duration("1h30m"), Ok(secs(5_400)));
    assert_eq!(parse_duration("1h 15m 30s"), Ok(secs(4_530)));
    assert_eq!(parse_duration("  45s 1m "), Ok(secs(105)));
    assert_eq!(parse_duration("1m1ms"), Ok(Duration::from_millis(60_001)));
    assert_eq!(parse_duration("10s10s"), Ok(secs(20)));
}

#[test]
fn errors() {
    assert_eq!(parse_duration(""), Err(ParseError::Empty));
    assert_eq!(parse_duration("   "), Err(ParseError::Empty));
    assert_eq!(parse_duration("10"), Err(ParseError::MissingUnit));
    assert_eq!(parse_duration("5y"), Err(ParseError::UnknownUnit("y".into())));
    assert_eq!(parse_duration("3h 2x"), Err(ParseError::UnknownUnit("x".into())));
    assert_eq!(parse_duration("h"), Err(ParseError::InvalidNumber(String::new())));
    assert_eq!(
        parse_duration("99999999999999999999s"),
        Err(ParseError::InvalidNumber("99999999999999999999".into()))
    );
}
