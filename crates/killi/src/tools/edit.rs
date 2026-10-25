use std::fs;

use serde_json::Value;

use super::files::TextFile;
use super::{Context, Output, is_binary, resolve};

/// Changes shown in the terminal's diff; a `replace_all` over thousands of matches would
/// otherwise build a diff of megabytes that nobody scrolls.
const MAX_HUNKS: usize = 100;

struct Edit {
    old: String,
    new: String,
    all: bool,
}

pub fn edit(ctx: &Context, args: &Value) -> Output {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return Output::error("edit needs `path` and `edits`.");
    };
    let edits = match parse_edits(args) {
        Ok(edits) => edits,
        Err(message) => return Output::error(message),
    };
    let full = resolve(ctx, path);
    let bytes = match fs::read(&full) {
        Ok(bytes) => bytes,
        Err(e) => return Output::error(format!("Could not read {path}: {e}. To create a file, use write.")),
    };
    if is_binary(&bytes) {
        return Output::error(format!("{path} is a binary file; edit only works on text."));
    }
    let file = TextFile::decode(&bytes);
    match apply(&file.text, &edits, path) {
        Ok(applied) => match fs::write(&full, file.encode(&applied.text)) {
            Ok(()) => {
                let mut output = Output::ok(format!("Applied {} replacement(s) to {path}.", applied.count))
                    .brief(format!("+{} −{}", applied.added, applied.removed));
                output.display = Some(applied.diff);
                output
            }
            Err(e) => Output::error(format!("Could not write {path}: {e}")),
        },
        Err(message) => Output::error(message),
    }
}

/// Accepts the declared `edits` list, a list sent as a JSON string, or a single top-level
/// `old_string`/`new_string` pair, since models trained on other harnesses send those.
fn parse_edits(args: &Value) -> Result<Vec<Edit>, String> {
    let one = |v: &Value| -> Option<Edit> {
        Some(Edit {
            old: v.get("old_string")?.as_str()?.to_string(),
            new: v.get("new_string")?.as_str()?.to_string(),
            all: v.get("replace_all").and_then(Value::as_bool).unwrap_or(false),
        })
    };
    let Some(list) = args.get("edits") else {
        return one(args).map(|e| vec![e]).ok_or_else(|| "edit needs `edits`: a list of {old_string, new_string}.".into());
    };
    let list = match list {
        Value::String(s) => serde_json::from_str(s).map_err(|e| format!("`edits` is not valid JSON: {e}"))?,
        other => other.clone(),
    };
    let Some(items) = list.as_array().filter(|items| !items.is_empty()) else {
        return Err("`edits` must be a non-empty list of {old_string, new_string}.".into());
    };
    items
        .iter()
        .enumerate()
        .map(|(i, item)| one(item).ok_or_else(|| format!("edits[{i}] needs string `old_string` and `new_string`.")))
        .collect()
}

struct Applied {
    text: String,
    count: usize,
    added: usize,
    removed: usize,
    /// The changes for the terminal; see `line_diff`.
    diff: String,
}

/// Matches every edit against the original text: exactly first, then ignoring trailing
/// whitespace, smart quotes, Unicode dashes and special spaces.
fn apply(text: &str, edits: &[Edit], path: &str) -> Result<Applied, String> {
    let label = |i: usize| if edits.len() == 1 { "old_string".to_string() } else { format!("edits[{i}].old_string") };
    let mut normalized: Option<Normalized> = None;
    let mut ranges: Vec<(usize, usize, usize)> = Vec::new();
    for (i, edit) in edits.iter().enumerate() {
        let old = edit.old.replace("\r\n", "\n");
        if old.is_empty() {
            return Err(format!("{} must not be empty in {path}.", label(i)));
        }
        let mut found: Vec<(usize, usize)> = text.match_indices(&old).map(|(s, m)| (s, s + m.len())).collect();
        if found.is_empty() {
            let haystack = normalized.get_or_insert_with(|| Normalized::new(text));
            let needle = Normalized::new(&old).text;
            if !needle.is_empty() {
                found = haystack
                    .text
                    .match_indices(&needle)
                    .map(|(s, m)| (haystack.map[s], haystack.map[s + m.len()]))
                    .collect();
            }
        }
        match found.len() {
            0 => {
                let hint = nearest_line(text, &old).map(|line| format!(" The closest line is line {line}; read from there.")).unwrap_or_default();
                return Err(format!(
                    "Could not find {} in {path}. It must match the file exactly, including indentation and line breaks; read the file again if unsure.{hint}",
                    label(i)
                ));
            }
            n if n > 1 && !edit.all => {
                return Err(format!(
                    "Found {n} matches for {} in {path}. Include more surrounding lines to make it unique, or set replace_all.",
                    label(i)
                ));
            }
            _ => ranges.extend(found.into_iter().map(|(s, e)| (s, e, i))),
        }
    }
    ranges.sort_unstable();
    for pair in ranges.windows(2) {
        if pair[1].0 < pair[0].1 {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit.",
                pair[0].2, pair[1].2
            ));
        }
    }
    // One pass over the sorted ranges: the new text, the line each range starts on, and its hunk.
    let news: Vec<String> = edits.iter().map(|edit| edit.new.replace("\r\n", "\n")).collect();
    let mut out = String::with_capacity(text.len());
    let mut diff = String::new();
    let (mut added, mut removed) = (0, 0);
    let (mut copied, mut line, mut hunks) = (0, 1, 0);
    for &(start, end, i) in &ranges {
        line += text[copied..start].matches('\n').count();
        out.push_str(&text[copied..start]);
        out.push_str(&news[i]);
        copied = end;
        let shift = added as isize - removed as isize;
        let (hunk, plus, minus) = line_diff(&text[start..end], &news[i], line, line.saturating_add_signed(shift));
        if hunks < MAX_HUNKS {
            if hunks > 0 {
                diff.push_str("⋮\n");
            }
            diff.push_str(&hunk);
            hunks += 1;
        }
        added += plus;
        removed += minus;
    }
    out.push_str(&text[copied..]);
    if ranges.len() > hunks {
        diff.push_str(&format!("⋮\n{} more changes not shown\n", ranges.len() - hunks));
    }
    if out == text {
        return Err(format!("No changes made to {path}: the replacement produced identical content."));
    }
    Ok(Applied { text: out, count: ranges.len(), added, removed, diff })
}

/// The lines that changed between `old` and `new`, and how many were added and removed. Each
/// row is `-`, `+` or a space, then the line number (from `old_line` or `new_line`), a tab
/// and the text; unchanged lines more than two away from a change fold into one `⋮`.
fn line_diff(old: &str, new: &str, old_line: usize, new_line: usize) -> (String, usize, usize) {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let mut ops: Vec<(char, &str)> = Vec::new();
    if a.len() * b.len() > 250_000 {
        ops.extend(a.iter().map(|l| ('-', *l)).chain(b.iter().map(|l| ('+', *l))));
    } else {
        // lcs[i][j]: the longest common subsequence of a[i..] and b[j..].
        let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                ops.push((' ', a[i]));
                (i, j) = (i + 1, j + 1);
            } else if j == b.len() || i < a.len() && lcs[i + 1][j] >= lcs[i][j + 1] {
                ops.push(('-', a[i]));
                i += 1;
            } else {
                ops.push(('+', b[j]));
                j += 1;
            }
        }
    }
    let near = |k: usize| ops[k.saturating_sub(2)..(k + 3).min(ops.len())].iter().any(|(sign, _)| *sign != ' ');
    let (mut out, mut folded) = (String::new(), false);
    let (mut old_no, mut new_no) = (old_line, new_line);
    for (k, (sign, text)) in ops.iter().enumerate() {
        let number = if *sign == '-' { old_no } else { new_no };
        if near(k) {
            out.push_str(&format!("{sign}{number}\t{text}\n"));
            folded = false;
        } else if !folded {
            out.push_str("⋮\n");
            folded = true;
        }
        match sign {
            '-' => old_no += 1,
            '+' => new_no += 1,
            _ => (old_no, new_no) = (old_no + 1, new_no + 1),
        }
    }
    let count = |c: char| ops.iter().filter(|(sign, _)| *sign == c).count();
    (out, count('+'), count('-'))
}

/// The 1-based line of `text` closest to `old`'s first non-empty line, trimmed on both sides,
/// for pointing a failed edit at where the model most likely meant. `None` when nothing is
/// within half the needle's length, so an unrelated file gets no hint.
fn nearest_line(text: &str, old: &str) -> Option<usize> {
    let needle = old.lines().map(str::trim).find(|line| !line.is_empty())?;
    let limit = (needle.chars().count() / 2).max(1);
    text.lines()
        .enumerate()
        .map(|(i, line)| (i + 1, edit_distance(needle, line.trim())))
        .min_by_key(|(_, distance)| *distance)
        .filter(|(_, distance)| *distance <= limit)
        .map(|(line, _)| line)
}

/// Levenshtein distance between two strings, over characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if ca == *cb { diagonal } else { 1 + diagonal.min(row[j]).min(above) };
            diagonal = above;
        }
    }
    row[b.len()]
}

/// Text with trailing whitespace removed from each line and look-alike characters folded,
/// plus a map from each byte of it back to a byte offset in the original.
struct Normalized {
    text: String,
    map: Vec<usize>,
}

impl Normalized {
    fn new(src: &str) -> Self {
        let mut text = String::with_capacity(src.len());
        let mut map = Vec::with_capacity(src.len() + 1);
        let mut pos = 0;
        for line in src.split_inclusive('\n') {
            let body = line.strip_suffix('\n').unwrap_or(line);
            for (offset, ch) in body.trim_end_matches(char::is_whitespace).char_indices() {
                text.push(fold(ch));
                map.resize(text.len(), pos + offset);
            }
            if body.len() < line.len() {
                text.push('\n');
                map.push(pos + body.len());
            }
            pos += line.len();
        }
        map.push(src.len());
        Self { text, map }
    }
}

fn fold(ch: char) -> char {
    match ch {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(text: &str, args: Value) -> Result<String, String> {
        apply(text, &parse_edits(&args)?, "f").map(|applied| applied.text)
    }

    #[test]
    fn the_diff_shows_only_changed_lines_with_context_and_numbers() {
        let old = "a\nb\nc\nd\ne\nf\ng\n";
        let new = "a\nb\nc\nD\ne\nf\ng\nh\n";
        let (diff, added, removed) = line_diff(old, new, 10, 10);
        assert_eq!((added, removed), (2, 1));
        assert_eq!(diff, "⋮\n 11\tb\n 12\tc\n-13\td\n+13\tD\n 14\te\n 15\tf\n 16\tg\n+17\th\n");
    }

    #[test]
    fn exact_multi_edit_against_the_original() {
        let out = run("a b c", json!({"edits": [
            {"old_string": "a", "new_string": "b"},
            {"old_string": "b", "new_string": "x"}
        ]}));
        assert_eq!(out.unwrap(), "b x c");
    }

    #[test]
    fn ambiguous_and_missing_matches_are_errors() {
        assert!(run("x x", json!({"old_string": "x", "new_string": "y"})).unwrap_err().contains("Found 2 matches"));
        assert_eq!(run("x x", json!({"old_string": "x", "new_string": "y", "replace_all": true})).unwrap(), "y y");
        assert!(run("abc", json!({"old_string": "zzz", "new_string": "y"})).unwrap_err().contains("Could not find"));
    }

    #[test]
    fn a_missing_match_points_at_the_closest_line() {
        let text = "fn main() {\n    let credits_balance = 0;\n}\n";
        let err = run(text, json!({"old_string": "\tlet credits_balanc = 0;", "new_string": "x"})).unwrap_err();
        assert!(err.contains("closest line is line 2"), "{err}");
        let far = run(text, json!({"old_string": "something unrelated entirely", "new_string": "x"})).unwrap_err();
        assert!(!far.contains("closest line"), "{far}");
    }

    #[test]
    fn each_change_in_the_diff_carries_its_own_line_number() {
        let edits = [Edit { old: "foo".into(), new: "bar".into(), all: true }];
        let applied = apply("a\nfoo\nb\nfoo\n", &edits, "f").unwrap();
        assert_eq!(applied.text, "a\nbar\nb\nbar\n");
        assert_eq!(applied.diff, "-2\tfoo\n+2\tbar\n⋮\n-4\tfoo\n+4\tbar\n");
    }

    #[test]
    fn replace_all_over_many_matches_is_one_pass_and_its_diff_is_short() {
        let text = "let foo = 1;\n".repeat(100_000);
        let edits = [Edit { old: "foo".into(), new: "bar".into(), all: true }];
        let started = std::time::Instant::now();
        let applied = apply(&text, &edits, "f").unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "took {:?}", started.elapsed());
        assert_eq!(applied.text, text.replace("foo", "bar"));
        assert_eq!((applied.count, applied.added, applied.removed), (100_000, 100_000, 100_000));
        assert!(applied.diff.ends_with("⋮\n99900 more changes not shown\n"));
        assert!(applied.diff.len() < 20_000, "{} bytes", applied.diff.len());
    }

    #[test]
    fn overlapping_edits_are_rejected() {
        let err = run("abcdef", json!({"edits": [
            {"old_string": "abcd", "new_string": "1"},
            {"old_string": "cdef", "new_string": "2"}
        ]}));
        assert!(err.unwrap_err().contains("overlap"));
    }

    #[test]
    fn fuzzy_match_ignores_trailing_space_and_smart_quotes() {
        let text = "let s = \u{201C}hi\u{201D};   \nnext\n";
        let out = run(text, json!({"old_string": "let s = \"hi\";\nnext", "new_string": "let s = \"yo\";\nnext"}));
        assert_eq!(out.unwrap(), "let s = \"yo\";\nnext\n");
    }

    #[test]
    fn edits_sent_as_a_json_string_are_accepted() {
        let out = run("one", json!({"edits": "[{\"old_string\":\"one\",\"new_string\":\"two\"}]"}));
        assert_eq!(out.unwrap(), "two");
    }
}
