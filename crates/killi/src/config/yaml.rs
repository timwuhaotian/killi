use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Reads a YAML config, as omp writes it, into the shape `read_json` returns. Only the
/// block style omp's own writer emits — mappings and sequences by indentation, plain,
/// quoted and single-line flow scalars — is understood; aliases, block scalars and other
/// spelled-out YAML are reported rather than guessed at, so an unimportable file fails the
/// import instead of importing wrong. A `.yaml` sibling stands in for a missing `.yml`.
pub(super) fn read_yaml(path: &Path) -> Result<Option<serde_json::Value>> {
    let present = if path.exists() { path.to_path_buf() } else { path.with_extension("yaml") };
    let text = match fs::read_to_string(&present) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("could not read {}", present.display())),
    };
    let mut lines: Vec<(usize, usize, &str)> = Vec::new();
    for (at, raw) in text.lines().enumerate() {
        let line = strip_yaml_comment(raw.trim_end());
        if line.trim().is_empty() {
            continue;
        }
        if line.trim() == "---" {
            if lines.is_empty() {
                continue;
            }
            bail!("YAML in {} has more than one document (line {})", present.display(), at + 1);
        }
        let (Some(spaces), None) = (line.bytes().position(|b| b != b' '), line.find('\t')) else {
            bail!("YAML in {} indents with tabs (line {})", present.display(), at + 1);
        };
        lines.push((spaces, at + 1, line[spaces..].trim_end()));
    }
    if lines.is_empty() {
        return Ok(Some(serde_json::Value::Null));
    }
    let at = lines[0].1;
    let (value, used) = yaml_block(&lines, 0, lines[0].0).map_err(|reason| anyhow::anyhow!("{reason} in {}", present.display()))?;
    if used != lines.len() {
        bail!("could not read the YAML in {} past line {at}", present.display());
    }
    Ok(Some(value))
}

/// The part of a YAML line before any comment: a `#` starts one only between words, and a
/// quoted span keeps its own.
fn strip_yaml_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote = 0u8;
    let mut i = 0;
    while i < bytes.len() {
        match (quote, bytes[i]) {
            (b'"', b'\\') => i += 1,
            (b'"', b'"') | (b'\'', b'\'') => quote = 0,
            (0, b'"' | b'\'') => quote = bytes[i],
            (0, b'#') if i == 0 || bytes[i - 1] == b' ' => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

/// One block node at `indent` from `lines[start..]`: its value and the index after it.
fn yaml_block(lines: &[(usize, usize, &str)], start: usize, indent: usize) -> std::result::Result<(serde_json::Value, usize), String> {
    if lines[start].2.starts_with("- ") || lines[start].2 == "-" {
        return yaml_sequence(lines, start, indent);
    }
    if yaml_key(lines[start].2).is_some() {
        return yaml_mapping(lines, start, indent);
    }
    Ok((yaml_scalar(lines[start].2)?, start + 1))
}

/// A block mapping whose keys sit at `indent`: each `key:` takes the value behind it, or
/// the deeper block under it.
fn yaml_mapping(lines: &[(usize, usize, &str)], start: usize, indent: usize) -> std::result::Result<(serde_json::Value, usize), String> {
    let mut map = serde_json::Map::new();
    let mut i = start;
    while i < lines.len() && lines[i].0 == indent {
        let (key, rest) = yaml_key(lines[i].2).ok_or_else(|| format!("expected a `key:` at line {}", lines[i].1))?;
        i += 1;
        map.insert(key, if rest.is_empty() { yaml_nested(lines, &mut i, indent)? } else { yaml_scalar(rest)? });
    }
    Ok((serde_json::Value::Object(map), i))
}

/// A `key:` with nothing behind it: the block under it, a sequence sitting at the key's own
/// indent (the one case YAML indents no deeper), or nothing.
fn yaml_nested(lines: &[(usize, usize, &str)], i: &mut usize, indent: usize) -> std::result::Result<serde_json::Value, String> {
    if *i >= lines.len() {
        return Ok(serde_json::Value::Null);
    }
    let (next, _, text) = lines[*i];
    if next > indent {
        let (value, used) = yaml_block(lines, *i, next)?;
        *i = used;
        Ok(value)
    } else if next == indent && (text.starts_with("- ") || text == "-") {
        let (value, used) = yaml_sequence(lines, *i, indent)?;
        *i = used;
        Ok(value)
    } else {
        Ok(serde_json::Value::Null)
    }
}

/// A block sequence whose dashes sit at `indent`. An item that opens a mapping — `- id: x`
/// with the rest aligned after the dash — stands as its own block, so its lines re-indent
/// to that column before the mapping reads them.
fn yaml_sequence(lines: &[(usize, usize, &str)], start: usize, indent: usize) -> std::result::Result<(serde_json::Value, usize), String> {
    let mut items = Vec::new();
    let mut i = start;
    while i < lines.len() && lines[i].0 == indent && (lines[i].2.starts_with("- ") || lines[i].2 == "-") {
        let (_, at, text) = lines[i];
        let inner = indent + 2;
        i += 1;
        let head = text.strip_prefix('-').unwrap_or_default().trim_start();
        if head.is_empty() {
            items.push(yaml_nested(lines, &mut i, indent)?);
        } else if yaml_key(head).is_some() {
            let mut block = vec![(inner, at, head)];
            while i < lines.len() && lines[i].0 >= inner {
                block.push(lines[i]);
                i += 1;
            }
            let (value, _) = yaml_mapping(&block, 0, inner)?;
            items.push(value);
        } else {
            items.push(yaml_scalar(head)?);
        }
    }
    Ok((serde_json::Value::Array(items), i))
}

/// A line's `key:` split, when it opens a mapping entry: the plain key (a `/` in a model id
/// stays, a `:` only ends the key before a space or the line's end) and what follows it.
fn yaml_key(line: &str) -> Option<(String, &str)> {
    if line.starts_with(['[', '{', '"', '\'']) {
        return None;
    }
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' && (i + 1 == bytes.len() || bytes[i + 1] == b' ') {
            let key = &line[..i];
            if key.is_empty() || key.contains([' ', ',', '[', ']', '{', '}']) {
                return None;
            }
            return Some((key.to_string(), line[i + 1..].trim_start()));
        }
        i += 1;
    }
    None
}

/// A scalar as omp's writer spells it: a quoted string, a one-line flow collection, or a
/// plain value read as null, bool, number or text. An anchor's name drops — omp's
/// `!command` keys stay text, for the importer to decline — and an alias or block scalar is
/// declined, not guessed.
fn yaml_scalar(text: &str) -> std::result::Result<serde_json::Value, String> {
    let text = text.trim();
    let text = match text.strip_prefix('&') {
        Some(after) => after.split_once(' ').map_or("", |(_, value)| value).trim_start(),
        None => text,
    };
    match text.chars().next() {
        Some(c @ ('*' | '|' | '>')) => Err(format!("unsupported YAML scalar starting `{c}`")),
        Some('"') => serde_json::from_str::<serde_json::Value>(text).map_err(|_| format!("bad quoted string `{text}`")),
        Some('\'') if text.len() >= 2 && text.ends_with('\'') => Ok(text[1..text.len() - 1].replace("''", "'").into()),
        Some('[') if text.ends_with(']') => {
            let inner = text[1..text.len() - 1].trim();
            if inner.is_empty() {
                Ok(serde_json::Value::Array(Vec::new()))
            } else {
                inner.split(',').map(yaml_scalar).collect::<std::result::Result<Vec<_>, _>>().map(serde_json::Value::Array)
            }
        }
        Some('{') if text.ends_with('}') => {
            let inner = text[1..text.len() - 1].trim();
            let mut map = serde_json::Map::new();
            if !inner.is_empty() {
                for entry in inner.split(',') {
                    let (key, value) = entry.split_once(':').ok_or_else(|| format!("bad flow mapping `{text}`"))?;
                    map.insert(match yaml_scalar(key)? {
                        serde_json::Value::String(key) => key,
                        other => other.to_string(),
                    }, yaml_scalar(value)?);
                }
            }
            Ok(serde_json::Value::Object(map))
        }
        _ => Ok(match text {
            "null" | "Null" | "NULL" | "~" | "" => serde_json::Value::Null,
            "true" | "True" | "TRUE" => true.into(),
            "false" | "False" | "FALSE" => false.into(),
            _ if text.parse::<i64>().is_ok() => text.parse::<i64>().unwrap_or_default().into(),
            _ if text.contains(['.', 'e', 'E']) && !text.starts_with(['+', '.']) && text.parse::<f64>().is_ok() => {
                text.parse::<f64>().unwrap_or_default().into()
            }
            _ => text.to_string().into(),
        }),
    }
}
