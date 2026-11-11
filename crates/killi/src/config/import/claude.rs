use super::*;
use std::path::Path;

use anyhow::{Result, bail};

/// Copies the provider, model and MCP servers from Claude Code's config
/// (`~/.claude/settings.json`, servers from `~/.claude.json`). Claude Code keeps no keys
/// in files, so the import reads the live `$ANTHROPIC_API_KEY`, or the key the settings'
/// own `env` block sets, and bails on a subscription login, as the other importers skip
/// theirs. The base URL comes the same way when a proxy is configured.
pub fn import_claude(force: bool) -> Result<()> {
    import_claude_from(
        &home().join(".claude"),
        &home().join(".claude.json"),
        &killi_home(),
        force,
    )
}

pub(crate) fn import_claude_from(
    dir: &Path,
    state_path: &Path,
    dst_dir: &Path,
    force: bool,
) -> Result<()> {
    let Some(settings) = read_json(&dir.join("settings.json"))? else {
        bail!("no settings.json in {} to import", dir.display())
    };
    let env = settings.get("env").cloned().unwrap_or_default();

    let base_url = live_env("ANTHROPIC_BASE_URL").or_else(|| json_str(&env, "ANTHROPIC_BASE_URL"));
    let Some((key_field, api_key)) = claude_key(&env) else {
        bail!("claude code has no API key to import (it signs in with a subscription)")
    };

    let mut providers = toml::Table::new();
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "anthropic".into());
    if let Some(url) = base_url {
        provider.insert("base_url".into(), url.into());
    }
    provider.insert(key_field.into(), api_key.into());
    providers.insert("anthropic".to_string(), toml::Value::Table(provider));

    let mut out = toml::Table::new();
    let mut models = toml::Table::new();
    let picked = live_env("ANTHROPIC_MODEL").or_else(|| json_str(&settings, "model"));
    match picked {
        Some(id) => {
            let mut table = toml::Table::new();
            table.insert("provider".into(), "anthropic".into());
            table.insert("model".into(), id.clone().into());
            let alias = format!("anthropic/{id}");
            models.insert(alias.clone(), toml::Value::Table(table));
            out.insert("default_model".into(), alias.into());
            if matches!(id.as_str(), "opus" | "sonnet" | "haiku") {
                eprintln!(
                    "claude's model `{id}` is an alias; set a full model id if the API refuses it"
                );
            }
        }
        None => eprintln!("claude's config names no model, so none is the default"),
    }
    let model_count = models.len();
    out.insert("providers".into(), toml::Value::Table(providers));
    out.insert("models".into(), toml::Value::Table(models));

    write_import(out, None, dst_dir, force)?;
    if let Some(state) = read_json(state_path)?
        && let Some(servers) = state
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

/// Claude Code's key as killi keeps it: the live `$ANTHROPIC_API_KEY` stays a variable
/// reference, while one the settings' own `env` block sets is copied inline. `None` on a
/// subscription login, which has no key to copy.
fn claude_key(env: &serde_json::Value) -> Option<(&'static str, String)> {
    if live_env("ANTHROPIC_API_KEY").is_some() {
        return Some(("api_key_env", "ANTHROPIC_API_KEY".to_string()));
    }
    json_str(env, "ANTHROPIC_API_KEY").map(|key| ("api_key", key))
}
