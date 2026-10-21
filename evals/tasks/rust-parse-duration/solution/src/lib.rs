use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The input is empty or only whitespace.
    Empty,
    /// A number is missing, or too large for a u64. Holds the digits that were read.
    InvalidNumber(String),
    /// A number has no unit after it.
    MissingUnit,
    /// A unit isn't one of d, h, m, s, ms. Holds the unit as written.
    UnknownUnit(String),
}

/// Parses a duration such as "90s", "1h30m", "2d" or "1h 15m 30s".
///
/// The input is one or more parts. Each part is an unsigned integer followed directly by a
/// unit: `d` (days), `h` (hours), `m` (minutes), `s` (seconds) or `ms` (milliseconds).
/// Whitespace may separate parts. Parts can come in any order and may repeat; they add up.
pub fn parse_duration(input: &str) -> Result<Duration, ParseError> {
    let mut rest = input.trim_start();
    if rest.is_empty() {
        return Err(ParseError::Empty);
    }
    let mut total = Duration::ZERO;
    while !rest.is_empty() {
        let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let digits = &rest[..digits_end];
        let value: u64 = digits.parse().map_err(|_| ParseError::InvalidNumber(digits.to_string()))?;
        rest = &rest[digits_end..];
        let unit_end = rest.find(|c: char| c.is_ascii_digit() || c.is_whitespace()).unwrap_or(rest.len());
        let unit = &rest[..unit_end];
        let part = match unit {
            "" => return Err(ParseError::MissingUnit),
            "d" => Duration::from_secs(value * 86_400),
            "h" => Duration::from_secs(value * 3_600),
            "m" => Duration::from_secs(value * 60),
            "s" => Duration::from_secs(value),
            "ms" => Duration::from_millis(value),
            other => return Err(ParseError::UnknownUnit(other.to_string())),
        };
        total += part;
        rest = rest[unit_end..].trim_start();
    }
    Ok(total)
}
