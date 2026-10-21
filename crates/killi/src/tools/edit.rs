use std::fs;

use serde_json::Value;

use super::files::TextFile;
use super::{Context, Output, is_binary, resolve};

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
        Ok((text, count)) => match fs::write(&full, file.encode(&text)) {
            Ok(()) => Output::ok(format!("Applied {count} replacement(s) to {path}.")),
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

/// Matches every edit against the original text: exactly first, then ignoring trailing
/// whitespace, smart quotes, Unicode dashes and special spaces.
fn apply(text: &str, edits: &[Edit], path: &str) -> Result<(String, usize), String> {
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
                return Err(format!(
                    "Could not find {} in {path}. It must match the file exactly, including indentation and line breaks; read the file again if unsure.",
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
    let mut out = text.to_string();
    for &(start, end, i) in ranges.iter().rev() {
        out.replace_range(start..end, &edits[i].new.replace("\r\n", "\n"));
    }
    if out == text {
        return Err(format!("No changes made to {path}: the replacement produced identical content."));
    }
    Ok((out, ranges.len()))
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
        apply(text, &parse_edits(&args)?, "f").map(|(out, _)| out)
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
