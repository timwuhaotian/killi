use super::*;
use std::path::Path;

use anyhow::{Result, bail};

/// Copies providers, models, the default model and MCP servers from opencode's config
/// (`~/.config/opencode/opencode.json`, with keys from `~/.local/share/opencode/auth.json`).
/// Providers opencode signs into with OAuth have no key to copy and are skipped.
pub fn import_opencode(force: bool) -> Result<()> {
    import_opencode_from(
        &home().join(".config/opencode/opencode.json"),
        &home().join(".local/share/opencode/auth.json"),
        &killi_home(),
        force,
    )
}

pub(crate) fn import_opencode_from(
    config_path: &Path,
    auth_path: &Path,
    dst_dir: &Path,
    force: bool,
) -> Result<()> {
    let Some(config) = read_json(config_path)? else {
        bail!("no opencode config at {} to import", config_path.display())
    };
    let auth = read_json(auth_path)?.unwrap_or_default();
    let Some(all) = config
        .get("provider")
        .and_then(serde_json::Value::as_object)
    else {
        bail!("no providers in {}", config_path.display())
    };

    let mut out = toml::Table::new();
    let mut providers = toml::Table::new();
    let mut models = toml::Table::new();
    let mut skipped = Vec::new();
    for (name, provider) in all {
        let Some(api_key) = opencode_key(provider, &auth, name) else {
            skipped.push(name.clone());
            continue;
        };
        let Some(listed) = provider
            .get("models")
            .and_then(serde_json::Value::as_object)
            .filter(|models| !models.is_empty())
        else {
            skipped.push(name.clone());
            continue;
        };
        let mut table = toml::Table::new();
        table.insert(
            "type".into(),
            opencode_protocol(json_str(provider, "npm").as_deref()).into(),
        );
        if let Some(url) = provider
            .get("options")
            .and_then(|options| json_str(options, "baseURL"))
        {
            table.insert("base_url".into(), url.into());
        }
        table.insert("api_key".into(), api_key.into());
        providers.insert(name.clone(), toml::Value::Table(table));

        for (id, model) in listed {
            let mut table = toml::Table::new();
            table.insert("provider".into(), name.clone().into());
            table.insert("model".into(), id.clone().into());
            let limits = model.get("limit");
            if let Some(context) = limits.and_then(|limits| json_u64(limits, "context")) {
                table.insert("max_context_size".into(), toml_int(context));
            }
            if let Some(output) = limits.and_then(|limits| json_u64(limits, "output")) {
                table.insert("max_output_size".into(), toml_int(output));
            }
            let mut capabilities = Vec::new();
            if model
                .get("reasoning")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                capabilities.push("thinking".to_string());
            }
            if model
                .get("attachment")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                capabilities.push("image_in".to_string());
            }
            if !capabilities.is_empty() {
                table.insert("capabilities".into(), capabilities.into());
            }
            models.insert(format!("{name}/{id}"), toml::Value::Table(table));
        }
    }
    if providers.is_empty() {
        bail!("opencode's config has no provider with an API key and models to import");
    }

    let configured = json_str(&config, "model");
    let default = configured
        .as_ref()
        .filter(|alias| models.contains_key(*alias))
        .or_else(|| models.keys().next())
        .cloned();
    if let Some(alias) = &default {
        out.insert("default_model".into(), alias.clone().into());
        if configured.as_deref() != Some(alias.as_str()) {
            eprintln!(
                "opencode's default model {} wasn't imported, so `{alias}` is the default",
                configured.as_deref().unwrap_or("(none)")
            );
        }
    }
    let (provider_count, model_count) = (providers.len(), models.len());
    out.insert("providers".into(), toml::Value::Table(providers));
    out.insert("models".into(), toml::Value::Table(models));

    write_import(out, None, dst_dir, force)?;
    copy_opencode_mcp(&config, dst_dir, force)?;
    println!(
        "Imported {provider_count} providers and {model_count} models into {}",
        dst_dir.join("config.toml").display()
    );
    if !skipped.is_empty() {
        println!(
            "Skipped providers without an API key (opencode OAuth, or no models): {}",
            skipped.join(", ")
        );
    }
    Ok(())
}

/// The key opencode keeps for a provider: the config's `options.apiKey`, else the API key
/// `/connect` stored in `auth.json`. An OAuth sign-in has no key to copy.
fn opencode_key(
    provider: &serde_json::Value,
    auth: &serde_json::Value,
    name: &str,
) -> Option<String> {
    if let Some(key) = provider
        .get("options")
        .and_then(|options| json_str(options, "apiKey"))
    {
        return Some(key);
    }
    let entry = auth.get(name)?;
    (entry.get("type").and_then(serde_json::Value::as_str) == Some("api"))
        .then(|| json_str(entry, "key"))?
}

/// The npm package opencode names for a provider, as killi's provider type.
fn opencode_protocol(npm: Option<&str>) -> &'static str {
    match npm {
        Some("@ai-sdk/anthropic") => "anthropic",
        Some("@ai-sdk/openai") => "openai",
        _ => "openai",
    }
}

/// Writes opencode's MCP servers in killi's `mcpServers` shape: a local server's command
/// array becomes the command and its args, a remote one its URL. Disabled ones are left out.
fn copy_opencode_mcp(config: &serde_json::Value, dst_dir: &Path, force: bool) -> Result<()> {
    let Some(all) = config
        .get("mcp")
        .and_then(serde_json::Value::as_object)
        .filter(|servers| !servers.is_empty())
    else {
        return Ok(());
    };
    let mut servers = serde_json::Map::new();
    for (name, server) in all {
        if server.get("enabled").and_then(serde_json::Value::as_bool) == Some(false) {
            continue;
        }
        let mut entry = serde_json::Map::new();
        let command = server.get("command").and_then(serde_json::Value::as_array);
        if let Some(command) = command.filter(|command| !command.is_empty()) {
            let args: Vec<&str> = command
                .iter()
                .skip(1)
                .filter_map(serde_json::Value::as_str)
                .collect();
            entry.insert("command".into(), command[0].clone());
            if !args.is_empty() {
                entry.insert("args".into(), args.into());
            }
        } else if let Some(url) = json_str(server, "url") {
            entry.insert("url".into(), url.into());
        }
        if !entry.is_empty() {
            servers.insert(name.clone(), entry.into());
        }
    }
    write_mcp_json(&servers, dst_dir, force)
}
