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
    let _ = input;
    todo!()
}
