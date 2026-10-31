use super::*;
use std::fs;
use std::ops::Range;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// The `[name]` table headers of a TOML text in file order, each with the range of lines
/// (`str::lines` indices) the table covers: from its own header line to the next header's,
/// or the end of the text. A header may carry spaces, quotes and a trailing comment
/// (`[ providers.x ]  # note`), and lines inside multi-line strings — the `[prompt]` block's
/// `system`, for one — are never headers.
fn table_spans(text: &str) -> Vec<(String, Range<usize>)> {
    let mut spans = Vec::new();
    let mut open: Option<&'static str> = None;
    for (n, line) in text.lines().enumerate() {
        let mut rest = line;
        if let Some(delim) = open {
            let Some(at) = rest.find(delim) else { continue };
            rest = &rest[at + delim.len()..];
        } else if let Some(name) = header_name(line) {
            spans.push((name, n..n));
            continue;
        }
        open = open_multi(rest);
    }
    let mut end = text.lines().count();
    for (_, span) in spans.iter_mut().rev() {
        span.end = end;
        end = span.start;
    }
    spans
}

/// The name a table header line carries, or `None` when the line is not one: it must open
/// with `[` (a `[[x]]` array's inner name reads the same) and close at the first `]` outside
/// quoted key parts, with nothing after it but spaces and a comment.
fn header_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let bytes = trimmed.as_bytes();
    if bytes.first() != Some(&b'[') {
        return None;
    }
    let array = bytes.get(1) == Some(&b'[');
    let mut quote = None;
    let mut close = None;
    for (i, &c) in bytes.iter().enumerate().skip(usize::from(array) + 1) {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b']' => {
                close = Some(i);
                break;
            }
            _ => {}
        }
    }
    let close = close?;
    let mut after = trimmed[close + 1..].trim_start();
    if array {
        after = after.strip_prefix(']')?.trim_start();
    }
    if !after.is_empty() && !after.starts_with('#') {
        return None;
    }
    let name = trimmed[usize::from(array) + 1..close].trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// The multi-line string delimiter (`'''` or `"""`) a line leaves open, if any: comments
/// and single-line strings are stepped over, and a string that closes on the same line
/// opens nothing.
fn open_multi(line: &str) -> Option<&'static str> {
    let bytes = line.as_bytes();
    let mut single = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(quote) = single {
            if quote == b'"' && c == b'\\' {
                i += 1;
            } else if c == quote {
                single = None;
            }
        } else {
            match c {
                b'#' => return None,
                b'"' | b'\'' => {
                    if bytes.get(i + 1) == Some(&c) && bytes.get(i + 2) == Some(&c) {
                        let delim: &'static str = if c == b'"' { "\"\"\"" } else { "'''" };
                        match line[i + 3..].find(delim) {
                            Some(at) => i += at + 3,
                            None => return Some(delim),
                        }
                    } else {
                        single = Some(c);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// The table a line belongs to, among `table_spans`' result: the last header at or above
/// the line, or `None` before the first header.
fn table_of(spans: &[(String, Range<usize>)], line: usize) -> Option<&str> {
    spans
        .iter()
        .take_while(|(_, span)| span.start <= line)
        .last()
        .map(|(name, _)| name.as_str())
}

/// The fields that hold a provider's key. Only one of them is read, so writing either one
/// replaces both.
pub(super) const KEY_FIELDS: [&str; 2] = ["api_key", "api_key_env"];

/// Stores `key` as the `api_key` of `provider` in the config file, in place of the
/// `api_key` or `api_key_env` line in its section, so `resolve` reads the key from the file.
/// The rest of the file, comments included, is left as it is.
pub(super) fn set_provider_key(path: &Path, provider: &str, key: &str) -> Result<()> {
    let text =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let fields = toml::Table::from_iter([("api_key".to_string(), toml::Value::from(key))]);
    let (new, _) = with_provider_fields(&text, provider, &fields)
        .with_context(|| format!("could not store the key in {}", path.display()))?;
    crate::fs_atomic::write(path, new.as_bytes(), Some(0o600))
        .with_context(|| format!("could not write {}", path.display()))
}

/// The config text with `fields` set in `[providers.<provider>]`, and the names of the fields
/// that changed. Each changed field goes on one line under the table's header, replacing
/// the line, dotted keys or `[providers.<provider>.<field>]` sub-table that set it before.
/// A table field, such as `tool_names`, keeps the entries it had besides the ones given.
/// When every field already holds its value the text comes back as it was. The rest of the
/// text, comments included, is left as it is.
pub(super) fn with_provider_fields(
    text: &str,
    provider: &str,
    fields: &toml::Table,
) -> Result<(String, Vec<String>)> {
    let parsed: toml::Table = toml::from_str(text).context("invalid config")?;
    let current = parsed
        .get("providers")
        .and_then(|providers| providers.get(provider))
        .and_then(toml::Value::as_table);
    let has = |field: &str| current.is_some_and(|table| table.contains_key(field));
    let mut changed: Vec<(String, toml::Value)> = Vec::new();
    for (field, value) in fields {
        let old = current.and_then(|table| table.get(field));
        let value = match (value, old) {
            (toml::Value::Table(given), Some(toml::Value::Table(had))) => {
                let mut merged = had.clone();
                merged.extend(given.clone());
                toml::Value::Table(merged)
            }
            _ => value.clone(),
        };
        let key_twin = KEY_FIELDS.contains(&field.as_str())
            && KEY_FIELDS
                .iter()
                .any(|other| *other != field.as_str() && has(other));
        if old != Some(&value) || key_twin {
            changed.push((field.clone(), value));
        }
    }
    if changed.is_empty() {
        return Ok((text.to_string(), Vec::new()));
    }
    // The changed field a line's key belongs to, as an index into `changed`.
    let replaced_by = |key: &str| {
        changed.iter().position(|(name, _)| {
            name == key || (KEY_FIELDS.contains(&name.as_str()) && KEY_FIELDS.contains(&key))
        })
    };
    let table = format!("providers.{provider}");
    let spans = table_spans(text);
    let Some(header) = spans
        .iter()
        .find(|(name, _)| *name == table)
        .map(|(_, span)| span.start)
    else {
        bail!("no [{table}] table to write to")
    };
    let mut out: Vec<String> = Vec::new();
    let mut written = vec![false; changed.len()];
    let mut under_header = 0;
    for (n, line) in text.lines().enumerate() {
        let in_table = table_of(&spans, n);
        if in_table == Some(table.as_str()) {
            if let Some(at) = line_key(line).and_then(replaced_by) {
                if !written[at] {
                    let (field, value) = &changed[at];
                    out.push(format!("{field} = {value}"));
                    written[at] = true;
                }
                continue;
            }
        } else if let Some(field) = in_table
            .and_then(|name| name.strip_prefix(&table))
            .and_then(|rest| rest.strip_prefix('.'))
            .and_then(|rest| rest.split('.').next())
        {
            let trimmed = line.trim();
            if replaced_by(field).is_some() && !trimmed.is_empty() && !trimmed.starts_with('#') {
                continue;
            }
        }
        out.push(line.to_string());
        if n == header {
            under_header = out.len();
        }
    }
    let unwritten = changed
        .iter()
        .zip(&written)
        .filter(|(_, written)| !**written)
        .map(|((field, value), _)| format!("{field} = {value}"));
    out.splice(under_header..under_header, unwritten);
    let new = format!("{}\n", out.join("\n"));
    toml::from_str::<Config>(&new).context("writing the provider would break the config")?;
    Ok((new, changed.into_iter().map(|(field, _)| field).collect()))
}

/// The first part of the key a `key = value` line sets, unquoted: `tool_names` for
/// `tool_names.bash = "run_command"`. `None` for blank lines, comments and headers.
fn line_key(line: &str) -> Option<&str> {
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
        return None;
    }
    let (key, _) = line.split_once('=')?;
    let first = key.split('.').next()?.trim();
    Some(first.trim_matches(|c| c == '"' || c == '\''))
}

/// Takes the `api_key` and `api_key_env` lines out of `provider`'s table, leaving the rest of
/// the file as it is. False when it had neither.
pub fn remove_provider_key(path: &Path, provider: &str) -> Result<bool> {
    let text =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let table = format!("providers.{provider}");
    let spans = table_spans(&text);
    let mut out: Vec<&str> = Vec::new();
    let mut removed = false;
    for (n, line) in text.lines().enumerate() {
        if table_of(&spans, n) == Some(table.as_str()) && line.trim().starts_with("api_key") {
            removed = true;
            continue;
        }
        out.push(line);
    }
    if removed {
        let new = format!("{}\n", out.join("\n"));
        toml::from_str::<Config>(&new).context("removing the key would break the config")?;
        crate::fs_atomic::write(path, new.as_bytes(), None)
            .with_context(|| format!("could not write {}", path.display()))?;
    }
    Ok(removed)
}

/// Makes `alias` the `default_model` of the config file: the line is replaced where the file
/// has one before its first table, and added at the top where it has none.
pub(super) fn set_default_model(path: &Path, alias: &str) -> Result<()> {
    let text =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let line = format!("default_model = {}", toml::Value::String(alias.to_string()));
    let spans = table_spans(&text);
    let mut out: Vec<String> = Vec::new();
    let mut written = false;
    for (n, text_line) in text.lines().enumerate() {
        let before_tables = spans.first().is_none_or(|(_, span)| n < span.start);
        if before_tables && !written && text_line.trim().starts_with("default_model") {
            out.push(line.clone());
            written = true;
            continue;
        }
        out.push(text_line.to_string());
    }
    if !written {
        out.insert(0, line);
    }
    let new = format!("{}\n", out.join("\n"));
    toml::from_str::<Config>(&new).context("setting the default model would break the config")?;
    crate::fs_atomic::write(path, new.as_bytes(), None)
        .with_context(|| format!("could not write {}", path.display()))
}

/// Writes or removes the config's `backup_model` — the fallback every session starts with —
/// the way `set_default_model` writes its line: replaced where the file has one before its
/// first table, and added at the top where it has none. `None` takes the line out, so no
/// session starts on a backup. The rest of the file, comments included, is left as it is.
pub fn set_backup_model(path: &Path, alias: Option<&str>) -> Result<()> {
    let text =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let line =
        alias.map(|alias| format!("backup_model = {}", toml::Value::String(alias.to_string())));
    let spans = table_spans(&text);
    let mut out: Vec<String> = Vec::new();
    let mut written = false;
    for (n, text_line) in text.lines().enumerate() {
        let before_tables = spans.first().is_none_or(|(_, span)| n < span.start);
        if before_tables && text_line.trim().starts_with("backup_model") {
            if !written && let Some(line) = &line {
                out.push(line.clone());
                written = true;
            }
            continue;
        }
        out.push(text_line.to_string());
    }
    if !written && let Some(line) = line {
        out.insert(0, line);
    }
    let new = format!("{}\n", out.join("\n"));
    toml::from_str::<Config>(&new).context("setting the backup model would break the config")?;
    crate::fs_atomic::write(path, new.as_bytes(), None)
        .with_context(|| format!("could not write {}", path.display()))
}

/// Stores `effort` as `alias`'s `default_effort` in the config file, in place of the
/// `default_effort` line in its `[models]` table, or right under the table's header, so the
/// next session on the model starts on it. The rest of the file, comments included, is left
/// as it is.
pub fn set_model_effort(path: &Path, alias: &str, effort: &str) -> Result<()> {
    let text =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let line = format!(
        "default_effort = {}",
        toml::Value::String(effort.to_string())
    );
    let spans = table_spans(&text);
    let mut out: Vec<String> = Vec::new();
    let mut written = false;
    for (n, text_line) in text.lines().enumerate() {
        let in_table = table_of(&spans, n).is_some_and(|name| is_model_table(name, alias));
        if in_table && text_line.trim().starts_with("default_effort") {
            if !written {
                out.push(line.clone());
                written = true;
            }
            continue;
        }
        out.push(text_line.to_string());
    }
    if !written {
        let Some((_, span)) = spans.iter().find(|(name, _)| is_model_table(name, alias)) else {
            bail!(
                "no [models.\"{alias}\"] table in {} to set the effort in",
                path.display()
            )
        };
        out.insert(span.start + 1, line);
    }
    let new = format!("{}\n", out.join("\n"));
    toml::from_str::<Config>(&new).context("setting the effort would break the config")?;
    crate::fs_atomic::write(path, new.as_bytes(), None)
        .with_context(|| format!("could not write {}", path.display()))
}

/// Whether `name`, as `table_spans` read it from a header, is `alias`'s `[models]` table:
/// the key may be quoted with either of TOML's quote characters, or bare.
fn is_model_table(name: &str, alias: &str) -> bool {
    name.strip_prefix("models.").is_some_and(|key| {
        key.strip_prefix('"')
            .and_then(|k| k.strip_suffix('"'))
            .or_else(|| key.strip_prefix('\'').and_then(|k| k.strip_suffix('\'')))
            .unwrap_or(key)
            == alias
    })
}

/// Writes the `[search]` table `/web-search-config` picked: its `provider`, `url`, `api_key`
/// and `api_key_env` lines are replaced in place, or the table is added at the end. A key of
/// `None` drops the key lines, so one set for another provider is never reused. The rest of
/// the file, comments included, is left as it is.
pub fn set_search(path: &Path, provider: &str, url: Option<&str>, key: Option<&str>) -> Result<()> {
    let text =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let value = |v: &str| toml::Value::String(v.to_string()).to_string();
    let mut fresh = vec![format!("provider = {}", value(provider))];
    if let Some(url) = url.filter(|u| !u.is_empty()) {
        fresh.push(format!("url = {}", value(url)));
    }
    if let Some(key) = key.filter(|k| !k.is_empty()) {
        fresh.push(format!("api_key = {}", value(key)));
    }
    let spans = table_spans(&text);
    let first = spans
        .iter()
        .find(|(name, _)| name == "search")
        .map(|(_, span)| span.start);
    let mut out: Vec<String> = Vec::new();
    let mut header = None;
    for (n, line) in text.lines().enumerate() {
        if Some(n) == first {
            header = Some(out.len());
        }
        if table_of(&spans, n) == Some("search")
            && ["provider", "url", "api_key", "api_key_env"]
                .iter()
                .any(|key| line.trim().starts_with(key))
        {
            continue;
        }
        out.push(line.to_string());
    }
    let at = match header {
        Some(at) => at + 1,
        None => {
            out.push(String::new());
            out.push("[search]".to_string());
            out.len()
        }
    };
    for (n, line) in fresh.into_iter().enumerate() {
        out.insert(at + n, line);
    }
    let new = format!("{}\n", out.join("\n"));
    toml::from_str::<Config>(&new).context("setting [search] would break the config")?;
    let mode = key.is_some_and(|key| !key.is_empty()).then_some(0o600);
    crate::fs_atomic::write(path, new.as_bytes(), mode)
        .with_context(|| format!("could not write {}", path.display()))
}
