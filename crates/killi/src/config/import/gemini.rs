use super::*;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Result, bail};

/// Copies the provider, model and MCP servers from Gemini CLI's config
/// (`~/.muse/settings.json`, keys and endpoint from the environment or `~/.muse/.env`).
/// killi has no Gemini-native API, so the import needs Gemini's OpenAI-compatible endpoint
/// override (`GOOGLE_GEMINI_BASE_URL`) and bails without one; a Google login has no key to
/// copy either.
pub fn import_gemini(force: bool) -> Result<()> {
    import_gemini_from(&home().join(".gemini"), &killi_home(), force)
}

pub(crate) fn import_gemini_from(dir: &Path, dst_dir: &Path, force: bool) -> Result<()> {
    let Some(settings) = read_json(&dir.join("settings.json"))? else {
        bail!("no settings.json in {} to import", dir.display())
    };
    let file_env = parse_dotenv(&dir.join(".env"));

    let base_url = live_env("GOOGLE_GEMINI_BASE_URL")
        .or_else(|| file_env.get("GOOGLE_GEMINI_BASE_URL").cloned());
    let Some(base_url) = base_url else {
        bail!("gemini has no OpenAI-compatible endpoint (GOOGLE_GEMINI_BASE_URL) to import")
    };
    let Some((key_field, api_key)) = gemini_key(&file_env) else {
        bail!("gemini has no API key to import (it signs in with Google)")
    };

    let mut providers = toml::Table::new();
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "openai".into());
    provider.insert("base_url".into(), base_url.into());
    provider.insert(key_field.into(), api_key.into());
    providers.insert("gemini".to_string(), toml::Value::Table(provider));

    let mut out = toml::Table::new();
    let mut models = toml::Table::new();
    let picked = settings
        .get("model")
        .and_then(|model| match model {
            serde_json::Value::String(id) => (!id.is_empty()).then(|| id.clone()),
            _ => json_str(model, "name"),
        })
        .or_else(|| live_env("GEMINI_MODEL"))
        .or_else(|| file_env.get("GEMINI_MODEL").cloned());
    match picked {
        Some(id) => {
            let mut table = toml::Table::new();
            table.insert("provider".into(), "gemini".into());
            table.insert("model".into(), id.clone().into());
            let alias = format!("gemini/{id}");
            models.insert(alias.clone(), toml::Value::Table(table));
            out.insert("default_model".into(), alias.into());
        }
        None => eprintln!("gemini's config names no model, so none is the default"),
    }
    let model_count = models.len();
    out.insert("providers".into(), toml::Value::Table(providers));
    out.insert("models".into(), toml::Value::Table(models));

    write_import(out, None, dst_dir, force)?;
    if let Some(servers) = settings
        .get("mcpServers")
        .and_then(serde_json::Value::as_object)
    {
        write_mcp_json(servers, dst_dir, force)?;
    }
    println!(
        "Imported 1 provider and {model_count} model{} into {}",
        if model_count == 1 { "" } else { "s" },
        dst_dir.join("config.toml").display()
    );
    Ok(())
}

/// Gemini's key as killi keeps it: a live variable stays a reference, one only `.env`
/// sets is copied inline. `GEMINI_API_KEY` first, then the Vertex express one.
fn gemini_key(file_env: &BTreeMap<String, String>) -> Option<(&'static str, String)> {
    for name in ["GEMINI_API_KEY", "GOOGLE_API_KEY"] {
        if live_env(name).is_some() {
            return Some(("api_key_env", name.to_string()));
        }
    }
    ["GEMINI_API_KEY", "GOOGLE_API_KEY"]
        .into_iter()
        .filter_map(|name| file_env.get(name).map(|key| ("api_key", key.clone())))
        .next()
}

/// The `KEY=value` lines of a `.env` file: blanks and comments skipped, an `export`
/// prefix and matching quotes dropped. A missing file reads as empty.
fn parse_dotenv(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, mut value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        value = value.trim();
        for quote in ['"', '\''] {
            if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
                value = &value[1..value.len() - 1];
                break;
            }
        }
        out.insert(name.to_string(), value.to_string());
    }
    out
}
