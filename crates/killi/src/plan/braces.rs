//! Brace expansion, as bash does it before anything else: `a{b,c}d` is `abd acd`, and
//! `{a..c}` and `{1..9..2}` are sequences. A brace pair with neither stays as it is.
//!
//! This runs on every `bash` call, plan mode or not, so its cost is bounded whatever the
//! input: a worklist instead of recursion, a cap on the words, and a budget on the
//! characters built along the way. Past either, the segment is refused.

use super::words::{Char, Raw};

/// Most words one segment may expand to (`{1..9999}`); past it the segment is refused.
const MAX_WORDS: usize = 1024;

/// Characters the expansion may build beyond twice the segment's own length.
const MAX_WORK: usize = 1 << 20;

/// What one brace pair expands to.
enum Group {
    /// The top-level commas between the braces.
    Commas(Vec<usize>),
    Sequence(Vec<String>),
}

/// `words` with their braces expanded, or `None` when that makes too many words or too much
/// work, or a sequence bash would read differently from this.
pub(super) fn expand(words: Vec<Raw>) -> Option<Vec<Raw>> {
    let length: usize = words.iter().map(|w| w.chars.len() + 1).sum();
    let mut budget = MAX_WORK.saturating_add(length.saturating_mul(2));
    let mut out = Vec::new();
    for word in words {
        let mut pending = vec![word];
        while let Some(raw) = pending.pop() {
            budget = budget.checked_sub(raw.chars.len() + 1)?;
            let Some((open, close, group)) = first_group(&raw.chars)? else {
                if out.len() == MAX_WORDS {
                    return None;
                }
                out.push(raw);
                continue;
            };
            let (word, unknown) = (raw.chars, raw.unknown);
            let first = pending.len();
            let mut add = |middle: &[Char]| {
                let length = open + middle.len() + word.len() - close;
                budget = budget.checked_sub(length)?;
                let mut joined = Vec::with_capacity(length);
                joined.extend_from_slice(&word[..open]);
                joined.extend_from_slice(middle);
                joined.extend_from_slice(&word[close + 1..]);
                pending.push(Raw {
                    chars: joined,
                    unknown,
                });
                Some(())
            };
            match group {
                Group::Commas(commas) => {
                    let mut start = open + 1;
                    for end in commas.into_iter().chain([close]) {
                        add(&word[start..end])?;
                        start = end + 1;
                    }
                }
                Group::Sequence(items) => {
                    for item in items {
                        let item: Vec<Char> = item.chars().map(|c| (c, true)).collect();
                        add(&item)?;
                    }
                }
            }
            pending[first..].reverse();
        }
    }
    Some(out)
}

/// The first brace pair in `word` bash expands, as `(open, close, group)`; `Some(None)` when
/// there is none, `None` when a sequence has too many items to check. One pass pairs the
/// braces; only pairs with no brace inside are tried as sequences, so the work is linear.
fn first_group(word: &[Char]) -> Option<Option<(usize, usize, Group)>> {
    let mut opens = Vec::new();
    let mut close_of = vec![usize::MAX; word.len()];
    let mut comma = vec![false; word.len()];
    let mut nested = vec![false; word.len()];
    for (at, &c) in word.iter().enumerate() {
        match c {
            ('{', false) => {
                if let Some(&outer) = opens.last() {
                    nested[outer] = true;
                }
                opens.push(at);
            }
            ('}', false) => {
                if let Some(open) = opens.pop() {
                    close_of[open] = at;
                }
                if let Some(&outer) = opens.last() {
                    nested[outer] = true;
                }
            }
            (',', false) => {
                if let Some(&open) = opens.last() {
                    comma[open] = true;
                }
            }
            _ => {}
        }
    }
    for open in 0..word.len() {
        let close = close_of[open];
        if close == usize::MAX {
            continue;
        }
        if comma[open] {
            let mut depth = 0usize;
            let mut commas = Vec::new();
            for (at, &c) in word.iter().enumerate().take(close).skip(open + 1) {
                match c {
                    ('{', false) => depth += 1,
                    ('}', false) => depth = depth.saturating_sub(1),
                    (',', false) if depth == 0 => commas.push(at),
                    _ => {}
                }
            }
            return Some(Some((open, close, Group::Commas(commas))));
        }
        if !nested[open]
            && let Some(items) = sequence(&word[open + 1..close])
        {
            return Some(Some((open, close, Group::Sequence(items?))));
        }
    }
    Some(None)
}

/// The items of a `{x..y}` or `{x..y..step}` sequence body, if `body` is one: `None` when it
/// isn't, `Some(None)` when it is but has too many items to check or numbers past what bash
/// reads.
fn sequence(body: &[Char]) -> Option<Option<Vec<String>>> {
    if body.iter().any(|&(_, quoted)| quoted) {
        return None;
    }
    let body: String = body.iter().map(|&(c, _)| c).collect();
    let parts: Vec<&str> = body.split("..").collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let number = |s: &str| {
        let digits = s.strip_prefix(['-', '+']).unwrap_or(s);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
    };
    let in_range = |s: &str| s.parse::<i64>().ok().map(i128::from);
    let step = match parts.get(2) {
        Some(step) if !number(step) => return None,
        Some(step) => match in_range(step) {
            Some(step) => step.abs().max(1),
            None => return Some(None),
        },
        None => 1,
    };
    let (from, to, letters) = match (number(parts[0]), number(parts[1])) {
        (true, true) => match (in_range(parts[0]), in_range(parts[1])) {
            (Some(from), Some(to)) => (from, to, false),
            _ => return Some(None),
        },
        _ => {
            let single = |s: &str| {
                let mut chars = s.chars();
                chars
                    .next()
                    .filter(|c| c.is_ascii_alphabetic() && chars.next().is_none())
            };
            (
                i128::from(single(parts[0])? as u8),
                i128::from(single(parts[1])? as u8),
                true,
            )
        }
    };
    let count = (to - from).abs() / step + 1;
    if count > MAX_WORDS as i128 {
        return Some(None);
    }
    let direction = if to >= from { step } else { -step };
    let items = (0..count)
        .map(|k| from + k * direction)
        .map(|n| match letters {
            true => char::from(n as u8).to_string(),
            false => n.to_string(),
        })
        .collect();
    Some(Some(items))
}
